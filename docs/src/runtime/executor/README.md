# Veloq Executor 与调度系统

本文档详细阐述 `src/runtime/executor` 模块的内部工作机制。这是 Veloq 运行时的“引擎”，负责任务的调度、负载均衡和执行循环。

## 1. 概要 (Overview)

`Executor` 模块实现了 **Work-Stealing** 和 **P2C (Power of Two Choices)** 相结合的混合调度算法。
它由两部分组成：
1.  **LocalExecutor** (`executor.rs`): 运行在每个 Worker 线程上的主循环，负责驱动 I/O、处理队列和 Mesh 消息。
2.  **Runtime & Spawning** (`runtime.rs` / `spawner.rs`): 负责线程的生命周期管理和任务分发策略。

## 2. 理念和思路 (Philosophy and Design)

### 2.1 混合调度策略 (Hybrid Scheduling)
Veloq 结合了发送端和接收端的负载均衡：
- **发送端 (P2C)**: 当调用 `spawn_balanced` 时，使用“两个随机选择”算法，比较两个 Worker 的负载，将任务发往较空闲的那个。
- **本地优先 (Local Injection)**: 当调用 `spawn` 时，任务被放入当前 Worker 的 `Injector` 队列，优先由本地消费，但也允许被窃取。
- **接收端 (Work-Stealing)**: 当 Worker 空闲时，主动去“窃取”其他 Worker 的 `Injector` 队列中的任务。

### 2.2 优先级倒置 (Priority Inversion for Latency)
在 `LocalExecutor` 的主循环中，显式定义了轮询顺序：
1.  **Mesh Messages** (最高优先级): 处理来自其他 Worker 的任务/消息。这保证了跨核通信的低延迟。
2.  **Local Queue**: 处理 `spawn_local` 生成的、不可被窃取的本地任务。
3.  **Global Injector**: 处理 `spawn` 生成的、或者是被 steal 来的任务。
4.  **Work Stealing**: 最后尝试去偷任务。

### 2.3 动态注册 (Dynamic Registry)
`ExecutorRegistry` 支持动态扩缩容，并使用 `smr-swap` 实现了无锁的读取快照，确保调度器在遍历 Worker 列表时不会被锁阻塞。

## 3. 模块内结构 (Internal Structure)

- `runtime.rs`:
    - `Runtime`: 运行时顶层入口。
    - `RuntimeBuilder`: 负责初始化所有 Worker、MeshMatrix 和共享状态，并启动线程。

- `executor.rs`:
    - `LocalExecutor`: Worker 线程的本地执行器。
    - `block_on`: 创建一个临时的 `LocalExecutor` 在当前线程运行 Future。

- `context.rs`:
    - `RuntimeContext`: 提供给用户的 TLS 上下文，包含 `spawn`, `spawn_balanced`, `spawn_local` 等接口。

- `executor/spawner.rs`:
    - `Spawner`: 封装 P2C 逻辑。
    - `ExecutorRegistry`: 全局注册表。
    - `ExecutorShared`: 跨线程共享的原子状态（负载计数、注入队列）。

## 4. 代码详细分析 (Detailed Analysis)

### 4.1 主循环 (`LocalExecutor::run`)
核心是一个带有 **Budget** (预算) 的循环：
```rust
loop {
    let mut executed = 0;
    while executed < BUDGET {
        // 1. Mesh Polling (Highest Priority)
        if self.try_poll_mesh() { ... }
        
        // 2. Local Queue
        if let Some(task) = self.queue.pop() { ... }
        
        // 3. Global Injector
        if self.try_poll_injector() { ... }
        
        // 4. Stealing
        if self.try_steal(executed) { ... }
    }
    
    // 5. IO Wait & Park
    self.park_and_wait(&main_woken);
}
```
`BUDGET` 机制（默认 64）防止计算密集型任务饿死 I/O 事件的轮询。

### 4.2 停车与唤醒 (`park_and_wait`)
这是一个精细的状态机：
1.  **Set PARKING**: 标记 Mesh 状态为 PARKING。
2.  **Double Check**: 再次检查 Mesh 和队列。
3.  **Commit PARKED**: 状态设为 PARKED。
4.  **Driver.wait()**: 调用底层的 `epoll_wait` / `GetQueuedCompletionStatus`。

当其他线程通过 Mesh 发送任务时，如果发现目标处于 `PARKED`，会通过 Driver 的 Waker 唤醒它。

### 4.3 任务生成 (`context.rs` & `spawner.rs`)
- **`spawn_balanced`**: 从注册表中随机选两个 Worker，比较 `total_load` (Local + Injected)，选择负载较小的那个，通过 Mesh 发送 Job。
- **`spawn`**: 直接 push 到 `handle.shared.injector`，并原子增加 `injected_load`。

### 4.4 smr-swap 的使用
`ExecutorRegistry` 使用 `smr_swap::SmrSwap` 来存储 `Vec<ExecutorHandle>`，实现 wait-free 的读取。

## 5. 存在的问题和 TODO (Issues and TODOs)

1.  **负载倾斜后的 Ping-Pong**:
    P2C 可能导致任务抖动。
    *TODO*: 实现指数退避 (Exponential Backoff) 的 Stealing 策略。

2.  **NUMA 感知**:
    *TODO*: 实现分层 Stealing。

3.  **Worker ID 回收**:
    ID 目前单调递增。

## 6. 未来的方向 (Future Directions)

1.  **时间片轮转**: 防止单个任务霸占 Budget。
2.  **自适应 Budget**: 动态调整 Budget。
