//! Timer Configuration Module
//!
//! Provides hierarchical configuration structure and Builder pattern for configuring timing wheel, service, and batch processing behavior.
//!
//! 定时器配置模块，提供分层的配置结构和 Builder 模式，用于配置时间轮、服务和批处理行为。
use crate::error::TimerError;
use std::num::NonZeroUsize;
use std::time::Duration;

/// Timing Wheel Configuration
///
/// Used to configure parameters for hierarchical timing wheel. The system only supports hierarchical mode.
///
/// # 时间轮配置
///
/// 用于配置分层时间轮的参数。系统只支持分层模式。
///
/// # Examples (示例)
/// ```no_run
/// use kestrel_timer::config::WheelConfig;
/// use std::time::Duration;
///
/// // Use default configuration (使用默认配置，分层模式)
/// let config = WheelConfig::default();
///
/// // Use Builder to customize configuration (使用 Builder 自定义配置)
/// let config = WheelConfig::builder()
///     .l0_tick_duration(Duration::from_millis(20))
///     .l0_slot_count(1024)
///     .l1_tick_duration(Duration::from_secs(2))
///     .l1_slot_count(128)
///     .build()
///     .unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct WheelConfig {
    /// Duration of each tick in L0 layer, bottom layer
    ///
    /// L0 层每个 tick 的持续时间
    pub l0_tick_duration: Duration,

    /// Number of slots in L0 layer, must be power of 2
    ///
    /// L0 层槽位数，必须是 2 的幂
    pub l0_slot_count: usize,

    /// Duration of each tick in L1 layer, upper layer
    ///
    /// L1 层每个 tick 的持续时间
    pub l1_tick_duration: Duration,

    /// Number of slots in L1 layer, must be power of 2
    ///
    /// L1 层槽位数，必须是 2 的幂
    pub l1_slot_count: usize,
}

impl Default for WheelConfig {
    fn default() -> Self {
        Self {
            l0_tick_duration: Duration::from_millis(10),
            l0_slot_count: 512,
            l1_tick_duration: Duration::from_secs(1),
            l1_slot_count: 64,
        }
    }
}

impl WheelConfig {
    /// Create configuration builder (创建配置构建器)
    pub fn builder() -> WheelConfigBuilder {
        WheelConfigBuilder::default()
    }
}

/// Timing Wheel Configuration Builder
#[derive(Debug, Clone)]
pub struct WheelConfigBuilder {
    l0_tick_duration: Duration,
    l0_slot_count: usize,
    l1_tick_duration: Duration,
    l1_slot_count: usize,
}

impl Default for WheelConfigBuilder {
    fn default() -> Self {
        Self {
            l0_tick_duration: Duration::from_millis(10),
            l0_slot_count: 512,
            l1_tick_duration: Duration::from_secs(1),
            l1_slot_count: 64,
        }
    }
}

impl WheelConfigBuilder {
    /// Set L0 layer tick duration
    pub fn l0_tick_duration(mut self, duration: Duration) -> Self {
        self.l0_tick_duration = duration;
        self
    }

    /// Set L0 layer slot count
    pub fn l0_slot_count(mut self, count: usize) -> Self {
        self.l0_slot_count = count;
        self
    }

    /// Set L1 layer tick duration
    pub fn l1_tick_duration(mut self, duration: Duration) -> Self {
        self.l1_tick_duration = duration;
        self
    }

    /// Set L1 layer slot count
    pub fn l1_slot_count(mut self, count: usize) -> Self {
        self.l1_slot_count = count;
        self
    }

