use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;

use deferred_map::{DefaultKey, Key};
use lite_sync::oneshot::lite::{Receiver, Sender, State, channel};
use lite_sync::spsc::{self, TryRecvError};

/// One-shot task completion state constants
///
/// 一次性任务完成状态常量
const ONESHOT_PENDING: u8 = 0;
const ONESHOT_CALLED: u8 = 1;
const ONESHOT_CANCELLED: u8 = 2;
const ONESHOT_CLOSED: u8 = 3;

/// Task Completion Reason for Periodic Tasks
///
/// Indicates the reason for task completion, called or cancelled.
///
/// 任务完成原因，调用或取消。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCompletion {
    /// Task was called
    ///
    /// 任务被调用
    Called,
    /// Task was cancelled
    ///
    /// 任务被取消
    Cancelled,
}

impl State for TaskCompletion {
    #[inline]
    fn to_u8(&self) -> u8 {
        match self {
            TaskCompletion::Called => ONESHOT_CALLED,
            TaskCompletion::Cancelled => ONESHOT_CANCELLED,
        }
    }

    #[inline]
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            ONESHOT_CALLED => Some(TaskCompletion::Called),
            ONESHOT_CANCELLED => Some(TaskCompletion::Cancelled),
            _ => None,
        }
    }

    #[inline]
    fn pending_value() -> u8 {
        ONESHOT_PENDING
    }

    #[inline]
    fn closed_value() -> u8 {
        ONESHOT_CLOSED
    }
}

/// Unique identifier for timer tasks
///
/// Now wraps a DeferredMap key (DefaultKey) which includes generation information
/// for safe reference and prevention of use-after-free.
///
/// 定时器任务唯一标识符
///
/// 现在封装 DeferredMap key (DefaultKey)，包含代数信息以实现安全引用和防止释放后使用
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(DefaultKey);

impl TaskId {
    /// Create TaskId from DeferredMap key (internal use)
    ///
    /// 从 DeferredMap key 创建 TaskId (内部使用)
    #[inline]
    pub(crate) fn from_key(key: DefaultKey) -> Self {
        TaskId(key)
    }

    /// Get the DeferredMap key
    ///
    /// 获取 DeferredMap key
    #[inline]
    pub(crate) fn key(&self) -> DefaultKey {
        self.0
    }

    /// Get the numeric value of the task ID
    ///
    /// 获取任务 ID 的数值
    #[inline]
    pub fn raw(&self) -> u64 {
        self.0.raw()
    }
}

pub struct TaskHandle {
    handle: deferred_map::Handle,
}

impl TaskHandle {
    /// Create a new task handle
    ///
    /// 创建一个新的任务句柄
    #[inline]
    pub(crate) fn new(handle: deferred_map::Handle) -> Self {
        Self { handle }
    }

    /// Get the task ID
    ///
    /// 获取任务 ID
    #[inline]
    pub fn task_id(&self) -> TaskId {
        TaskId::from_key(self.handle.key())
    }

    /// Convert to deferred map handle
    ///
    /// 转换为 deferred map 句柄
    #[inline]
    pub(crate) fn into_handle(self) -> deferred_map::Handle {
        self.handle
    }
}

/// Timer Callback Trait
///
/// Types implementing this trait can be used as timer callbacks.
///
/// 可实现此特性的类型可以作为定时器回调函数。
///
/// # Examples (示例)
///
/// ```
/// use kestrel_timer::task::TimerCallback;
/// use std::future::Future;
/// use std::pin::Pin;
///
/// struct MyCallback;
///
/// impl TimerCallback for MyCallback {
///     fn call(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
///         Box::pin(async {
///             println!("Timer callback executed!");
///         })
///     }
/// }
/// ```
pub trait TimerCallback: Send + Sync + 'static {
    /// Execute callback, returns a Future
    ///
    /// 执行回调函数，返回一个 Future
    fn call(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Implement TimerCallback trait for closures
///
/// Supports Fn() -> Future closures, can be called multiple times, suitable for periodic tasks
///
/// 实现 TimerCallback 特性的类型，支持 Fn() -> Future 闭包，可以多次调用，适合周期性任务
impl<F, Fut> TimerCallback for F
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn call(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(self())
    }
}

