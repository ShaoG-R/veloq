use crate::CallbackWrapper;
use crate::config::{BatchConfig, WheelConfig};
use crate::task::{
    TaskCompletion, TaskHandle, TaskId, TaskLocation, TaskTypeWithCompletionNotifier,
    TimerTaskForWheel, TimerTaskWithCompletionNotifier,
};
use deferred_map::DeferredMap;
use std::time::Duration;

pub struct WheelAdvanceResult {
    pub id: TaskId,
    pub callback: Option<CallbackWrapper>,
}

/// Timing wheel single layer data structure
///
/// 时间轮单层数据结构
struct WheelLayer {
    /// Slot array, each slot stores a group of timer tasks
    ///
    /// 槽数组，每个槽存储一组定时器任务
    slots: Vec<Vec<TimerTaskForWheel>>,

    /// Current time pointer (tick index)
    ///
    /// 当前时间指针（tick 索引）
    current_tick: u64,

    /// Slot count
    ///
    /// 槽数量
    slot_count: usize,

    /// Duration of each tick
    ///
    /// 每个 tick 的持续时间
    tick_duration: Duration,

    /// Cache: tick duration in milliseconds (u64) - avoid repeated conversion
    ///
    /// 缓存：tick 持续时间（毫秒，u64）- 避免重复转换
    tick_duration_ms: u64,

    /// Cache: slot mask (slot_count - 1) - for fast modulo operation
    ///
    /// 缓存：槽掩码（slot_count - 1）- 用于快速取模运算
    slot_mask: usize,
}

impl WheelLayer {
    /// Create a new wheel layer
    ///
    /// 创建一个新的时间轮层
    fn new(slot_count: usize, tick_duration: Duration) -> Self {
        let mut slots = Vec::with_capacity(slot_count);
        // Pre-allocate capacity for each slot to reduce subsequent reallocation during push
        // Most slots typically contain 0-4 tasks, pre-allocate capacity of 4
        //
        // 为每个槽预分配容量以减少后续 push 时的重新分配
        // 大多数槽通常包含 0-4 个任务，预分配容量为 4
        for _ in 0..slot_count {
            slots.push(Vec::with_capacity(4));
        }

        let tick_duration_ms = tick_duration.as_millis() as u64;
        let slot_mask = slot_count - 1;

        Self {
            slots,
            current_tick: 0,
            slot_count,
            tick_duration,
            tick_duration_ms,
            slot_mask,
        }
    }

    /// Calculate the number of ticks corresponding to the delay
    ///
    /// 计算延迟对应的 tick 数量
    fn delay_to_ticks(&self, delay: Duration) -> u64 {
        let ticks = delay.as_millis() as u64 / self.tick_duration.as_millis() as u64;
        ticks.max(1) // at least 1 tick (至少 1 个 tick)
    }
}

/// Timing wheel data structure (hierarchical mode)
///
/// Now uses DeferredMap for O(1) task lookup and generational safety.
///
/// 时间轮数据结构（分层模式）
///
/// 现在使用 DeferredMap 实现 O(1) 任务查找和代数安全
pub struct Wheel {
    /// L0 layer (bottom layer)
    ///
    /// L0 层（底层）
    l0: WheelLayer,

    /// L1 layer (top layer)
    ///
    /// L1 层（顶层）
    l1: WheelLayer,

    /// L1 tick ratio relative to L0 tick
    ///
    /// L1 tick 相对于 L0 tick 的比率
    pub(crate) l1_tick_ratio: u64,

    /// Task index for fast lookup and cancellation using DeferredMap
    ///
    /// Keys are TaskIds (u64 from DeferredMap), values are TaskLocations
    ///
    /// 任务索引，使用 DeferredMap 实现快速查找和取消
    ///
    /// 键是 TaskId（来自 DeferredMap 的 u64），值是 TaskLocation
    pub(crate) task_index: DeferredMap<TaskLocation>,

    /// Batch processing configuration
    ///
    /// 批处理配置
    batch_config: BatchConfig,

    /// Cache: L0 layer capacity in milliseconds - avoid repeated calculation
    ///
    /// 缓存：L0 层容量（毫秒）- 避免重复计算
    l0_capacity_ms: u64,

    /// Cache: L1 layer capacity in ticks - avoid repeated calculation
    ///
    /// 缓存：L1 层容量（tick 数）- 避免重复计算
    l1_capacity_ticks: u64,
}

impl Wheel {
    /// Create new timing wheel
    ///
    /// # Parameters
    /// - `config`: Timing wheel configuration (already validated)
    /// - `batch_config`: Batch processing configuration
    ///
    /// # Notes
    /// Configuration parameters have been validated in WheelConfig::builder().build(), so this method will not fail.
    /// Now uses DeferredMap for task indexing with generational safety.
    ///
    /// 创建新的时间轮
    ///
    /// # 参数
    /// - `config`: 时间轮配置（已验证）
    /// - `batch_config`: 批处理配置
    ///
    /// # 注意
    /// 配置参数已在 WheelConfig::builder().build() 中验证，因此此方法不会失败。
    /// 现在使用 DeferredMap 进行任务索引，具有代数安全特性。
    pub fn new(config: WheelConfig, batch_config: BatchConfig) -> Self {
        let l0 = WheelLayer::new(config.l0_slot_count, config.l0_tick_duration);
        let l1 = WheelLayer::new(config.l1_slot_count, config.l1_tick_duration);

        // Calculate L1 tick ratio relative to L0 tick
        // 计算 L1 tick 相对于 L0 tick 的比率
        let l1_tick_ratio = l1.tick_duration_ms / l0.tick_duration_ms;

        // Pre-calculate capacity to avoid repeated calculation in insert
        // 预计算容量，避免在 insert 中重复计算
        let l0_capacity_ms = (l0.slot_count as u64) * l0.tick_duration_ms;
        let l1_capacity_ticks = l1.slot_count as u64;

        // Estimate initial capacity for DeferredMap based on L0 slot count
        // 根据 L0 槽数量估算 DeferredMap 的初始容量
        let estimated_capacity = (l0.slot_count / 4).max(64);

        Self {
            l0,
            l1,
            l1_tick_ratio,
            task_index: DeferredMap::with_capacity(estimated_capacity),
            batch_config,
            l0_capacity_ms,
            l1_capacity_ticks,
        }
    }

    /// Get current tick (L0 layer tick)
    ///
    /// 获取当前 tick（L0 层 tick）
    #[allow(dead_code)]
    pub fn current_tick(&self) -> u64 {
        self.l0.current_tick
    }

    /// Get tick duration (L0 layer tick duration)
    ///
    /// 获取 tick 持续时间（L0 层 tick 持续时间）
    #[allow(dead_code)]
    pub fn tick_duration(&self) -> Duration {
        self.l0.tick_duration
    }

