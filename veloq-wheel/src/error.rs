use std::fmt;

/// Timer Error Type
///
/// 定时器错误类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimerError {
    /// Invalid slot count (must be a power of 2 and greater than 0)
    ///
    /// 无效的槽位数 (必须是 2 的幂且大于 0)
    InvalidSlotCount {
        slot_count: usize,
        reason: &'static str,
    },

    /// Configuration validation failed
    ///
    /// 配置验证失败
    InvalidConfiguration { field: String, reason: String },

    /// Registration failed, internal channel is full or closed
    ///
    /// 注册失败，内部通道已满或已关闭
    RegisterFailed,

    /// Batch operation failed: handles and tasks length mismatch
    ///
    /// 批量操作失败：handles 和 tasks 长度不匹配
    BatchLengthMismatch {
        handles_len: usize,
        tasks_len: usize,
    },
}

impl fmt::Display for TimerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimerError::InvalidSlotCount { slot_count, reason } => {
                write!(
                    f,
                    "Invalid slot count {}: {} (无效的槽位数 {}: {})",
                    slot_count, reason, slot_count, reason
                )
            }
            TimerError::InvalidConfiguration { field, reason } => {
                write!(
                    f,
                    "Configuration validation failed ({}): {} (配置验证失败 ({}): {})",
                    field, reason, field, reason
                )
            }
            TimerError::RegisterFailed => {
                write!(
                    f,
                    "Registration failed: internal channel is full or closed (注册失败: 内部通道已满或已关闭)"
                )
            }
            TimerError::BatchLengthMismatch {
                handles_len,
                tasks_len,
            } => {
                write!(
                    f,
                    "Batch operation failed: handles length ({}) does not match tasks length ({}) (批量操作失败: handles 长度 ({}) 与 tasks 长度 ({}) 不匹配)",
                    handles_len, tasks_len, handles_len, tasks_len
                )
            }
        }
    }
}

impl std::error::Error for TimerError {}
