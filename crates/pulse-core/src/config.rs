use std::net::IpAddr;
use thiserror::Error;

/// Operating mode: Client initiates traffic; Server listens.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Client { host: String },
    Server { one_off: bool },
}

/// Transport protocol selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Protocol {
    Tcp { congestion: Option<CongestionAlgo> },
    Udp { target_bps: u64, burst_packets: Option<u32> },
    Sctp,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CongestionAlgo {
    Cubic,
    Bbr,
    Reno,
}

impl CongestionAlgo {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cubic => "cubic",
            Self::Bbr => "bbr",
            Self::Reno => "reno",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    Upload,
    ReverseDownload,
    Bidirectional,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopCondition {
    Seconds(u32),
    Bytes(u64),
    Blocks(u64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Preset {
    MaxSpeed,
    VoipGaming,
    WifiRoaming,
}

/// Unified test configuration with full iperf3 parameter parity.
#[derive(Debug, Clone, PartialEq)]
pub struct TestConfig {
    pub mode: Mode,
    pub port: u16,
    pub bind_addr: Option<IpAddr>,
    pub protocol: Protocol,
    pub direction: Direction,
    pub parallel_streams: u8,
    pub window_bytes: Option<u64>,
    pub mss: Option<u16>,
    pub dscp: Option<String>,
    pub stop: StopCondition,
    pub omit_seconds: u32,
    pub interval_secs: f32,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self::from_preset(Preset::MaxSpeed, "127.0.0.1".into())
    }
}

impl TestConfig {
    /// Constructs a beginner-friendly preset configuration.
    pub fn from_preset(preset: Preset, host: String) -> Self {
        match preset {
            Preset::MaxSpeed => Self {
                mode: Mode::Client { host },
                port: 5201,
                bind_addr: None,
                protocol: Protocol::Tcp { congestion: None },
                direction: Direction::ReverseDownload,
                parallel_streams: 4,
                window_bytes: None,
                mss: None,
                dscp: None,
                stop: StopCondition::Seconds(10),
                omit_seconds: 2,
                interval_secs: 1.0,
            },
            Preset::VoipGaming => Self {
                mode: Mode::Client { host },
                port: 5201,
                bind_addr: None,
                protocol: Protocol::Udp {
                    target_bps: 10_000_000,
                    burst_packets: None,
                },
                direction: Direction::Bidirectional,
                parallel_streams: 1,
                window_bytes: None,
                mss: None,
                dscp: Some("EF".into()),
                stop: StopCondition::Seconds(15),
                omit_seconds: 0,
                interval_secs: 1.0,
            },
            Preset::WifiRoaming => Self {
                mode: Mode::Client { host },
                port: 5201,
                bind_addr: None,
                protocol: Protocol::Tcp { congestion: None },
                direction: Direction::ReverseDownload,
                parallel_streams: 1,
                window_bytes: None,
                mss: None,
                dscp: None,
                stop: StopCondition::Seconds(30),
                omit_seconds: 0,
                interval_secs: 0.5,
            },
        }
    }

    /// Validates configuration invariants before spawning iperf3.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.port == 0 {
            return Err(ConfigError::InvalidPort);
        }
        if let Mode::Client { host } = &self.mode {
            if host.trim().is_empty() {
                return Err(ConfigError::EmptyHost);
            }
        } else if self.direction == Direction::Bidirectional {
            return Err(ConfigError::BidirInServerMode);
        }
        if !(1..=128).contains(&self.parallel_streams) {
            return Err(ConfigError::InvalidStreams);
        }
        if self.interval_secs <= 0.0 || self.interval_secs > 60.0 {
            return Err(ConfigError::InvalidInterval);
        }
        match self.stop {
            StopCondition::Seconds(0) => return Err(ConfigError::ZeroDuration),
            StopCondition::Seconds(sec) if self.omit_seconds >= sec => {
                return Err(ConfigError::OmitExceedsDuration(self.omit_seconds, sec));
            }
            StopCondition::Bytes(0) | StopCondition::Blocks(0) => {
                return Err(ConfigError::ZeroDuration);
            }
            _ => {}
        }
        if matches!(self.protocol, Protocol::Tcp { congestion: Some(_) })
            && !cfg!(target_os = "linux")
        {
            return Err(ConfigError::CongestionUnsupported);
        }
        Ok(())
    }