    /// Get slot count (L0 layer slot count)
    ///
    /// 获取槽数量（L0 层槽数量）
    #[allow(dead_code)]
    pub fn slot_count(&self) -> usize {
        self.l0.slot_count
    }

    /// Calculate the number of ticks corresponding to the delay (based on L0 layer)
    ///
    /// 计算延迟对应的 tick 数量（基于 L0 层）
    #[allow(dead_code)]
    pub fn delay_to_ticks(&self, delay: Duration) -> u64 {
        self.l0.delay_to_ticks(delay)
    }

    /// Determine which layer the delay should be inserted into
    ///
    /// # Returns
    /// Returns: (layer, ticks, rounds)
    /// - Layer: 0 = L0, 1 = L1
    /// - Ticks: number of ticks calculated from current tick
    /// - Rounds: number of rounds (only used for very long delays in L1 layer)
    ///
    /// 确定延迟应该插入到哪一层
    ///
    /// # 返回值
    /// 返回：(层级, ticks, 轮数)
    /// - 层级：0 = L0, 1 = L1
    /// - Ticks：从当前 tick 计算的 tick 数量
    /// - 轮数：轮数（仅用于 L1 层的超长延迟）
    #[inline(always)]
    fn determine_layer(&self, delay: Duration) -> (u8, u64, u32) {
        let delay_ms = delay.as_millis() as u64;

        // Fast path: most tasks are within L0 range (using cached capacity)
        // 快速路径：大多数任务在 L0 范围内（使用缓存的容量）
        if delay_ms < self.l0_capacity_ms {
            let l0_ticks = (delay_ms / self.l0.tick_duration_ms).max(1);
            return (0, l0_ticks, 0);
        }

        // Slow path: L1 layer tasks (using cached values)
        // 慢速路径：L1 层任务（使用缓存的值）
        let l1_ticks = (delay_ms / self.l1.tick_duration_ms).max(1);

        if l1_ticks < self.l1_capacity_ticks {
            (1, l1_ticks, 0)
        } else {
            let rounds = (l1_ticks / self.l1_capacity_ticks) as u32;
            (1, l1_ticks, rounds)
        }
    }

    /// Allocate a handle from DeferredMap
    ///
    /// 从 DeferredMap 分配一个 handle
    ///
    /// # Returns
    /// A unique handle for later insertion
    ///
    /// # 返回值
    /// 用于后续插入的唯一 handle
    pub fn allocate_handle(&mut self) -> TaskHandle {
        TaskHandle::new(self.task_index.allocate_handle())
    }

    /// Batch allocate handles from DeferredMap
    ///
    /// 从 DeferredMap 批量分配 handles
    ///
    /// # Parameters
    /// - `count`: Number of handles to allocate
    ///
    /// # Returns
    /// Vector of unique handles for later batch insertion
    ///
    /// # 参数
    /// - `count`: 要分配的 handle 数量
    ///
    /// # 返回值
    /// 用于后续批量插入的唯一 handles 向量
    pub fn allocate_handles(&mut self, count: usize) -> Vec<TaskHandle> {
        let mut handles = Vec::with_capacity(count);
        for _ in 0..count {
            handles.push(TaskHandle::new(self.task_index.allocate_handle()));
        }
        handles
    }

    /// Insert timer task
    ///
    /// # Parameters
    /// - `handle`: Handle for the task
    /// - `task`: Timer task with completion notifier
    ///
    /// # Returns
    /// Unique identifier of the task (TaskId) - now generated from DeferredMap
    ///
    /// # Implementation Details
    /// - Allocates Handle from DeferredMap to generate TaskId with generational safety
    /// - Automatically calculate the layer and slot where the task should be inserted
    /// - Hierarchical mode: short delay tasks are inserted into L0, long delay tasks are inserted into L1
    /// - Use bit operations to optimize slot index calculation
    /// - Use DeferredMap for O(1) lookup and cancellation with generation checking
    ///
    /// 插入定时器任务
    ///
    /// # 参数
    /// - `handle`: 任务的 handle
    /// - `task`: 带完成通知器的定时器任务
    ///
    /// # 返回值
    /// 任务的唯一标识符（TaskId）- 现在从 DeferredMap 生成
    ///
    /// # 实现细节
    /// - 自动计算任务应该插入的层级和槽位
    /// - 分层模式：短延迟任务插入 L0，长延迟任务插入 L1
    /// - 使用位运算优化槽索引计算
    /// - 使用 DeferredMap 实现 O(1) 查找和取消，带代数检查
    #[inline]
    pub fn insert(&mut self, handle: TaskHandle, task: TimerTaskWithCompletionNotifier) {
        let task_id = handle.task_id();

        let (level, ticks, rounds) = self.determine_layer(task.delay);

        // Use match to reduce branches, and use cached slot mask
        // 使用 match 减少分支，并使用缓存的槽掩码
        let (current_tick, slot_mask, slots) = match level {
            0 => (self.l0.current_tick, self.l0.slot_mask, &mut self.l0.slots),
            _ => (self.l1.current_tick, self.l1.slot_mask, &mut self.l1.slots),
        };

        let total_ticks = current_tick + ticks;
        let slot_index = (total_ticks as usize) & slot_mask;

        // Create task with the assigned TaskId
        // 使用分配的 TaskId 创建任务
        let task = TimerTaskForWheel::new_with_id(task_id, task, total_ticks, rounds);

        // Get the index position of the task in Vec (the length before insertion is the index of the new task)
        // 获取任务在 Vec 中的索引位置（插入前的长度就是新任务的索引）
        let vec_index = slots[slot_index].len();
        let location = TaskLocation::new(level, slot_index, vec_index);

        // Insert task into slot
        // 将任务插入槽中
        slots[slot_index].push(task);

        // Insert task location into DeferredMap using handle
        // 使用 handle 将任务位置插入 DeferredMap
        self.task_index.insert(handle.into_handle(), location);
    }

