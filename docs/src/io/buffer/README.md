# buffer 模块文档

本文档详细介绍了 `veloq-runtime` 中的 `io::buffer` 模块。该模块负责高性能异步 I/O 的内存管理，特别针对 io_uring 和 IOCP 的需求进行了优化，提供了一套能够保持地址稳定、支持类型擦除且对零拷贝友好的内存池抽象。

## 1. 概要 (Overview)

`src/io/buffer` 模块不仅仅是一个内存分配器，它是连接**用户态内存**与**内核 I/O** 的桥梁。其核心设计目标包括：

*   **地址稳定 (Address Stability)**: 异步 I/O 提交期间，缓冲区物理地址不可变。
*   **注册优化 (Registration Friendly)**: 为了支持 io_uring 的 `IORING_REGISTER_BUFFERS` 或 Windows RIO，底层内存必须易于提取并以大块形式注册。
*   **分层架构**: 将内存分配 (`BackingPool`) 与 注册逻辑 (`BufferRegistrar`) 解耦。
*   **类型擦除**: 通过 `AnyBufPool` 和手动 VTable，使得上层应用无需关心底层的具体分配策略（Buddy 还是 Hybrid）。

核心组件结构：
*   **`BackingPool` Trait**: 定义原始内存管理的接口（分配、释放、获取内存区域）。
*   **`BufPool` Trait**: 面向用户的顶层接口，提供 `alloc` 方法返回 `FixedBuf`。
*   **`RegisteredPool<P>`**: 组合器。将一个 `BackingPool` 和一个 `BufferRegistrar` 绑定，自动处理内存注册，并实现 `BufPool`。
*   **`BuddyPool`**: 基于伙伴系统 + Slab 缓存的通用分配器。
*   **`HybridPool`**: 基于统一 Arena 的多级 Slab 分配器，针对网络 I/O 优化。

## 2. 理念和思路 (Philosophy and Design)

### 2.1 内存稳定性与生命周期
在 Proactor 模式中，内核直接操作用户内存。`FixedBuf` 句柄拥有底层的内存块，并且不支持原地扩容。其生命周期通过 Rust 所有权系统管理，确保在 I/O 完成前内存有效。

### 2.2 Direct I/O 对齐
所有 Pool 实现均基于 `AlignedMemory`，强制执行 4KB (Page Size) 对齐。这确保了生成的缓冲区天然满足 O_DIRECT / FILE_FLAG_NO_BUFFERING 的严格要求（通常要求 512B 或 4KB 对齐）。

### 2.3 核心与注册分离
最新的设计将内存分配逻辑与 I/O 驱动的注册逻辑分离：
*   **BackingPool**: 只管内存怎么切分（Buddy 算法或 Slab 算法），不知道 io_uring/IOCP 的存在。
*   **BufferRegistrar**: 驱动层提供的接口，负责将 `BackingPool` 提供的内存区域列表 (`Vec<BufferRegion>`) 注册给内核，并返回 Token (index 或 ID)。
*   **RegisteredPool**: 胶水层，初始化时调用注册，分配时将 Token 注入到 `FixedBuf` 中。

## 3. 模块内结构 (Internal Structure)

```
src/io/buffer.rs           // 核心定义：BackingPool, BufPool, RegisteredPool, FixedBuf
src/io/buffer/
├── buddy.rs               // BuddyPool: 缓存层 + 原始伙伴系统
└── hybrid.rs              // HybridPool: 统一 Arena + 多级 Slab
```

## 4. 代码详细分析 (Detailed Analysis)

### 4.1 核心抽象 (`buffer.rs`)

**`FixedBuf`**:
用户持有的最终句柄。
```rust
pub struct FixedBuf {
    ptr: NonNull<u8>,
    cap: usize,
    global_index: u16,      // 注册后的 Buffer Index (io_uring use)
    pool_data: NonNull<()>, // 能够找到源 Pool 的指针
    vtable: &'static PoolVTable, // 虚函数表
    context: usize,         // 分配上下文 (如 slab index 或 buddy order)
    ...
}
```
`FixedBuf` 不依赖具体泛型，可以跨模块传递。`drop` 时通过 `vtable.dealloc` 归还内存。