/// Callback wrapper for standardized callback creation and management
///
/// Callback 包装器，用于标准化回调创建和管理
///
/// # Examples (示例)
///
/// ```
/// use kestrel_timer::CallbackWrapper;
///
/// let callback = CallbackWrapper::new(|| async {
///     println!("Timer callback executed!");
/// });
/// ```
#[derive(Clone)]
pub struct CallbackWrapper {
    callback: Arc<dyn TimerCallback>,
}

impl CallbackWrapper {
    /// Create a new callback wrapper
    ///
    /// # Parameters
    /// - `callback`: Callback object implementing TimerCallback trait
    ///
    /// # 创建一个新的回调包装器
    ///
    /// # 参数
    /// - `callback`: 实现 TimerCallback 特性的回调对象
    ///
    /// # Examples (示例)
    ///
    /// ```
    /// use kestrel_timer::CallbackWrapper;
    ///
    /// let callback = CallbackWrapper::new(|| async {
    ///     println!("Timer fired!"); // 定时器触发
    /// });
    /// ```
    #[inline]
    pub fn new(callback: impl TimerCallback) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    /// Call the callback function
    ///
    /// 调用回调函数
    #[inline]
    pub fn call(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.callback.call()
    }
}

/// Task type enum to distinguish between one-shot and periodic timers
///
/// 任务类型枚举，用于区分一次性和周期性定时器
#[derive(Clone)]
pub enum TaskType {
    /// One-shot timer: executes once and completes
    ///
    /// 一次性定时器：执行一次后完成
    OneShot,

    /// Periodic timer: repeats at fixed intervals
    ///
    /// 周期性定时器：按固定间隔重复执行
    Periodic {
        /// Period interval for periodic tasks
        ///
        /// 周期任务的间隔时间
        interval: std::time::Duration,
        /// Buffer size for periodic task completion notifier
        ///
        /// 周期性任务完成通知器的缓冲区大小
        buffer_size: NonZeroUsize,
    },
}

/// Task type enum to distinguish between one-shot and periodic timers
///
/// 任务类型枚举，用于区分一次性和周期性定时器
pub enum TaskTypeWithCompletionNotifier {
    /// One-shot timer: executes once and completes
    ///
    /// 一次性定时器：执行一次后完成
    OneShot {
        completion_notifier: Sender<TaskCompletion>,
    },

    /// Periodic timer: repeats at fixed intervals
    ///
    /// 周期性定时器：按固定间隔重复执行
    Periodic {
        /// Period interval for periodic tasks
        ///
        /// 周期任务的间隔时间
        interval: std::time::Duration,
        /// Completion notifier for periodic tasks
        ///
        /// 周期性任务完成通知器
        completion_notifier: PeriodicCompletionNotifier,
    },
}

impl TaskTypeWithCompletionNotifier {
    /// Get the interval for periodic tasks
    ///
    /// Returns `None` for one-shot tasks
    ///
    /// 获取周期任务的间隔时间
    ///
    /// 对于一次性任务返回 `None`
    #[inline]
    pub fn get_interval(&self) -> Option<std::time::Duration> {
        match self {
            TaskTypeWithCompletionNotifier::Periodic { interval, .. } => Some(*interval),
            TaskTypeWithCompletionNotifier::OneShot { .. } => None,
        }
    }
}

/// Completion notifier for periodic tasks
///
/// Uses custom SPSC channel for high-performance, low-latency notification
///
/// 周期任务完成通知器
///
/// 使用自定义 SPSC 通道实现高性能、低延迟的通知
pub struct PeriodicCompletionNotifier(pub spsc::Sender<TaskCompletion, 32>);