    /// Batch insert timer tasks
    ///
    /// # Parameters
    /// - `handles`: List of pre-allocated handles for tasks
    /// - `tasks`: List of timer tasks with completion notifiers
    ///
    /// # Returns
    /// - `Ok(())` if all tasks are successfully inserted
    /// - `Err(TimerError::BatchLengthMismatch)` if handles and tasks lengths don't match
    ///
    /// # Performance Advantages
    /// - Reduce repeated boundary checks and capacity adjustments
    /// - For tasks with the same delay, calculation results can be reused
    /// - Uses DeferredMap for efficient task indexing with generational safety
    ///
    /// 批量插入定时器任务
    ///
    /// # 参数
    /// - `handles`: 任务的预分配 handles 列表
    /// - `tasks`: 带完成通知器的定时器任务列表
    ///
    /// # 返回值
    /// - `Ok(())` 如果所有任务成功插入
    /// - `Err(TimerError::BatchLengthMismatch)` 如果 handles 和 tasks 长度不匹配
    ///
    /// # 性能优势
    /// - 减少重复的边界检查和容量调整
    /// - 对于相同延迟的任务，可以重用计算结果
    /// - 使用 DeferredMap 实现高效的任务索引，具有代数安全特性
    #[inline]
    pub fn insert_batch(
        &mut self,
        handles: Vec<TaskHandle>,
        tasks: Vec<TimerTaskWithCompletionNotifier>,
    ) -> Result<(), crate::error::TimerError> {
        // Validate that handles and tasks have the same length
        // 验证 handles 和 tasks 长度相同
        if handles.len() != tasks.len() {
            return Err(crate::error::TimerError::BatchLengthMismatch {
                handles_len: handles.len(),
                tasks_len: tasks.len(),
            });
        }

        for (handle, task) in handles.into_iter().zip(tasks.into_iter()) {
            let task_id = handle.task_id();

            let (level, ticks, rounds) = self.determine_layer(task.delay);

            // Use match to reduce branches, and use cached slot mask
            // 使用 match 减少分支，并使用缓存的槽掩码
            let (current_tick, slot_mask, slots) = match level {
                0 => (self.l0.current_tick, self.l0.slot_mask, &mut self.l0.slots),
                _ => (self.l1.current_tick, self.l1.slot_mask, &mut self.l1.slots),
            };

            let total_ticks = current_tick + ticks;
            let slot_index = (total_ticks as usize) & slot_mask;

            // Create task with the assigned TaskId
            // 使用分配的 TaskId 创建任务
            let task = TimerTaskForWheel::new_with_id(task_id, task, total_ticks, rounds);

            // Get the index position of the task in Vec
            // 获取任务在 Vec 中的索引位置
            let vec_index = slots[slot_index].len();
            let location = TaskLocation::new(level, slot_index, vec_index);

            // Insert task into slot
            // 将任务插入槽中
            slots[slot_index].push(task);

            // Insert task location into DeferredMap using handle
            // 使用 handle 将任务位置插入 DeferredMap
            self.task_index.insert(handle.into_handle(), location);
        }

        Ok(())
    }

    /// Cancel timer task
    ///
    /// # Parameters
    /// - `task_id`: Task ID
    ///
    /// # Returns
    /// Returns true if the task exists and is successfully cancelled, otherwise returns false
    ///
    /// Now uses DeferredMap for safe task removal with generational checking
    ///
    /// 取消定时器任务
    ///
    /// # 参数
    /// - `task_id`: 任务 ID
    ///
    /// # 返回值
    /// 如果任务存在且成功取消则返回 true，否则返回 false
    ///
    /// 现在使用 DeferredMap 实现安全的任务移除，带代数检查
    #[inline]
    pub fn cancel(&mut self, task_id: TaskId) -> bool {
        // Remove task location from DeferredMap using TaskId as key
        // 使用 TaskId 作为 key 从 DeferredMap 中移除任务位置
        let location = match self.task_index.remove(task_id.key()) {
            Some(loc) => loc,
            None => return false, // Task not found or already removed (generation mismatch)
        };

        // Use match to get slot reference, reduce branches
        // 使用 match 获取槽引用，减少分支
        let slot = match location.level {
            0 => &mut self.l0.slots[location.slot_index],
            _ => &mut self.l1.slots[location.slot_index],
        };

        // Boundary check and ID verification
        // 边界检查和 ID 验证
        if location.vec_index >= slot.len() || slot[location.vec_index].get_id() != task_id {
            // Index inconsistent - this shouldn't happen with DeferredMap, but handle it anyway
            // 索引不一致 - DeferredMap 不应该出现这种情况，但还是处理一下
            return false;
        }

        // Use swap_remove to remove task, record swapped task ID
        // 使用 swap_remove 移除任务，记录被交换的任务 ID
        let removed_task = slot.swap_remove(location.vec_index);

        // Ensure the correct task was removed
        // 确保移除了正确的任务
        debug_assert_eq!(removed_task.get_id(), task_id);

        match removed_task.into_task_type() {
            TaskTypeWithCompletionNotifier::OneShot {
                completion_notifier,
            } => {
                // Use Notify + AtomicU8 for zero-allocation notification
                // 使用 Notify + AtomicU8 实现零分配通知
                completion_notifier.notify(crate::task::TaskCompletion::Cancelled);
            }
            TaskTypeWithCompletionNotifier::Periodic {
                completion_notifier,
                ..
            } => {
                // Use flume for high-performance periodic notification
                // 使用 flume 实现高性能周期通知
                let _ = completion_notifier.0.try_send(TaskCompletion::Cancelled);
            }
        }

        // If a swap occurred (vec_index is not the last element)
        // 如果发生了交换（vec_index 不是最后一个元素）
        if location.vec_index < slot.len() {
            let swapped_task_id = slot[location.vec_index].get_id();
            // Update swapped element's index in one go, using DeferredMap's get_mut
            // 一次性更新被交换元素的索引，使用 DeferredMap 的 get_mut
            if let Some(swapped_location) = self.task_index.get_mut(swapped_task_id.key()) {
                swapped_location.vec_index = location.vec_index;
            }
        }

        true
    }

