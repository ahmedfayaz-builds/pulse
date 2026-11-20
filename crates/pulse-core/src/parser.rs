use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

/// Streaming NDJSON event emitted by `iperf3 --json --json-stream`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "lowercase")]
pub enum IperfEvent {
    Start(StartData),
    Interval(IntervalData),
    End(EndData),
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartData {
    pub version: Option<String>,
    pub connecting_to: Option<Endpoint>,
    pub test_start: Option<TestStartInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestStartInfo {
    pub protocol: Option<String>,
    pub num_streams: Option<u32>,
    pub duration: Option<u32>,
    pub reverse: Option<i32>,
    pub bidir: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntervalData {
    #[serde(default)]
    pub streams: Vec<StreamTick>,
    pub sum: Option<SumTick>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamTick {
    pub socket: Option<i32>,
    pub start: f64,
    pub end: f64,
    pub bits_per_second: f64,
    pub retransmits: Option<u64>,
    pub snd_cwnd: Option<u64>,
    pub jitter_ms: Option<f64>,
    pub lost_packets: Option<u64>,
    pub packets: Option<u64>,
    pub omitted: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SumTick {
    pub start: f64,
    pub end: f64,
    pub bits_per_second: f64,
    pub retransmits: Option<u64>,
    pub jitter_ms: Option<f64>,
    pub lost_packets: Option<u64>,
    pub packets: Option<u64>,
    pub lost_percent: Option<f64>,
    pub omitted: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EndData {
    #[serde(default)]
    pub streams: Vec<EndStreamPair>,
    pub sum_sent: Option<SumTick>,
    pub sum_received: Option<SumTick>,
    pub sum: Option<SumTick>,
    pub cpu_utilization_percent: Option<CpuUtil>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EndStreamPair {
    pub sender: Option<EndStreamStats>,
    pub receiver: Option<EndStreamStats>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EndStreamStats {
    pub bits_per_second: Option<f64>,
    pub retransmits: Option<u64>,
    pub max_rtt: Option<u64>,
    pub min_rtt: Option<u64>,
    pub mean_rtt: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CpuUtil {
    pub host_total: f64,
    pub remote_total: f64,
}

#[derive(Debug, Clone)]
pub enum TelemetryMsg {
    Event(IperfEvent),
    RawLog(String),
    ErrorLog(String),
    Exited(Option<i32>),
}

/// Parses a single line of iperf3 stdout: NDJSON first, regex fallback for legacy/non-JSON output.
pub fn parse_stdout_line(line: &str) -> TelemetryMsg {
    let trimmed = line.trim();
    if trimmed.starts_with('{') {
        if let Ok(ev) = serde_json::from_str::<IperfEvent>(trimmed) {
            return TelemetryMsg::Event(ev);
        }
    }

    if let Some(tick) = parse_console_fallback(trimmed) {
        return TelemetryMsg::Event(IperfEvent::Interval(IntervalData {
            streams: vec![],
            sum: Some(tick),
        }));
    }

    TelemetryMsg::RawLog(line.to_string())
}

fn parse_console_fallback(line: &str) -> Option<SumTick> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"\[\s*(?:SUM|\d+)\]\s+(\d+\.\d+)-(\d+\.\d+)\s+sec\s+[\d.]+\s+\w+Bytes\s+([\d.]+)\s+([KMG]?bits/sec)(?:\s+(\d+))?",
        )
        .expect("valid fallback regex")
    });

    let caps = re.captures(line)?;
    let start: f64 = caps.get(1)?.as_str().parse().ok()?;
    let end: f64 = caps.get(2)?.as_str().parse().ok()?;
    let val: f64 = caps.get(3)?.as_str().parse().ok()?;
    let unit = caps.get(4)?.as_str();
    let mult = match unit {
        "Gbits/sec" => 1_000_000_000.0,
        "Mbits/sec" => 1_000_000.0,
        "Kbits/sec" => 1_000.0,
        _ => 1.0,
    };
    let retransmits = caps.get(5).and_then(|m| m.as_str().parse::<u64>().ok());

    Some(SumTick {
        start,
        end,
        bits_per_second: val * mult,
        retransmits,
        jitter_ms: None,
        lost_packets: None,
        packets: None,
        lost_percent: None,
        omitted: Some(line.contains("(omitted)")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_stream_interval() {
        let raw = r#"{"event":"interval","data":{"streams":[{"socket":5,"start":0.0,"end":1.0,"bits_per_second":941000000.0,"retransmits":2,"omitted":false}],"sum":{"start":0.0,"end":1.0,"bits_per_second":941000000.0,"retransmits":2,"omitted":false}}}"#;
        match parse_stdout_line(raw) {
            TelemetryMsg::Event(IperfEvent::Interval(iv)) => {
                let sum = iv.sum.unwrap();
                assert!((sum.bits_per_second - 941_000_000.0).abs() < 1.0);
                assert_eq!(sum.retransmits, Some(2));
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn parses_udp_end_event_with_jitter_and_loss() {
        let raw = r#"{"event":"end","data":{"streams":[],"sum":{"start":0.0,"end":10.0,"bits_per_second":999970.5,"jitter_ms":0.304,"lost_packets":5,"packets":870,"lost_percent":0.574}}}"#;
        match parse_stdout_line(raw) {
            TelemetryMsg::Event(IperfEvent::End(end)) => {
                let sum = end.sum.unwrap();
                assert_eq!(sum.lost_packets, Some(5));
                assert!((sum.jitter_ms.unwrap() - 0.304).abs() < 0.001);
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn parses_legacy_console_fallback() {
        let raw = "[  5]   0.00-1.00   sec   112 MBytes   941 Mbits/sec    3   424 KBytes";
        match parse_stdout_line(raw) {
            TelemetryMsg::Event(IperfEvent::Interval(iv)) => {
                let sum = iv.sum.unwrap();
                assert!((sum.bits_per_second - 941_000_000.0).abs() < 1.0);
                assert_eq!(sum.retransmits, Some(3));
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }
}