/// Completion receiver for periodic tasks
///
/// 周期任务完成通知接收器
pub struct PeriodicCompletionReceiver(pub spsc::Receiver<TaskCompletion, 32>);

impl PeriodicCompletionReceiver {
    /// Try to receive a completion notification
    ///
    /// 尝试接收完成通知
    #[inline]
    pub fn try_recv(&mut self) -> Result<TaskCompletion, TryRecvError> {
        self.0.try_recv()
    }

    /// Receive a completion notification
    ///
    /// 接收完成通知
    #[inline]
    pub async fn recv(&mut self) -> Option<TaskCompletion> {
        self.0.recv().await
    }
}

/// Completion notifier for one-shot tasks
///
/// 一次性任务完成通知器
pub enum CompletionNotifier {
    OneShot(Sender<TaskCompletion>),
    Periodic(PeriodicCompletionNotifier),
}

/// Completion receiver for one-shot and periodic tasks
///
/// 一次性和周期任务完成通知接收器
pub enum CompletionReceiver {
    OneShot(Receiver<TaskCompletion>),
    Periodic(PeriodicCompletionReceiver),
}

/// Timer Task
///
/// Users interact via a two-step API
/// 1. Create task using `TimerTask::new_oneshot()` or `TimerTask::new_periodic()`
/// 2. Register task using `TimerWheel::register()` or `TimerService::register()`
///
/// TaskId is assigned when the task is inserted into the timing wheel.
///
/// 定时器任务
///
/// 用户通过两步 API 与定时器交互
/// 1. 使用 `TimerTask::new_oneshot()` 或 `TimerTask::new_periodic()` 创建任务
/// 2. 使用 `TimerWheel::register()` 或 `TimerService::register()` 注册任务
///
/// TaskId 在任务插入到时间轮时分配
pub struct TimerTask {
    /// Task type (one-shot or periodic)
    ///
    /// 任务类型（一次性或周期性）
    pub(crate) task_type: TaskType,

    /// User-specified delay duration (initial delay for periodic tasks)
    ///
    /// 用户指定的延迟时间（周期任务的初始延迟）
    pub(crate) delay: std::time::Duration,

    /// Async callback function, optional
    ///
    /// 异步回调函数，可选
    pub(crate) callback: Option<CallbackWrapper>,
}

impl TimerTask {
    /// Create a new one-shot timer task
    ///
    /// # Parameters
    /// - `delay`: Delay duration before task execution
    /// - `callback`: Callback function, optional
    ///
    /// # Note
    /// TaskId will be assigned when the task is inserted into the timing wheel.
    ///
    /// 创建一个新的一次性定时器任务
    ///
    /// # 参数
    /// - `delay`: 任务执行前的延迟时间
    /// - `callback`: 回调函数，可选
    ///
    /// # 注意
    /// TaskId 将在任务插入到时间轮时分配
    #[inline]
    pub fn new_oneshot(delay: std::time::Duration, callback: Option<CallbackWrapper>) -> Self {
        Self {
            task_type: TaskType::OneShot,
            delay,
            callback,
        }
    }

    /// Create a new periodic timer task
    ///
    /// # Parameters
    /// - `initial_delay`: Initial delay before first execution
    /// - `interval`: Interval between subsequent executions
    /// - `callback`: Callback function, optional
    ///
    /// # Note
    /// TaskId will be assigned when the task is inserted into the timing wheel.
    ///
    /// 创建一个新的周期性定时器任务
    ///
    /// # 参数
    /// - `initial_delay`: 首次执行前的初始延迟
    /// - `interval`: 后续执行之间的间隔
    /// - `callback`: 回调函数，可选
    ///
    /// # 注意
    /// TaskId 将在任务插入到时间轮时分配
    #[inline]
    pub fn new_periodic(
        initial_delay: std::time::Duration,
        interval: std::time::Duration,
        callback: Option<CallbackWrapper>,
        buffer_size: Option<NonZeroUsize>,
    ) -> Self {
        Self {
            task_type: TaskType::Periodic {
                interval,
                buffer_size: buffer_size.unwrap_or(NonZeroUsize::new(32).unwrap()),
            },
            delay: initial_delay,
            callback,
        }
    }