    /// Batch cancel timer tasks
    ///
    /// # Parameters
    /// - `task_ids`: List of task IDs to cancel
    ///
    /// # Returns
    /// Number of successfully cancelled tasks
    ///
    /// # Performance Advantages
    /// - Reduce repeated HashMap lookup overhead
    /// - Multiple cancellation operations on the same slot can be batch processed
    /// - Use unstable sort to improve performance
    /// - Small batch optimization: skip sorting based on configuration threshold, process directly
    ///
    /// 批量取消定时器任务
    ///
    /// # 参数
    /// - `task_ids`: 要取消的任务 ID 列表
    ///
    /// # 返回值
    /// 成功取消的任务数量
    ///
    /// # 性能优势
    /// - 减少重复的 HashMap 查找开销
    /// - 同一槽位的多个取消操作可以批量处理
    /// - 使用不稳定排序提高性能
    /// - 小批量优化：根据配置阈值跳过排序，直接处理
    #[inline]
    pub fn cancel_batch(&mut self, task_ids: &[TaskId]) -> usize {
        let mut cancelled_count = 0;

        // Small batch optimization: cancel one by one to avoid grouping and sorting overhead
        // 小批量优化：逐个取消以避免分组和排序开销
        if task_ids.len() <= self.batch_config.small_batch_threshold {
            for &task_id in task_ids {
                if self.cancel(task_id) {
                    cancelled_count += 1;
                }
            }
            return cancelled_count;
        }

        // Group by layer and slot to optimize batch cancellation
        // Use SmallVec to avoid heap allocation in most cases
        // 按层级和槽位分组以优化批量取消
        // 使用 SmallVec 避免在大多数情况下进行堆分配
        let l0_slot_count = self.l0.slot_count;
        let l1_slot_count = self.l1.slot_count;

        let mut l0_tasks_by_slot: Vec<Vec<(TaskId, usize)>> = vec![Vec::new(); l0_slot_count];
        let mut l1_tasks_by_slot: Vec<Vec<(TaskId, usize)>> = vec![Vec::new(); l1_slot_count];

        // Collect information of tasks to be cancelled
        // 收集要取消的任务信息
        for &task_id in task_ids {
            // Use DeferredMap's get with TaskId key
            // 使用 DeferredMap 的 get，传入 TaskId key
            if let Some(location) = self.task_index.get(task_id.key()) {
                if location.level == 0 {
                    l0_tasks_by_slot[location.slot_index].push((task_id, location.vec_index));
                } else {
                    l1_tasks_by_slot[location.slot_index].push((task_id, location.vec_index));
                }
            }
        }

        // Process L0 layer cancellation
        // 处理 L0 层取消
        for (slot_index, tasks) in l0_tasks_by_slot.iter_mut().enumerate() {
            if tasks.is_empty() {
                continue;
            }

            // Sort by vec_index in descending order, delete from back to front to avoid index invalidation
            // 按 vec_index 降序排序，从后向前删除以避免索引失效
            tasks.sort_unstable_by(|a, b| b.1.cmp(&a.1));

            let slot = &mut self.l0.slots[slot_index];

            for &(task_id, vec_index) in tasks.iter() {
                if vec_index < slot.len() && slot[vec_index].get_id() == task_id {
                    let removed_task = slot.swap_remove(vec_index);
                    match removed_task.into_task_type() {
                        TaskTypeWithCompletionNotifier::OneShot {
                            completion_notifier,
                        } => {
                            // Use Notify + AtomicU8 for zero-allocation notification
                            // 使用 Notify + AtomicU8 实现零分配通知
                            completion_notifier.notify(crate::task::TaskCompletion::Cancelled);
                        }
                        TaskTypeWithCompletionNotifier::Periodic {
                            completion_notifier,
                            ..
                        } => {
                            // Use flume for high-performance periodic notification
                            // 使用 flume 实现高性能周期通知
                            let _ = completion_notifier.0.try_send(TaskCompletion::Cancelled);
                        }
                    }

                    if vec_index < slot.len() {
                        let swapped_task_id = slot[vec_index].get_id();
                        // Use DeferredMap's get_mut with TaskId key
                        // 使用 DeferredMap 的 get_mut，传入 TaskId key
                        if let Some(swapped_location) =
                            self.task_index.get_mut(swapped_task_id.key())
                        {
                            swapped_location.vec_index = vec_index;
                        }
                    }

                    // Use DeferredMap's remove with TaskId key
                    // 使用 DeferredMap 的 remove，传入 TaskId key
                    self.task_index.remove(task_id.key());
                    cancelled_count += 1;
                }
            }
        }

        // Process L1 layer cancellation
        // 处理 L1 层取消
        for (slot_index, tasks) in l1_tasks_by_slot.iter_mut().enumerate() {
            if tasks.is_empty() {
                continue;
            }

            tasks.sort_unstable_by(|a, b| b.1.cmp(&a.1));

            let slot = &mut self.l1.slots[slot_index];

            for &(task_id, vec_index) in tasks.iter() {
                if vec_index < slot.len() && slot[vec_index].get_id() == task_id {
                    let removed_task = slot.swap_remove(vec_index);

                    match removed_task.into_task_type() {
                        TaskTypeWithCompletionNotifier::OneShot {
                            completion_notifier,
                        } => {
                            // Use Notify + AtomicU8 for zero-allocation notification
                            // 使用 Notify + AtomicU8 实现零分配通知
                            completion_notifier.notify(crate::task::TaskCompletion::Cancelled);
                        }
                        TaskTypeWithCompletionNotifier::Periodic {
                            completion_notifier,
                            ..
                        } => {
                            // Use flume for high-performance periodic notification
                            // 使用 flume 实现高性能周期通知
                            let _ = completion_notifier.0.try_send(TaskCompletion::Cancelled);
                        }
                    }

                    if vec_index < slot.len() {
                        let swapped_task_id = slot[vec_index].get_id();
                        // Use DeferredMap's get_mut with TaskId key
                        // 使用 DeferredMap 的 get_mut，传入 TaskId key
                        if let Some(swapped_location) =
                            self.task_index.get_mut(swapped_task_id.key())
                        {
                            swapped_location.vec_index = vec_index;
                        }
                    }

                    // Use DeferredMap's remove with TaskId key
                    // 使用 DeferredMap 的 remove，传入 TaskId key
                    self.task_index.remove(task_id.key());
                    cancelled_count += 1;
                }
            }
        }

        cancelled_count
    }

    /// Reinsert periodic task with the same TaskId
    ///
    /// Used to automatically reschedule periodic tasks after they expire
    ///
    /// TaskId remains unchanged, only location is updated in DeferredMap
    ///
    /// 重新插入周期性任务（保持相同的 TaskId）
    ///
    /// 用于在周期性任务过期后自动重新调度
    ///
    /// TaskId 保持不变，只更新 DeferredMap 中的位置
    fn reinsert_periodic_task(&mut self, task_id: TaskId, task: TimerTaskWithCompletionNotifier) {
        // Determine which layer the interval should be inserted into
        // This method is only called by periodic tasks, so the interval is guaranteed to be Some.
        // 确定间隔应该插入到哪一层
        // 该方法只能由周期性任务调用，所以间隔是 guaranteed 应当保证为 Some.
        let (level, ticks, rounds) = self.determine_layer(task.get_interval().unwrap());

        // Use match to reduce branches, and use cached slot mask
        // 使用 match 减少分支，并使用缓存的槽掩码
        let (current_tick, slot_mask, slots) = match level {
            0 => (self.l0.current_tick, self.l0.slot_mask, &mut self.l0.slots),
            _ => (self.l1.current_tick, self.l1.slot_mask, &mut self.l1.slots),
        };

        let total_ticks = current_tick + ticks;
        let slot_index = (total_ticks as usize) & slot_mask;

        // Get the index position of the task in Vec
        // 获取任务在 Vec 中的索引位置
        let vec_index = slots[slot_index].len();
        let new_location = TaskLocation::new(level, slot_index, vec_index);

        // Insert task into slot
        // 将任务插入槽中
        slots[slot_index].push(TimerTaskForWheel::new_with_id(
            task_id,
            task,
            total_ticks,
            rounds,
        ));

        // Update task location in DeferredMap (doesn't remove, just updates)
        // 更新 DeferredMap 中的任务位置（不删除，仅更新）
        if let Some(location) = self.task_index.get_mut(task_id.key()) {
            *location = new_location;
        }
    }

