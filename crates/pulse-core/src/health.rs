/// Beginner-friendly health classification for live and final telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Unknown,
    Healthy,
    Warning,
    Critical,
}

impl HealthStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "WAITING",
            Self::Healthy => "HEALTHY",
            Self::Warning => "DEGRADED",
            Self::Critical => "CRITICAL",
        }
    }

    /// Combine two statuses by taking the worse severity.
    pub fn worst(self, other: Self) -> Self {
        let rank = |s: Self| match s {
            Self::Unknown => 0,
            Self::Healthy => 1,
            Self::Warning => 2,
            Self::Critical => 3,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

#[derive(Debug, Clone)]
pub struct HealthVerdict {
    pub overall: HealthStatus,
    pub retransmits_status: HealthStatus,
    pub jitter_status: HealthStatus,
    pub loss_status: HealthStatus,
    pub summary_line: String,
}

pub fn evaluate_retransmits(retransmits: Option<u64>) -> HealthStatus {
    match retransmits {
        None => HealthStatus::Unknown,
        Some(0..=5) => HealthStatus::Healthy,
        Some(6..=50) => HealthStatus::Warning,
        Some(_) => HealthStatus::Critical,
    }
}

pub fn evaluate_jitter(jitter_ms: Option<f64>) -> HealthStatus {
    match jitter_ms {
        None => HealthStatus::Unknown,
        Some(j) if j < 5.0 => HealthStatus::Healthy,
        Some(j) if j <= 20.0 => HealthStatus::Warning,
        Some(_) => HealthStatus::Critical,
    }
}

pub fn evaluate_loss(loss_percent: Option<f64>) -> HealthStatus {
    match loss_percent {
        None => HealthStatus::Unknown,
        Some(l) if l <= 0.1 => HealthStatus::Healthy,
        Some(l) if l <= 1.0 => HealthStatus::Warning,
        Some(_) => HealthStatus::Critical,
    }
}

pub fn build_verdict(
    retransmits: Option<u64>,
    jitter_ms: Option<f64>,
    loss_percent: Option<f64>,
) -> HealthVerdict {
    let r = evaluate_retransmits(retransmits);
    let j = evaluate_jitter(jitter_ms);
    let l = evaluate_loss(loss_percent);
    let overall = r.worst(j).worst(l);

    let summary_line = match overall {
        HealthStatus::Unknown => "Waiting for telemetry...".into(),
        HealthStatus::Healthy => "Connection is stable with minimal packet loss or retries.".into(),
        HealthStatus::Warning => "Moderate congestion or jitter detected; calls/streams may stutter.".into(),
        HealthStatus::Critical => "Heavy packet loss or retransmits; check Wi-Fi signal or cabling.".into(),
    };

    HealthVerdict {
        overall,
        retransmits_status: r,
        jitter_status: j,
        loss_status: l,
        summary_line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_picks_worst_metric() {
        let v = build_verdict(Some(1), Some(25.0), Some(0.0));
        assert_eq!(v.retransmits_status, HealthStatus::Healthy);
        assert_eq!(v.jitter_status, HealthStatus::Critical);
        assert_eq!(v.overall, HealthStatus::Critical);
    }
}