**`RegisteredPool`**:
这是通常用户使用的具体类型。它持有 `pool: P` 和 `registrar`。
1.  `new()`: 调用 `pool.get_memory_regions()` 获取底层大块内存。
2.  调用 `registrar.register()` 获得 `registration_ids`。
3.  `alloc()`: 从 `pool` 分配内存，填入对应的 `global_index`，构造 `FixedBuf`。

### 4.2 BuddyPool (`buddy.rs`)
`BuddyPool` 采用了两层架构来平衡性能与碎片率：

1.  **L0 Layer: Slab Cache (`BuddyAllocator::slabs`)**:
    *   针对常用大小 (Order 0-5, 即 4KB-128KB) 维护了简单的栈式缓存 (`Vec<ptr>`)。
    *   **分配**: 优先从对应 Order 的缓存栈中弹出，O(1) 极速分配。
    *   **释放**: 优先推入缓存栈。只有当缓存满时，才回退到底层伙伴系统。

2.  **L1 Layer: Raw Buddy System (`RawBuddyAllocator`)**:
    *   经典的二进制伙伴系统，管理 32MB 的 `Arena`。
    *   支持动态分裂与合并 (Coalescing)。
    *   使用 Bitmap 加速空闲块查找。

这种设计有效缓解了纯 Buddy 系统在频繁分配释放时的“合并/分裂抖动”问题。

### 4.3 HybridPool (`hybrid.rs`)
`HybridPool` 专为固定大小的网络包设计，采用 **Unified Arena (统一竞技场)** 布局。

*   **统一内存 layout**:
    不同于传统的 "每个 Slab 独立申请内存"，`HybridPool` 预先计算所有规格 Slab (4K, 8K, 16K, 32K, 64K) 所需的总内存，并一次性申请一块连续的 `AlignedMemory`。
    *   `Arena Start` -> `[ Slab 4K ] [ Slab 8K ] ... [ Slab 64K ]` -> `Arena End`
    *   **优势**: `get_memory_regions()` 只返回**一个**大的 Region。这对 io_uring 注册非常友好（即使是 IORING_REGISTER_BUFFERS 也通常希望区域数量少且连续）。

*   **分配策略**:
    *   **Fixed Size**: 根据大小直接映射到对应的 Slab (O(1))。
    *   **Fallback**: 超过 64KB 的请求通过 Global Allocator 分配（此时 `global_index` 无效，无法享受零拷贝注册）。

## 5. 存在的问题和 TODO (Issues and TODOs)

1.  **静态大小限制**:
    *   `BuddyPool` (32MB) 和 `HybridPool` (14MB) 的 Arena 大小目前是硬编码的。
    *   **TODO**: 支持配置构建器 (`Builder` 模式) 来自定义大小。

2.  **扩容机制**:
    *   目前的 Pool 是静态的。如果内存耗尽，只能返回 `None` 或回退到 Global Allocator。
    *   对于注册内存来说，动态扩容（Re-registering）成本很高。未来可能需要探索 `IORING_REGISTER_BUFFERS_UPDATE` 或环形 Pool 链表。

3.  **线程模型**:
    *   目前的设计依然是 `Thread-Local` 优化的 (使用 `Rc<RefCell>`)。
    *   虽然 `FixedBuf` 可以被发送到其他线程 (如果标记为 Send)，但目前的 `drop` 逻辑假定 Pool 在本线程有效。若要支持跨线程释放（Work Stealing 场景），需要引入线程安全的 `Arc<Mutex/SpinLock>` 或无锁并发队列，但这会引入额外的同步开销。

4.  **BitSet 依赖**:
    *   `HybridPool` 依赖 `veloq_bitset` 进行 Double-Free 检测，性能开销需持续关注。