    /// Advance the timing wheel by one tick, return all expired tasks
    ///
    /// # Returns
    /// List of expired tasks
    ///
    /// # Implementation Details
    /// - L0 layer advances 1 tick each time (no rounds check)
    /// - L1 layer advances once every (L1_tick / L0_tick) times
    /// - L1 expired tasks are batch demoted to L0
    ///
    /// 推进时间轮一个 tick，返回所有过期的任务
    ///
    /// # 返回值
    /// 过期任务列表
    ///
    /// # 实现细节
    /// - L0 层每次推进 1 个 tick（无轮数检查）
    /// - L1 层每 (L1_tick / L0_tick) 次推进一次
    /// - L1 层过期任务批量降级到 L0
    pub fn advance(&mut self) -> Vec<WheelAdvanceResult> {
        // Advance L0 layer
        // 推进 L0 层
        self.l0.current_tick += 1;

        let mut expired_tasks = Vec::new();

        // Process L0 layer expired tasks
        // Distinguish between one-shot and periodic tasks
        // 处理 L0 层过期任务
        // 区分一次性任务和周期性任务
        let l0_slot_index = (self.l0.current_tick as usize) & self.l0.slot_mask;

        // Collect periodic tasks to reinsert
        // 收集需要重新插入的周期性任务
        let mut periodic_tasks_to_reinsert = Vec::new();

        {
            let l0_slot = &mut self.l0.slots[l0_slot_index];

            // Process all tasks in the current slot by always removing from index 0
            // This avoids index tracking issues with swap_remove
            // 通过始终从索引 0 移除来处理当前槽中的所有任务
            // 这避免了 swap_remove 的索引跟踪问题
            while !l0_slot.is_empty() {
                // Always remove from index 0
                // 始终从索引 0 移除
                let task_with_notifier = l0_slot.swap_remove(0);
                let task_id = task_with_notifier.get_id();

                // Determine if this is a periodic task (check before consuming)
                // 判断是否为周期性任务（在消耗前检查）
                let is_periodic = matches!(
                    task_with_notifier.task.task_type,
                    TaskTypeWithCompletionNotifier::Periodic { .. }
                );

                // For one-shot tasks, remove from DeferredMap index
                // For periodic tasks, keep in index (will update location in reinsert)
                // 对于一次性任务，从 DeferredMap 索引中移除
                // 对于周期性任务，保留在索引中（重新插入时会更新位置）
                if !is_periodic {
                    self.task_index.remove(task_id.key());
                }

                // Update swapped element's index (the element that was moved to index 0)
                // 更新被交换元素的索引（被移动到索引 0 的元素）
                if !l0_slot.is_empty() {
                    let swapped_task_id = l0_slot[0].get_id();
                    // Use DeferredMap's get_mut with TaskId key
                    // 使用 DeferredMap 的 get_mut，传入 TaskId key
                    if let Some(swapped_location) = self.task_index.get_mut(swapped_task_id.key()) {
                        swapped_location.vec_index = 0;
                    }
                }

                let TimerTaskForWheel { task_id, task, .. } = task_with_notifier;

                match &task.task_type {
                    TaskTypeWithCompletionNotifier::Periodic {
                        completion_notifier,
                        ..
                    } => {
                        let _ = completion_notifier.0.try_send(TaskCompletion::Called);

                        periodic_tasks_to_reinsert.push((
                            task_id,
                            TimerTaskWithCompletionNotifier {
                                task_type: task.task_type,
                                delay: task.delay,
                                callback: task.callback.clone(),
                            },
                        ));
                    }
                    TaskTypeWithCompletionNotifier::OneShot {
                        completion_notifier,
                    } => {
                        completion_notifier.notify(crate::task::TaskCompletion::Called);
                    }
                }

                expired_tasks.push(WheelAdvanceResult {
                    id: task_id,
                    callback: task.callback,
                });
            }
        }

        // Reinsert periodic tasks for next interval
        // 重新插入周期性任务到下一个周期
        for (task_id, task) in periodic_tasks_to_reinsert {
            self.reinsert_periodic_task(task_id, task);
        }

        // Process L1 layer
        // Check if L1 layer needs to be advanced
        // 处理 L1 层
        // 检查是否需要推进 L1 层
        if self.l0.current_tick.is_multiple_of(self.l1_tick_ratio) {
            self.l1.current_tick += 1;
            let l1_slot_index = (self.l1.current_tick as usize) & self.l1.slot_mask;
            let l1_slot = &mut self.l1.slots[l1_slot_index];

            // Collect L1 layer expired tasks
            // 收集 L1 层过期任务
            let mut tasks_to_demote = Vec::new();
            let mut i = 0;
            while i < l1_slot.len() {
                let task = &mut l1_slot[i];

                if task.rounds > 0 {
                    // Still has rounds, decrease rounds and keep
                    // 还有轮数，减少轮数并保留
                    task.rounds -= 1;
                    // Use DeferredMap's get_mut with TaskId key
                    // 使用 DeferredMap 的 get_mut，传入 TaskId key
                    if let Some(location) = self.task_index.get_mut(task.get_id().key()) {
                        location.vec_index = i;
                    }
                    i += 1;
                } else {
                    // rounds = 0, need to demote to L0
                    // Don't remove from task_index - will update location in demote_tasks
                    // rounds = 0，需要降级到 L0
                    // 不从 task_index 中移除 - 在 demote_tasks 中会更新位置
                    let task_to_demote = l1_slot.swap_remove(i);

                    if i < l1_slot.len() {
                        let swapped_task_id = l1_slot[i].get_id();
                        // Use DeferredMap's get_mut with TaskId key
                        // 使用 DeferredMap 的 get_mut，传入 TaskId key
                        if let Some(swapped_location) =
                            self.task_index.get_mut(swapped_task_id.key())
                        {
                            swapped_location.vec_index = i;
                        }
                    }

                    tasks_to_demote.push(task_to_demote);
                }
            }

            // Demote tasks to L0
            // 将任务降级到 L0
            self.demote_tasks(tasks_to_demote);
        }

        expired_tasks
    }