    /// Builds exact iperf3 CLI arguments with streaming JSON enabled.
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut args = vec![
            "-p".into(),
            self.port.to_string(),
            "-i".into(),
            format!("{:.2}", self.interval_secs),
            "--json".into(),
            "--json-stream".into(),
            "--forceflush".into(),
        ];

        if let Some(ip) = self.bind_addr {
            args.extend(["-B".into(), ip.to_string()]);
        }

        match &self.mode {
            Mode::Server { one_off } => {
                args.push("-s".into());
                if *one_off {
                    args.push("-1".into());
                }
                return args;
            }
            Mode::Client { host } => {
                args.extend(["-c".into(), host.trim().to_string()]);
            }
        }

        match self.direction {
            Direction::Upload => {}
            Direction::ReverseDownload => args.push("-R".into()),
            Direction::Bidirectional => args.push("--bidir".into()),
        }

        match self.protocol {
            Protocol::Tcp { congestion } => {
                if let Some(algo) = congestion {
                    args.extend(["-C".into(), algo.as_str().into()]);
                }
            }
            Protocol::Udp {
                target_bps,
                burst_packets,
            } => {
                args.push("-u".into());
                let bw = match burst_packets {
                    Some(b) => format!("{target_bps}/{b}"),
                    None => target_bps.to_string(),
                };
                args.extend(["-b".into(), bw]);
            }
            Protocol::Sctp => args.push("--sctp".into()),
        }

        if self.parallel_streams > 1 {
            args.extend(["-P".into(), self.parallel_streams.to_string()]);
        }
        if let Some(w) = self.window_bytes {
            args.extend(["-w".into(), w.to_string()]);
        }
        if let Some(m) = self.mss {
            args.extend(["-M".into(), m.to_string()]);
        }
        if let Some(d) = &self.dscp {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                args.extend(["--dscp".into(), trimmed.to_string()]);
            }
        }
        if self.omit_seconds > 0 {
            args.extend(["-O".into(), self.omit_seconds.to_string()]);
        }

        match self.stop {
            StopCondition::Seconds(s) => args.extend(["-t".into(), s.to_string()]),
            StopCondition::Bytes(n) => args.extend(["-n".into(), n.to_string()]),
            StopCondition::Blocks(k) => args.extend(["-k".into(), k.to_string()]),
        }

        args
    }

    pub fn to_cli_string(&self) -> String {
        format!("iperf3 {}", self.to_cli_args().join(" "))
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum ConfigError {
    #[error("Target host cannot be empty")]
    EmptyHost,
    #[error("Port must be between 1 and 65535")]
    InvalidPort,
    #[error("Parallel streams must be 1..=128")]
    InvalidStreams,
    #[error("Interval must be > 0 and <= 60 seconds")]
    InvalidInterval,
    #[error("Duration/limit must be greater than 0")]
    ZeroDuration,
    #[error("Omit warm-up ({0}s) must be less than test duration ({1}s)")]
    OmitExceedsDuration(u32, u32),
    #[error("Congestion control (-C) is only supported on Linux TCP tests")]
    CongestionUnsupported,
    #[error("Bidirectional mode requires Client mode")]
    BidirInServerMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_max_speed_generates_expected_flags() {
        let cfg = TestConfig::from_preset(Preset::MaxSpeed, "10.0.0.5".into());
        assert!(cfg.validate().is_ok());
        let args = cfg.to_cli_args();
        assert!(args.contains(&"-c".into()));
        assert!(args.contains(&"10.0.0.5".into()));
        assert!(args.contains(&"-R".into()));
        assert!(args.contains(&"-P".into()));
        assert!(args.contains(&"4".into()));
        assert!(args.contains(&"-O".into()));
        assert!(args.contains(&"2".into()));
        assert!(args.contains(&"--json-stream".into()));
    }

    #[test]
    fn empty_host_fails_validation() {
        let cfg = TestConfig::from_preset(Preset::MaxSpeed, "   ".into());
        assert_eq!(cfg.validate(), Err(ConfigError::EmptyHost));
    }

    #[test]
    fn omit_ge_duration_fails_validation() {
        let mut cfg = TestConfig::default();
        cfg.stop = StopCondition::Seconds(5);
        cfg.omit_seconds = 5;
        assert_eq!(cfg.validate(), Err(ConfigError::OmitExceedsDuration(5, 5)));
    }
}