    /// Get task type
    ///
    /// 获取任务类型
    #[inline]
    pub fn get_task_type(&self) -> &TaskType {
        &self.task_type
    }

    /// Get the interval for periodic tasks
    ///
    /// Returns `None` for one-shot tasks
    ///
    /// 获取周期任务的间隔时间
    ///
    /// 对于一次性任务返回 `None`
    #[inline]
    pub fn get_interval(&self) -> Option<std::time::Duration> {
        match self.task_type {
            TaskType::Periodic { interval, .. } => Some(interval),
            TaskType::OneShot => None,
        }
    }
}

/// Timer Task
///
/// Users interact via a two-step API
/// 1. Create task using `TimerTask::new_oneshot()` or `TimerTask::new_periodic()`
/// 2. Register task using `TimerWheel::register()` or `TimerService::register()`
///
/// 定时器任务
///
/// 用户通过两步 API 与定时器交互
/// 1. 使用 `TimerTask::new_oneshot()` 或 `TimerTask::new_periodic()` 创建任务
/// 2. 使用 `TimerWheel::register()` 或 `TimerService::register()` 注册任务
pub struct TimerTaskWithCompletionNotifier {
    /// Task type (one-shot or periodic)
    ///
    /// 任务类型（一次性或周期性）
    pub(crate) task_type: TaskTypeWithCompletionNotifier,

    /// User-specified delay duration (initial delay for periodic tasks)
    ///
    /// 用户指定的延迟时间（周期任务的初始延迟）
    pub(crate) delay: std::time::Duration,

    /// Async callback function, optional
    ///
    /// 异步回调函数，可选
    pub(crate) callback: Option<CallbackWrapper>,
}

impl TimerTaskWithCompletionNotifier {
    /// Create a new timer task with completion notifier from a timer task
    ///
    /// TaskId will be assigned later when inserted into the timing wheel
    ///
    /// 从定时器任务创建一个新的定时器任务完成通知器
    ///
    /// TaskId 将在插入到时间轮时分配
    ///
    /// # Parameters
    /// - `task`: The timer task to create from
    ///
    /// # Returns
    /// A tuple containing the new timer task with completion notifier and the completion receiver
    ///
    /// 返回一个包含新的定时器任务完成通知器和完成通知接收器的元组
    ///
    pub fn from_timer_task(task: TimerTask) -> (Self, CompletionReceiver) {
        match task.task_type {
            TaskType::OneShot => {
                // Create oneshot notifier and receiver with optimized single Arc allocation
                // 创建 oneshot 通知器和接收器，使用优化的单个 Arc 分配
                let (notifier, receiver) = channel();

                (
                    Self {
                        task_type: TaskTypeWithCompletionNotifier::OneShot {
                            completion_notifier: notifier,
                        },
                        delay: task.delay,
                        callback: task.callback,
                    },
                    CompletionReceiver::OneShot(receiver),
                )
            }
            TaskType::Periodic {
                interval,
                buffer_size,
            } => {
                // Use custom SPSC channel for high-performance periodic notification
                // 使用自定义 SPSC 通道实现高性能周期通知
                let (tx, rx) = spsc::channel(buffer_size);

                let notifier = PeriodicCompletionNotifier(tx);
                let receiver = PeriodicCompletionReceiver(rx);

                (
                    Self {
                        task_type: TaskTypeWithCompletionNotifier::Periodic {
                            interval,
                            completion_notifier: notifier,
                        },
                        delay: task.delay,
                        callback: task.callback,
                    },
                    CompletionReceiver::Periodic(receiver),
                )
            }
        }
    }