    /// Demote tasks from L1 to L0
    ///
    /// Recalculate and insert L1 expired tasks into L0 layer
    ///
    /// Updates task location in DeferredMap without removing/reinserting
    ///
    /// 将任务从 L1 降级到 L0
    ///
    /// 重新计算并将 L1 过期任务插入到 L0 层
    ///
    /// 更新 DeferredMap 中的任务位置而不删除/重新插入
    fn demote_tasks(&mut self, tasks: Vec<TimerTaskForWheel>) {
        for task in tasks {
            // Calculate the remaining delay of the task in L0 layer
            // The task's deadline_tick is based on L1 tick, needs to be converted to L0 tick
            // 计算任务在 L0 层的剩余延迟
            // 任务的 deadline_tick 基于 L1 tick，需要转换为 L0 tick
            let l1_tick_ratio = self.l1_tick_ratio;

            // Calculate the original expiration time of the task (L1 tick)
            // 计算任务的原始过期时间（L1 tick）
            let l1_deadline = task.deadline_tick;

            // Convert to L0 tick expiration time
            // 转换为 L0 tick 过期时间
            let l0_deadline_tick = l1_deadline * l1_tick_ratio;
            let l0_current_tick = self.l0.current_tick;

            // Calculate remaining L0 ticks
            // 计算剩余 L0 ticks
            let remaining_l0_ticks = if l0_deadline_tick > l0_current_tick {
                l0_deadline_tick - l0_current_tick
            } else {
                1 // At least trigger in next tick (至少在下一个 tick 触发)
            };

            // Calculate L0 slot index
            // 计算 L0 槽索引
            let target_l0_tick = l0_current_tick + remaining_l0_ticks;
            let l0_slot_index = (target_l0_tick as usize) & self.l0.slot_mask;

            let task_id = task.get_id();
            let vec_index = self.l0.slots[l0_slot_index].len();
            let new_location = TaskLocation::new(0, l0_slot_index, vec_index);

            // Insert into L0 layer
            // 插入到 L0 层
            self.l0.slots[l0_slot_index].push(task);

            // Update task location in DeferredMap (task already exists in index)
            // 更新 DeferredMap 中的任务位置（任务已在索引中）
            if let Some(location) = self.task_index.get_mut(task_id.key()) {
                *location = new_location;
            }
        }
    }

