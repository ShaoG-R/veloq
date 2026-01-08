# Veloq Runtime 核心架构文档

本文档主要介绍 `veloq-runtime` 核心运行时层 (`src/runtime`) 的架构设计、核心组件及实现原理。

## 1. 概要 (Overview)

Veloq Runtime 是一个基于 **Thread-per-Core** 模型的高性能异步运行时。它旨在充分利用现代多核硬件的特性，通过减少跨核通信、锁竞争和缓存抖动来最大化 I/O 和计算吞吐量。与 Tokio 等通用运行时不同，Veloq 更侧重于显式的资源管理和基于网格 (Mesh) 的任务调度。

## 2. 理念和思路 (Philosophy and Design)

### 2.1 Thread-per-Core 与 Nothing-Shared
我们坚信在现代高并发场景下，**数据局部性 (Data Locality)** 是性能的关键。
- 也就是每个 Worker 线程拥有独立的 I/O 驱动 (Driver)、任务队列和缓冲区池。
- 尽量避免全局锁。线程间的交互主要通过显式的消息传递（Mesh Network）或高效的工作窃取（Work Stealing）进行。

### 2.2 Mesh Network (以通信代共享)
传统的运行时通常使用全局的 `Mutex<Queue>` 或 `SegQueue` 来进行跨线程任务调度，这在高负载下会产生严重的 CAS 争用。
Veloq 引入了 **Mesh** 概念：
- 每个 Worker 之间建立两两互联的 **SPSC (Single-Producer Single-Consumer)** 无锁通道。
- 当 Worker A 需要将任务发给 Worker B 时，它直接将任务推入 A->B 的 SPSC 通道，零锁竞争。
- 这种全互联拓扑形成了一个高效的任务分发网格。

### 2.3 显式上下文 (Explicit Context)
为了避免隐式的全局状态（如 TLS 中的隐藏变量），Veloq 提供了 `RuntimeContext`：
- 显式通过上下文访问 `Spawner` (用于负载均衡生成任务)、`Driver` (I/O) 和 `Mesh` (通信)。
- `spawn_balanced`: 使用 P2C 算法进行负载均衡的任务生成。
- `spawn`: 优先注入当前 Worker 的 Injector 队列。
- `bind_pool`: 强制要求缓冲区池与当前线程绑定，确保 I/O 内存操作的安全性。

## 3. 模块内结构 (Internal Structure)

代码位于 `src/runtime/`：

```
src/runtime/
├── runtime.rs    // Runtime 主入口，包含 Runtime 结构体、RuntimeBuilder 及 Worker 线程启动逻辑
├── context.rs    // 线程局部上下文 (TLS)，提供 spawn 接口和资源访问 (RuntimeContext)
├── task.rs       // 任务 (Task) 定义，手动实现的 RawWakerVTable
├── mesh.rs       // Mesh 网络通信原语 (SPSC Channel)
├── join.rs       // JoinHandle 实现，管理任务结果的异步等待
├── executor.rs   // LocalExecutor 定义，Worker 线程主循环
└── executor/     // 执行器内部实现细节
    └── spawner.rs // 任务生成器、注册表 (Registry) 和负载均衡逻辑
```

## 4. 代码详细分析 (Detailed Analysis)

### 4.1 Runtime & Initialization (`runtime.rs`)
`Runtime` 结构体持有所有 Worker 线程的句柄 (`JoinHandle`) 和全局注册表 (`ExecutorRegistry`)。
在 `RuntimeBuilder::build()` 过程中，会进行如下关键初始化：
- **MeshMatrix**: 创建一个扁平化的 SPSC 通道矩阵，负责所有 Worker 之间的两两互联。
- **Shared States**: 预分配所有 Worker 的共享状态 (`ExecutorShared`)，包含注入队列 (`Injector`) 和负载计数器。
- **Thread Spawning**: 启动 Worker 线程，每个线程运行一个 `LocalExecutor`，并绑定对应的 Mesh 通道和 Buffer Pool。

### 4.2 Context (`context.rs`)
`RuntimeContext` 是运行时与任务之间的桥梁。它包含：
- **Driver**: 指向底层 `PlatformDriver` 的弱引用。
- **ExecutorHandle**: 当前执行器的句柄，包含共享状态 (Shared State)。
- **Spawner**: 全局生成器，用于 `spawn_balanced`。
- **Mesh**: 访问 Mesh 网络的能力。

**Spawn 策略**:
- `spawn_balanced(future)`: 使用 P2C (Power of Two Choices) 算法选择两个负载最小的 Worker 之一，通过 Mesh 发送任务。
- `spawn(future)`: 将任务推入当前 Worker 的 Global Injector 队列。这比 Local Queue 慢一点（有原子操作），但允许任务被其他 Worker 窃取。

### 4.3 Task (`task.rs`)
Veloq 的 Task 是对 `Future` 的轻量级封装。
- **RawWakerVTable**: 手动实现了虚函数表，而不是使用 `Arc::new(Mutex::new(..))`。
- **内存布局**:
  `Rc<Task>` 包含 `RefCell<Option<Pin<Box<Future>>>>` 和指向执行器队列的 `Weak` 指针。
  使用 `Rc` 而非 `Arc` 是因为 Task 通常在本地队列中流转，且 Veloq 鼓励本地计算。跨线程调度时，Task 会被打包成 `Job` (Box<FnOnce>) 传输。

### 4.4 JoinHandle (`join.rs`)
实现了任务结果的异步获取。
- **无锁状态机**: 使用 `AtomicU8` 维护状态 (`IDLE` -> `WAITING` -> `READY`)。
- **Local vs Send**: 区分 `LocalJoinHandle` (单线程内) 和 `JoinHandle` (跨线程)，分别优化开销。

## 5. 存在的问题和 TODO (Issues and TODOs)

1.  **Blocking IO 支持不足**:
    目前缺乏一个专用的 `blocking_spawn` 线程池来处理重 CPU 或阻塞系统调用（非 I/O 类）。
    *TODO*: 引入专用的 Blocking Thread Pool。

2.  **Task Debugging**:
    目前的 Task 结构体非常精简，缺乏调试信息。
    *TODO*: 在 Debug 模式下注入追踪信息。

3.  **Local Task 饿死**:
    虽然有 Budget 机制，但在极端混合场景下（大量 Mesh 消息 + 本地任务），调度策略可能仍需微调。

## 6. 未来的方向 (Future Directions)

1.  **结构化并发 (Structured Concurrency)**:
    实现类似 `TaskScope` 的机制。

2.  **协作式抢占 (Cooperative Preemption)**:
    目前依赖用户代码中的 `.await` 点进行调度。如果用户写了死循环，Worker 会卡死。未来可考虑结合编译器插件或计时器信号进行强制让出检测。