    /// Build and validate configuration
    ///
    /// # Returns
    /// - `Ok(WheelConfig)`: Configuration is valid
    /// - `Err(TimerError)`: Configuration validation failed
    ///
    /// # Validation Rules
    /// - L0 tick duration must be greater than 0
    /// - L1 tick duration must be greater than 0
    /// - L0 slot count must be greater than 0 and power of 2
    /// - L1 slot count must be greater than 0 and power of 2
    /// - L1 tick must be an integer multiple of L0 tick
    pub fn build(self) -> Result<WheelConfig, TimerError> {
        // Validate L0 layer configuration
        if self.l0_tick_duration.is_zero() {
            return Err(TimerError::InvalidConfiguration {
                field: "l0_tick_duration".to_string(),
                reason: "L0 layer tick duration must be greater than 0".to_string(),
            });
        }

        if self.l0_slot_count == 0 {
            return Err(TimerError::InvalidSlotCount {
                slot_count: self.l0_slot_count,
                reason: "L0 layer slot count must be greater than 0",
            });
        }

        if !self.l0_slot_count.is_power_of_two() {
            return Err(TimerError::InvalidSlotCount {
                slot_count: self.l0_slot_count,
                reason: "L0 layer slot count must be power of 2",
            });
        }

        // Validate L1 layer configuration
        if self.l1_tick_duration.is_zero() {
            return Err(TimerError::InvalidConfiguration {
                field: "l1_tick_duration".to_string(),
                reason: "L1 layer tick duration must be greater than 0".to_string(),
            });
        }

        if self.l1_slot_count == 0 {
            return Err(TimerError::InvalidSlotCount {
                slot_count: self.l1_slot_count,
                reason: "L1 layer slot count must be greater than 0",
            });
        }

        if !self.l1_slot_count.is_power_of_two() {
            return Err(TimerError::InvalidSlotCount {
                slot_count: self.l1_slot_count,
                reason: "L1 layer slot count must be power of 2",
            });
        }

        // Validate L1 tick is an integer multiple of L0 tick
        let l0_ms = self.l0_tick_duration.as_millis() as u64;
        let l1_ms = self.l1_tick_duration.as_millis() as u64;
        if !l1_ms.is_multiple_of(l0_ms) {
            return Err(TimerError::InvalidConfiguration {
                field: "l1_tick_duration".to_string(),
                reason: format!(
                    "L1 tick duration ({} ms) must be an integer multiple of L0 tick duration ({} ms)",
                    l1_ms, l0_ms
                ),
            });
        }

        Ok(WheelConfig {
            l0_tick_duration: self.l0_tick_duration,
            l0_slot_count: self.l0_slot_count,
            l1_tick_duration: self.l1_tick_duration,
            l1_slot_count: self.l1_slot_count,
        })
    }
}

/// Service Configuration
///
/// Used to configure channel capacities for TimerService.
///
/// # 服务配置
///
/// 用于配置 TimerService 的通道容量。
///
/// # Examples (示例)
/// ```no_run
/// use kestrel_timer::config::ServiceConfig;
/// use std::num::NonZeroUsize;
///
/// // Use default configuration (使用默认配置)
/// let config = ServiceConfig::default();
///
/// // Use Builder to customize configuration (使用 Builder 自定义配置)
/// let config = ServiceConfig::builder()
///     .command_channel_capacity(NonZeroUsize::new(1024).unwrap())
///     .timeout_channel_capacity(NonZeroUsize::new(2000).unwrap())
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Command channel capacity
    ///
    /// 命令通道容量
    pub command_channel_capacity: NonZeroUsize,

    /// Timeout channel capacity
    ///
    /// 超时通道容量
    pub timeout_channel_capacity: NonZeroUsize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            command_channel_capacity: NonZeroUsize::new(512).unwrap(),
            timeout_channel_capacity: NonZeroUsize::new(1000).unwrap(),
        }
    }
}

impl ServiceConfig {
    /// Create configuration builder (创建配置构建器)
    pub fn builder() -> ServiceConfigBuilder {
        ServiceConfigBuilder::default()
    }
}

/// Service Configuration Builder
///
/// 用于构建 ServiceConfig 的构建器。
#[derive(Debug, Clone)]
pub struct ServiceConfigBuilder {
    /// Command channel capacity
    ///
    /// 命令通道容量
    pub command_channel_capacity: NonZeroUsize,

    /// Timeout channel capacity
    ///
    /// 超时通道容量
    pub timeout_channel_capacity: NonZeroUsize,
}