    /// Check if the timing wheel is empty
    ///
    /// 检查时间轮是否为空
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.task_index.is_empty()
    }

    /// Postpone timer task (keep original TaskId)
    ///
    /// # Parameters
    /// - `task_id`: Task ID to postpone
    /// - `new_delay`: New delay time (recalculated from current tick, not continuing from original delay time)
    /// - `new_callback`: New callback function (if None, keep original callback)
    ///
    /// # Returns
    /// Returns true if the task exists and is successfully postponed, otherwise returns false
    ///
    /// # Implementation Details
    /// - Remove task from original layer/slot, keep its completion_notifier (will not trigger cancellation notification)
    /// - Update delay time and callback function (if provided)
    /// - Recalculate target layer, slot, and rounds based on new_delay
    /// - Cross-layer migration may occur (L0 <-> L1)
    /// - Re-insert to new position using original TaskId
    /// - Keep consistent with external held TaskId reference
    ///
    /// 延期定时器任务（保留原始 TaskId）
    ///
    /// # 参数
    /// - `task_id`: 要延期的任务 ID
    /// - `new_delay`: 新延迟时间（从当前 tick 重新计算，而非从原延迟时间继续）
    /// - `new_callback`: 新回调函数（如果为 None，则保留原回调）
    ///
    /// # 返回值
    /// 如果任务存在且成功延期则返回 true，否则返回 false
    ///
    /// # 实现细节
    /// - 从原层级/槽位移除任务，保留其 completion_notifier（不会触发取消通知）
    /// - 更新延迟时间和回调函数（如果提供）
    /// - 根据 new_delay 重新计算目标层级、槽位和轮数
    /// - 可能发生跨层迁移（L0 <-> L1）
    /// - 使用原始 TaskId 重新插入到新位置
    /// - 与外部持有的 TaskId 引用保持一致
    #[inline]
    pub fn postpone(
        &mut self,
        task_id: TaskId,
        new_delay: Duration,
        new_callback: Option<crate::task::CallbackWrapper>,
    ) -> bool {
        // Step 1: Get task location from DeferredMap (don't remove)
        // 步骤 1: 从 DeferredMap 获取任务位置（不删除）
        let old_location = match self.task_index.get(task_id.key()) {
            Some(loc) => *loc,
            None => return false,
        };

        // Use match to get slot reference
        // 使用 match 获取槽引用
        let slot = match old_location.level {
            0 => &mut self.l0.slots[old_location.slot_index],
            _ => &mut self.l1.slots[old_location.slot_index],
        };

        // Verify task is still at expected position
        // 验证任务仍在预期位置
        if old_location.vec_index >= slot.len() || slot[old_location.vec_index].get_id() != task_id
        {
            // Index inconsistent, return failure
            // 索引不一致，返回失败
            return false;
        }

        // Use swap_remove to remove task from slot
        // 使用 swap_remove 从槽中移除任务
        let mut task = slot.swap_remove(old_location.vec_index);

        // Update swapped element's index (if a swap occurred)
        // 更新被交换元素的索引（如果发生了交换）
        if old_location.vec_index < slot.len() {
            let swapped_task_id = slot[old_location.vec_index].get_id();
            // Use DeferredMap's get_mut with TaskId key
            // 使用 DeferredMap 的 get_mut，传入 TaskId key
            if let Some(swapped_location) = self.task_index.get_mut(swapped_task_id.key()) {
                swapped_location.vec_index = old_location.vec_index;
            }
        }

        // Step 2: Update task's delay and callback
        // 步骤 2: 更新任务的延迟和回调
        task.update_delay(new_delay);
        if let Some(callback) = new_callback {
            task.update_callback(callback);
        }

        // Step 3: Recalculate layer, slot, and rounds based on new delay
        // 步骤 3: 根据新延迟重新计算层级、槽位和轮数
        let (new_level, ticks, new_rounds) = self.determine_layer(new_delay);

        // Use match to reduce branches, and use cached slot mask
        // 使用 match 减少分支，并使用缓存的槽掩码
        let (current_tick, slot_mask, slots) = match new_level {
            0 => (self.l0.current_tick, self.l0.slot_mask, &mut self.l0.slots),
            _ => (self.l1.current_tick, self.l1.slot_mask, &mut self.l1.slots),
        };

        let total_ticks = current_tick + ticks;
        let new_slot_index = (total_ticks as usize) & slot_mask;

        // Update task's timing wheel parameters
        // 更新任务的时间轮参数
        task.deadline_tick = total_ticks;
        task.rounds = new_rounds;

        // Step 4: Re-insert task to new layer/slot
        // 步骤 4: 将任务重新插入到新层级/槽位
        let new_vec_index = slots[new_slot_index].len();
        let new_location = TaskLocation::new(new_level, new_slot_index, new_vec_index);

        slots[new_slot_index].push(task);

        // Update task location in DeferredMap (task already exists in index)
        // 更新 DeferredMap 中的任务位置（任务已在索引中）
        if let Some(location) = self.task_index.get_mut(task_id.key()) {
            *location = new_location;
        }

        true
    }

    /// Batch postpone timer tasks
    ///
    /// # Parameters
    /// - `updates`: List of tuples of (task ID, new delay)
    ///
    /// # Returns
    /// Number of successfully postponed tasks
    ///
    /// # Performance Advantages
    /// - Batch processing reduces function call overhead
    /// - All delays are recalculated from current_tick at call time
    ///
    /// # Notes
    /// - If a task ID does not exist, that task will be skipped without affecting other tasks' postponement
    ///
    /// 批量延期定时器任务
    ///
    /// # 参数
    /// - `updates`: (任务 ID, 新延迟) 元组列表
    ///
    /// # 返回值
    /// 成功延期的任务数量
    ///
    /// # 性能优势
    /// - 批处理减少函数调用开销
    /// - 所有延迟在调用时从 current_tick 重新计算
    ///
    /// # 注意
    /// - 如果任务 ID 不存在，该任务将被跳过，不影响其他任务的延期
    #[inline]
    pub fn postpone_batch(&mut self, updates: Vec<(TaskId, Duration)>) -> usize {
        let mut postponed_count = 0;

        for (task_id, new_delay) in updates {
            if self.postpone(task_id, new_delay, None) {
                postponed_count += 1;
            }
        }

        postponed_count
    }

    /// Batch postpone timer tasks (replace callbacks)
    ///
    /// # Parameters
    /// - `updates`: List of tuples of (task ID, new delay, new callback)
    ///
    /// # Returns
    /// Number of successfully postponed tasks
    ///
    /// 批量延期定时器任务（替换回调）
    ///
    /// # 参数
    /// - `updates`: (任务 ID, 新延迟, 新回调) 元组列表
    ///
    /// # 返回值
    /// 成功延期的任务数量
    pub fn postpone_batch_with_callbacks(
        &mut self,
        updates: Vec<(TaskId, Duration, Option<crate::task::CallbackWrapper>)>,
    ) -> usize {
        let mut postponed_count = 0;

        for (task_id, new_delay, new_callback) in updates {
            if self.postpone(task_id, new_delay, new_callback) {
                postponed_count += 1;
            }
        }

        postponed_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{CallbackWrapper, TimerTask, TimerTaskWithCompletionNotifier};

    #[test]
    fn test_wheel_creation() {
        let wheel = Wheel::new(WheelConfig::default(), BatchConfig::default());
        assert_eq!(wheel.slot_count(), 512);
        assert_eq!(wheel.current_tick(), 0);
        assert!(wheel.is_empty());
    }

    #[test]
    fn test_hierarchical_wheel_creation() {
        let config = WheelConfig::default();

        let wheel = Wheel::new(config, BatchConfig::default());
        assert_eq!(wheel.slot_count(), 512); // L0 slot count (L0 层槽数量)
        assert_eq!(wheel.current_tick(), 0);
        assert!(wheel.is_empty());
        // L1 layer always exists in hierarchical mode (L1 层在分层模式下始终存在)
        assert_eq!(wheel.l1.slot_count, 64);
        assert_eq!(wheel.l1_tick_ratio, 100); // 1000ms / 10ms = 100 ticks (1000毫秒 / 10毫秒 = 100个tick)
    }

    #[test]
    fn test_hierarchical_config_validation() {
        // L1 tick must be an integer multiple of L0 tick (L1 tick 必须是 L0 tick 的整数倍)
        let result = WheelConfig::builder()
            .l0_tick_duration(Duration::from_millis(10))
            .l0_slot_count(512)
            .l1_tick_duration(Duration::from_millis(15)) // Not an integer multiple (不是整数倍)
            .l1_slot_count(64)
            .build();

        assert!(result.is_err());

        // Correct configuration (正确的配置)
        let result = WheelConfig::builder()
            .l0_tick_duration(Duration::from_millis(10))
            .l0_slot_count(512)
            .l1_tick_duration(Duration::from_secs(1)) // 1000ms / 10ms = 100 ticks (1000毫秒 / 10毫秒 = 100个tick)
            .l1_slot_count(64)
            .build();

        assert!(result.is_ok());
    }

    #[test]
    fn test_layer_determination() {
        let config = WheelConfig::default();

        let wheel = Wheel::new(config, BatchConfig::default());

        // Short delay should enter L0 layer (短延迟应该进入 L0 层)
        // L0: 512 slots * 10ms = 5120ms
        let (level, _, rounds) = wheel.determine_layer(Duration::from_millis(100));
        assert_eq!(level, 0);
        assert_eq!(rounds, 0);

        // Long delay should enter L1 layer
        // 超过 L0 范围（>5120ms） (超过 L0 范围 (>5120毫秒))
        let (level, _, rounds) = wheel.determine_layer(Duration::from_secs(10));
        assert_eq!(level, 1);
        assert_eq!(rounds, 0);

        // Long delay should enter L1 layer and have rounds
        // L1: 64 slots * 1000ms = 64000ms (L1: 64个槽 * 1000毫秒 = 64000毫秒)
        let (level, _, rounds) = wheel.determine_layer(Duration::from_secs(120));
        assert_eq!(level, 1);
        assert!(rounds > 0);
    }

    #[test]
    fn test_hierarchical_insert_and_advance() {
        let config = WheelConfig::default();

        let mut wheel = Wheel::new(config, BatchConfig::default());

        // Insert short delay task into L0 (插入短延迟任务到 L0)
        let callback = CallbackWrapper::new(|| async {});
        let task = TimerTask::new_oneshot(Duration::from_millis(100), Some(callback));
        let (task_with_notifier, _completion_rx) =
            TimerTaskWithCompletionNotifier::from_timer_task(task);
        let handle = wheel.allocate_handle();
        let task_id = handle.task_id();
        wheel.insert(handle, task_with_notifier);

        // Verify task is in L0 layer (验证任务在 L0 层)
        let location = wheel.task_index.get(task_id.key()).unwrap();
        assert_eq!(location.level, 0);

        // Advance 10 ticks (100ms) (前进 10 个 tick (100毫秒))
        for _ in 0..10 {
            let expired = wheel.advance();
            if !expired.is_empty() {
                assert_eq!(expired.len(), 1);
                assert_eq!(expired[0].id, task_id);
                return;
            }
        }
        panic!("Task should have expired");
    }

    #[test]
    fn test_cross_layer_cancel() {
        let config = WheelConfig::default();

        let mut wheel = Wheel::new(config, BatchConfig::default());

        // Insert L0 task (插入 L0 任务)
        let callback1 = CallbackWrapper::new(|| async {});
        let task1 = TimerTask::new_oneshot(Duration::from_millis(100), Some(callback1));
        let (task_with_notifier1, _rx1) = TimerTaskWithCompletionNotifier::from_timer_task(task1);
        let handle1 = wheel.allocate_handle();
        let task_id1 = handle1.task_id();
        wheel.insert(handle1, task_with_notifier1);

        // Insert L1 task (插入 L1 任务)
        let callback2 = CallbackWrapper::new(|| async {});
        let task2 = TimerTask::new_oneshot(Duration::from_secs(10), Some(callback2));
        let (task_with_notifier2, _rx2) = TimerTaskWithCompletionNotifier::from_timer_task(task2);
        let handle2 = wheel.allocate_handle();
        let task_id2 = handle2.task_id();
        wheel.insert(handle2, task_with_notifier2);

        // Verify levels (验证层级)
        assert_eq!(wheel.task_index.get(task_id1.key()).unwrap().level, 0);
        assert_eq!(wheel.task_index.get(task_id2.key()).unwrap().level, 1);

        // Cancel L0 task (取消 L0 任务)
        assert!(wheel.cancel(task_id1));
        assert!(wheel.task_index.get(task_id1.key()).is_none());

        // Cancel L1 task (取消 L1 任务)
        assert!(wheel.cancel(task_id2));
        assert!(wheel.task_index.get(task_id2.key()).is_none());

        assert!(wheel.is_empty()); // 时间轮应该为空
    }

    #[test]
    fn test_delay_to_ticks() {
        let wheel = Wheel::new(WheelConfig::default(), BatchConfig::default());
        assert_eq!(wheel.delay_to_ticks(Duration::from_millis(100)), 10);
        assert_eq!(wheel.delay_to_ticks(Duration::from_millis(50)), 5);
        assert_eq!(wheel.delay_to_ticks(Duration::from_millis(1)), 1); // Minimum 1 tick (最小 1 个 tick)
    }

    #[test]
    fn test_wheel_invalid_slot_count() {
        let result = WheelConfig::builder().l0_slot_count(100).build();
        assert!(result.is_err());
        if let Err(crate::error::TimerError::InvalidSlotCount { slot_count, reason }) = result {
            assert_eq!(slot_count, 100);
            assert_eq!(reason, "L0 layer slot count must be power of 2"); // L0 层槽数量必须是 2 的幂
        } else {
            panic!("Expected InvalidSlotCount error"); // 期望 InvalidSlotCount 错误
        }
    }

    #[test]
    fn test_minimum_delay() {
        let mut wheel = Wheel::new(WheelConfig::default(), BatchConfig::default());

        // Test minimum delay (delays less than 1 tick should be rounded up to 1 tick) (测试最小延迟 (延迟小于 1 个 tick 应该向上舍入到 1 个 tick))
        let callback = CallbackWrapper::new(|| async {});
        let task = TimerTask::new_oneshot(Duration::from_millis(1), Some(callback));
        let (task_with_notifier, _completion_rx) =
            TimerTaskWithCompletionNotifier::from_timer_task(task);
        let handle = wheel.allocate_handle();
        let task_id: TaskId = handle.task_id();
        wheel.insert(handle, task_with_notifier);

        // Advance 1 tick, task should trigger (前进 1 个 tick，任务应该触发)
        let expired = wheel.advance();
        assert_eq!(
            expired.len(),
            1,
            "Minimum delay task should be triggered after 1 tick"
        ); // 最小延迟任务应该在 1 个 tick 后触发
        assert_eq!(expired[0].id, task_id);
    }

    #[test]
    fn test_advance_empty_slots() {
        let mut wheel = Wheel::new(WheelConfig::default(), BatchConfig::default());

        // Do not insert any tasks, advance multiple ticks (不插入任何任务，前进多个tick)
        for _ in 0..100 {
            let expired = wheel.advance();
            assert!(
                expired.is_empty(),
                "Empty slots should not return any tasks"
            );
        }

        assert_eq!(
            wheel.current_tick(),
            100,
            "current_tick should correctly increment"
        ); // current_tick 应该正确递增
    }

    #[test]
    fn test_slot_boundary() {
        let mut wheel = Wheel::new(WheelConfig::default(), BatchConfig::default());

        // Test slot boundary and wraparound (测试槽边界和环绕)
        // 第一个任务：延迟 10ms（1 tick），应该在 slot 1 触发
        let callback1 = CallbackWrapper::new(|| async {});
        let task1 = TimerTask::new_oneshot(Duration::from_millis(10), Some(callback1));
        let (task_with_notifier1, _rx1) = TimerTaskWithCompletionNotifier::from_timer_task(task1);
        let handle1 = wheel.allocate_handle();
        let task_id_1 = handle1.task_id();
        wheel.insert(handle1, task_with_notifier1);

        // Second task: delay 5110ms (511 ticks), should trigger on slot 511 (第二个任务：延迟 5110毫秒 (511个tick)，应该在槽 511 触发)
        let callback2 = CallbackWrapper::new(|| async {});
        let task2 = TimerTask::new_oneshot(Duration::from_millis(5110), Some(callback2));
        let (task_with_notifier2, _rx2) = TimerTaskWithCompletionNotifier::from_timer_task(task2);
        let handle2 = wheel.allocate_handle();
        let task_id_2 = handle2.task_id();
        wheel.insert(handle2, task_with_notifier2);

        // Advance 1 tick, first task should trigger (前进 1 个 tick，第一个任务应该触发)
        let expired = wheel.advance();
        assert_eq!(expired.len(), 1, "First task should trigger on tick 1"); // 第一个任务应该在第 1 个 tick 触发
        assert_eq!(expired[0].id, task_id_1);

        // Continue advancing to 511 ticks (from tick 1 to tick 511), second task should trigger (继续前进 511个tick (从tick 1到tick 511)，第二个任务应该触发)
        let mut triggered = false;
        for i in 0..510 {
            let expired = wheel.advance();
            if !expired.is_empty() {
                assert_eq!(
                    expired.len(),
                    1,
                    "The {}th advance should trigger the second task",
                    i + 2
                ); // 第 {i + 2} 次前进应该触发第二个任务
                assert_eq!(expired[0].id, task_id_2);
                triggered = true;
                break;
            }
        }
        assert!(triggered, "Second task should trigger on tick 511"); // 第二个任务应该在第 511 个 tick 触发

        assert!(wheel.is_empty(), "All tasks should have been triggered"); // 所有任务都应该被触发
    }

    #[test]
    fn test_task_id_uniqueness() {
        let mut wheel = Wheel::new(WheelConfig::default(), BatchConfig::default());

        // Insert multiple tasks, verify TaskId uniqueness (插入多个任务，验证 TaskId 唯一性)
        let mut task_ids = std::collections::HashSet::new();
        for _ in 0..100 {
            let callback = CallbackWrapper::new(|| async {});
            let task = TimerTask::new_oneshot(Duration::from_millis(100), Some(callback));
            let (task_with_notifier, _rx) = TimerTaskWithCompletionNotifier::from_timer_task(task);
            let handle = wheel.allocate_handle();
            let task_id = handle.task_id();
            wheel.insert(handle, task_with_notifier);

            assert!(task_ids.insert(task_id), "TaskId should be unique"); // TaskId 应该唯一
        }

        assert_eq!(task_ids.len(), 100);
    }
}