    /// Into task type
    ///
    /// 将任务类型转换为完成通知器
    #[inline]
    pub fn into_task_type(self) -> TaskTypeWithCompletionNotifier {
        self.task_type
    }

    /// Get the interval for periodic tasks
    ///
    /// Returns `None` for one-shot tasks
    ///
    /// 获取周期任务的间隔时间
    ///
    /// 对于一次性任务返回 `None`
    #[inline]
    pub fn get_interval(&self) -> Option<std::time::Duration> {
        self.task_type.get_interval()
    }
}

pub(crate) struct TimerTaskForWheel {
    pub(crate) task_id: TaskId,
    pub(crate) task: TimerTaskWithCompletionNotifier,
    pub(crate) deadline_tick: u64,
    pub(crate) rounds: u32,
}

impl TimerTaskForWheel {
    /// Create a new timer task for wheel with assigned TaskId
    ///
    /// 使用分配的 TaskId 创建一个新的定时器任务用于时间轮
    ///
    /// # Parameters
    /// - `task_id`: The TaskId assigned by DeferredMap
    /// - `task`: The timer task to create from
    /// - `deadline_tick`: The deadline tick for the task
    /// - `rounds`: The rounds for the task
    ///
    /// # Returns
    /// A new timer task for wheel
    ///
    /// 返回一个新的定时器任务用于时间轮
    ///
    #[inline]
    pub(crate) fn new_with_id(
        task_id: TaskId,
        task: TimerTaskWithCompletionNotifier,
        deadline_tick: u64,
        rounds: u32,
    ) -> Self {
        Self {
            task_id,
            task,
            deadline_tick,
            rounds,
        }
    }

    /// Get task ID
    ///
    /// 获取任务 ID
    #[inline]
    pub fn get_id(&self) -> TaskId {
        self.task_id
    }

    /// Into task type
    ///
    /// 将任务类型转换为完成通知器
    #[inline]
    pub fn into_task_type(self) -> TaskTypeWithCompletionNotifier {
        self.task.into_task_type()
    }

    /// Update the delay of the timer task
    ///
    /// 更新定时器任务的延迟
    ///
    /// # Parameters
    /// - `delay`: The new delay for the task
    ///
    #[inline]
    pub fn update_delay(&mut self, delay: std::time::Duration) {
        self.task.delay = delay
    }

    /// Update the callback of the timer task
    ///
    /// 更新定时器任务的回调
    ///
    /// # Parameters
    /// - `callback`: The new callback for the task
    ///
    #[inline]
    pub fn update_callback(&mut self, callback: CallbackWrapper) {
        self.task.callback = Some(callback)
    }
}

/// Task location information (including level) for hierarchical timing wheel
///
/// Memory layout optimization: level field placed first to reduce padding via struct alignment
///
/// 任务位置信息（包括层级）用于分层时间轮
///
/// 内存布局优化：将 level 字段放在第一位，通过结构体对齐来减少填充
#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskLocation {
    /// Slot index
    ///
    /// 槽索引
    pub slot_index: usize,
    /// Index position of task in slot Vec for O(1) cancellation
    ///
    /// 槽向量中任务的索引位置，用于 O(1) 取消
    pub vec_index: usize,
    /// Level: 0 = L0 (bottom layer), 1 = L1 (upper layer)
    /// Using u8 instead of bool to reserve space for potential multi-layer expansion
    ///
    /// 层级：0 = L0（底层），1 = L1（上层）
    /// 使用 u8 而不是 bool 来保留空间，用于潜在的多层扩展
    pub level: u8,
}

impl TaskLocation {
    /// Create a new task location information
    ///
    /// 创建一个新的任务位置信息
    #[inline(always)]
    pub fn new(level: u8, slot_index: usize, vec_index: usize) -> Self {
        Self {
            slot_index,
            vec_index,
            level,
        }
    }
}