impl Default for ServiceConfigBuilder {
    fn default() -> Self {
        let config = ServiceConfig::default();
        Self {
            command_channel_capacity: config.command_channel_capacity,
            timeout_channel_capacity: config.timeout_channel_capacity,
        }
    }
}

impl ServiceConfigBuilder {
    /// Set command channel capacity (设置命令通道容量)
    pub fn command_channel_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.command_channel_capacity = capacity;
        self
    }

    /// Set timeout channel capacity (设置超时通道容量)
    pub fn timeout_channel_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.timeout_channel_capacity = capacity;
        self
    }

    /// Build and validate configuration
    ///
    /// # Returns
    /// - `Ok(ServiceConfig)`: Configuration is valid
    /// - `Err(TimerError)`: Configuration validation failed
    ///
    /// # Validation Rules
    /// - All channel capacities must be greater than 0
    ///
    /// 构建并验证配置
    ///
    /// # 返回值
    /// - `Ok(ServiceConfig)`: 配置有效
    /// - `Err(TimerError)`: 配置验证失败
    ///
    /// # 验证规则
    /// - 所有通道容量必须大于 0
    ///
    pub fn build(self) -> ServiceConfig {
        ServiceConfig {
            command_channel_capacity: self.command_channel_capacity,
            timeout_channel_capacity: self.timeout_channel_capacity,
        }
    }
}

/// Batch Processing Configuration
///
/// Used to configure optimization parameters for batch operations.
///
/// 用于配置批处理操作的优化参数。
///
/// # 批处理配置
///
/// 用于配置批处理操作的优化参数。
///
/// # Examples (示例)
/// ```no_run
/// use kestrel_timer::config::BatchConfig;
///
/// // Use default configuration (使用默认配置)
/// let config = BatchConfig::default();
///
/// // Custom configuration (使用自定义配置)
/// let config = BatchConfig {
///     small_batch_threshold: 20,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Small batch threshold, used for batch cancellation optimization
    /// When the number of tasks to be cancelled is less than or equal to this value, cancel individually without grouping and sorting
    ///
    /// 小批量阈值，用于批量取消操作的优化
    ///
    /// 当需要取消的任务数量小于或等于此值时，取消操作将单独进行，无需分组和排序
    pub small_batch_threshold: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            small_batch_threshold: 10,
        }
    }
}

/// Top-level Timer Configuration
///
/// Combines all sub-configurations to provide complete timer system configuration.
///
/// # 定时器配置
///
/// 用于组合所有子配置，提供完整的定时器系统配置。
///
/// # Examples (示例)
/// ```no_run
/// use kestrel_timer::config::TimerConfig;
/// use std::num::NonZeroUsize;
///
/// // Use default configuration (使用默认配置)
/// let config = TimerConfig::default();
///
/// // Use Builder to customize configuration, service parameters only
/// // (使用 Builder 自定义配置，仅配置服务参数)
/// let config = TimerConfig::builder()
///     .command_channel_capacity(NonZeroUsize::new(1024).unwrap())
///     .timeout_channel_capacity(NonZeroUsize::new(2000).unwrap())
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct TimerConfig {
    /// Timing wheel configuration
    pub wheel: WheelConfig,
    /// Service configuration
    pub service: ServiceConfig,
    /// Batch processing configuration
    pub batch: BatchConfig,
}

impl TimerConfig {
    /// Create configuration builder (创建配置构建器)
    pub fn builder() -> TimerConfigBuilder {
        TimerConfigBuilder::default()
    }
}

/// Top-level Timer Configuration Builder (顶级定时器配置构建器)
///
/// 用于构建 TimerConfig 的构建器。
#[derive(Debug, Default)]
pub struct TimerConfigBuilder {
    wheel_builder: WheelConfigBuilder,
    service_builder: ServiceConfigBuilder,
    batch_config: BatchConfig,
}

impl TimerConfigBuilder {
    /// Set command channel capacity (设置命令通道容量)
    pub fn command_channel_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.service_builder = self.service_builder.command_channel_capacity(capacity);
        self
    }

    /// Set timeout channel capacity (设置超时通道容量)
    pub fn timeout_channel_capacity(mut self, capacity: NonZeroUsize) -> Self {
        self.service_builder = self.service_builder.timeout_channel_capacity(capacity);
        self
    }

    /// Set small batch threshold (设置小批量阈值)
    pub fn small_batch_threshold(mut self, threshold: usize) -> Self {
        self.batch_config.small_batch_threshold = threshold;
        self
    }

    /// Build and validate configuration
    ///
    /// # Returns
    /// - `Ok(TimerConfig)`: Configuration is valid
    /// - `Err(TimerError)`: Configuration validation failed
    ///
    /// # 构建并验证配置
    ///
    /// # 返回值
    /// - `Ok(TimerConfig)`: 配置有效
    /// - `Err(TimerError)`: 配置验证失败
    ///
    pub fn build(self) -> Result<TimerConfig, TimerError> {
        Ok(TimerConfig {
            wheel: self.wheel_builder.build()?,
            service: self.service_builder.build(),
            batch: self.batch_config,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wheel_config_default() {
        let config = WheelConfig::default();
        assert_eq!(config.l0_tick_duration, Duration::from_millis(10));
        assert_eq!(config.l0_slot_count, 512);
        assert_eq!(config.l1_tick_duration, Duration::from_secs(1));
        assert_eq!(config.l1_slot_count, 64);
    }

    #[test]
    fn test_wheel_config_builder() {
        let config = WheelConfig::builder()
            .l0_tick_duration(Duration::from_millis(20))
            .l0_slot_count(1024)
            .l1_tick_duration(Duration::from_secs(2))
            .l1_slot_count(128)
            .build()
            .unwrap();

        assert_eq!(config.l0_tick_duration, Duration::from_millis(20));
        assert_eq!(config.l0_slot_count, 1024);
        assert_eq!(config.l1_tick_duration, Duration::from_secs(2));
        assert_eq!(config.l1_slot_count, 128);
    }

    #[test]
    fn test_wheel_config_validation_zero_tick() {
        let result = WheelConfig::builder()
            .l0_tick_duration(Duration::ZERO)
            .build();

        assert!(result.is_err());
    }

    #[test]
    fn test_wheel_config_validation_invalid_slot_count() {
        let result = WheelConfig::builder().l0_slot_count(100).build();

        assert!(result.is_err());
    }

    #[test]
    fn test_service_config_builder() {
        let config = ServiceConfig::builder()
            .command_channel_capacity(NonZeroUsize::new(1024).unwrap())
            .timeout_channel_capacity(NonZeroUsize::new(2000).unwrap())
            .build();

        assert_eq!(
            config.command_channel_capacity,
            NonZeroUsize::new(1024).unwrap()
        );
        assert_eq!(
            config.timeout_channel_capacity,
            NonZeroUsize::new(2000).unwrap()
        );
    }

    #[test]
    fn test_batch_config_default() {
        let config = BatchConfig::default();
        assert_eq!(config.small_batch_threshold, 10);
    }

    #[test]
    fn test_timer_config_default() {
        let config = TimerConfig::default();
        assert_eq!(config.wheel.l0_slot_count, 512);
        assert_eq!(
            config.service.command_channel_capacity,
            NonZeroUsize::new(512).unwrap()
        );
        assert_eq!(config.batch.small_batch_threshold, 10);
    }

    #[test]
    fn test_timer_config_builder() {
        let config = TimerConfig::builder()
            .command_channel_capacity(NonZeroUsize::new(1024).unwrap())
            .timeout_channel_capacity(NonZeroUsize::new(2000).unwrap())
            .small_batch_threshold(20)
            .build()
            .unwrap();

        assert_eq!(
            config.service.command_channel_capacity,
            NonZeroUsize::new(1024).unwrap()
        );
        assert_eq!(
            config.service.timeout_channel_capacity,
            NonZeroUsize::new(2000).unwrap()
        );
        assert_eq!(config.batch.small_batch_threshold, 20);
    }
}
