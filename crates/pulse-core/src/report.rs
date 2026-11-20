use crate::config::{Direction, Mode, Protocol, TestConfig};
use crate::health::HealthVerdict;
use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct IntervalSample {
    pub start_sec: f64,
    pub end_sec: f64,
    pub mbps: f64,
    pub retransmits: Option<u64>,
    pub jitter_ms: Option<f64>,
    pub loss_percent: Option<f64>,
    pub omitted: bool,
}

#[derive(Debug, Clone)]
pub struct SessionReport {
    pub timestamp_unix: u64,
    pub target: String,
    pub port: u16,
    pub mode_label: String,
    pub protocol_label: String,
    pub direction_label: String,
    pub streams: u8,
    pub avg_mbps: f64,
    pub peak_mbps: f64,
    pub retransmits: Option<u64>,
    pub jitter_ms: Option<f64>,
    pub loss_percent: Option<f64>,
    pub health_label: String,
    pub health_summary: String,
    pub samples: Vec<IntervalSample>,
}

impl SessionReport {
    pub fn from_session(
        cfg: &TestConfig,
        avg_mbps: f64,
        peak_mbps: f64,
        retransmits: Option<u64>,
        jitter_ms: Option<f64>,
        loss_percent: Option<f64>,
        verdict: &HealthVerdict,
        samples: Vec<IntervalSample>,
    ) -> Self {
        let timestamp_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let (target, mode_label) = match &cfg.mode {
            Mode::Client { host } => (host.clone(), "Client".to_string()),
            Mode::Server { one_off } => (
                "0.0.0.0".to_string(),
                if *one_off { "Server (One-Off)" } else { "Server" }.to_string(),
            ),
        };

        let protocol_label = match cfg.protocol {
            Protocol::Tcp { .. } => "TCP",
            Protocol::Udp { .. } => "UDP",
            Protocol::Sctp => "SCTP",
        }
        .to_string();

        let direction_label = match cfg.direction {
            Direction::Upload => "Upload",
            Direction::ReverseDownload => "Download (-R)",
            Direction::Bidirectional => "Bidirectional (--bidir)",
        }
        .to_string();

        Self {
            timestamp_unix,
            target,
            port: cfg.port,
            mode_label,
            protocol_label,
            direction_label,
            streams: cfg.parallel_streams,
            avg_mbps,
            peak_mbps,
            retransmits,
            jitter_ms,
            loss_percent,
            health_label: verdict.overall.label().to_string(),
            health_summary: verdict.summary_line.clone(),
            samples,
        }
    }

    /// Exports raw interval telemetry as RFC 4180 CSV.
    pub fn to_csv(&self) -> String {
        let mut out = String::from("start_sec,end_sec,mbps,retransmits,jitter_ms,loss_percent,omitted\n");
        for s in &self.samples {
            let r = s.retransmits.map(|v| v.to_string()).unwrap_or_default();
            let j = s.jitter_ms.map(|v| format!("{v:.3}")).unwrap_or_default();
            let l = s.loss_percent.map(|v| format!("{v:.3}")).unwrap_or_default();
            let _ = writeln!(
                out,
                "{:.2},{:.2},{:.3},{},{},{},{}",
                s.start_sec, s.end_sec, s.mbps, r, j, l, s.omitted
            );
        }
        out
    }

    /// Exports a human-readable plain-text diagnostic report.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "PULSE NETWORK DIAGNOSTIC REPORT");
        let _ = writeln!(out, "==================================================");
        let _ = writeln!(out, "Timestamp (Unix) : {}", self.timestamp_unix);
        let _ = writeln!(out, "Mode             : {}", self.mode_label);
        let _ = writeln!(out, "Endpoint         : {}:{}", self.target, self.port);
        let _ = writeln!(out, "Protocol/Dir     : {} / {}", self.protocol_label, self.direction_label);
        let _ = writeln!(out, "Parallel Streams : {}", self.streams);
        let _ = writeln!(out);
        let _ = writeln!(out, "SUMMARY");
        let _ = writeln!(out, "--------------------------------------------------");
        let _ = writeln!(out, "Health Verdict   : {} - {}", self.health_label, self.health_summary);
        let _ = writeln!(out, "Average Speed    : {:.2} Mbps", self.avg_mbps);
        let _ = writeln!(out, "Peak Speed       : {:.2} Mbps", self.peak_mbps);
        if let Some(r) = self.retransmits {
            let _ = writeln!(out, "TCP Retransmits  : {r}");
        }
        if let Some(j) = self.jitter_ms {
            let _ = writeln!(out, "UDP Jitter       : {j:.3} ms");
        }
        if let Some(l) = self.loss_percent {
            let _ = writeln!(out, "Packet Loss      : {l:.3}%");
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "INTERVAL TELEMETRY ({} samples)", self.samples.len());
        let _ = writeln!(out, "--------------------------------------------------");
        let _ = writeln!(out, "{:<14} {:>12} {:>10} {:>10} {:>8}", "Interval (s)", "Speed (Mbps)", "Retrans", "Jitter(ms)", "Loss(%)");
        for s in &self.samples {
            let iv = format!("{:.2}-{:.2}", s.start_sec, s.end_sec);
            let r = s.retransmits.map(|v| v.to_string()).unwrap_or_else(|| "-".into());
            let j = s.jitter_ms.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into());
            let l = s.loss_percent.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into());
            let _ = writeln!(out, "{iv:<14} {:>12.2} {:>10} {:>10} {:>8}", s.mbps, r, j, l);
        }
        out
    }

    /// Generates a standalone PDF 1.4 report with a native vector throughput graph.
    pub fn to_pdf(&self) -> Vec<u8> {
        let mut stream = String::from("BT\n");
        let mut y = 755.0_f32;

        let header_lines: Vec<(bool, String)> = vec![
            (true, "PULSE NETWORK DIAGNOSTIC REPORT".into()),
            (false, format!("Endpoint: {}:{}  |  Mode: {}  |  Protocol: {} ({})", self.target, self.port, self.mode_label, self.protocol_label, self.direction_label)),
            (false, format!("Unix Timestamp: {}  |  Parallel Streams: {}", self.timestamp_unix, self.streams)),
            (false, String::new()),
            (true, format!("HEALTH VERDICT: {}", self.health_label)),
            (false, self.health_summary.clone()),
            (false, format!("Average Speed: {:.2} Mbps   |   Peak Speed: {:.2} Mbps", self.avg_mbps, self.peak_mbps)),
            (false, format!(
                "Retransmits: {}   |   Jitter: {}   |   Packet Loss: {}",
                self.retransmits.map(|v| v.to_string()).unwrap_or_else(|| "N/A (UDP)".into()),
                self.jitter_ms.map(|v| format!("{v:.2} ms")).unwrap_or_else(|| "N/A (TCP)".into()),
                self.loss_percent.map(|v| format!("{v:.2}%")).unwrap_or_else(|| "N/A (TCP)".into()),
            )),
        ];

        for (bold, text) in &header_lines {
            let escaped = pdf_escape(text);
            if *bold {
                let _ = writeln!(stream, "/F2 12 Tf 1 0 0 1 48 {y:.1} Tm ({escaped}) Tj");
            } else {
                let _ = writeln!(stream, "/F1 9.5 Tf 1 0 0 1 48 {y:.1} Tm ({escaped}) Tj");
            }
            y -= if *bold { 17.0 } else { 13.0 };
        }
        stream.push_str("ET\n");

        // Vector Throughput Graph (x: 48..564, y: chart_bot..chart_top)
        let chart_x = 48.0_f32;
        let chart_w = 516.0_f32;
        let chart_h = 120.0_f32;
        let chart_top = y - 10.0;
        let chart_bot = chart_top - chart_h;

        // Chart background & border
        let _ = writeln!(
            stream,
            "q\n0.96 0.97 0.99 rg 0.85 0.88 0.93 RG 0.8 w\n{chart_x:.1} {chart_bot:.1} {chart_w:.1} {chart_h:.1} re B"
        );
        // Horizontal gridlines (mid & top)
        let mid_y = chart_bot + chart_h * 0.5;
        let _ = writeln!(
            stream,
            "0.88 0.91 0.95 RG 0.5 w [3 3] 0 d\n{chart_x:.1} {mid_y:.1} m {:.1} {mid_y:.1} l S\n[] 0 d",
            chart_x + chart_w
        );

        let max_mbps = self
            .samples
            .iter()
            .map(|s| s.mbps)
            .fold(1.0_f64, f64::max) as f32
            * 1.15;

        if !self.samples.is_empty() {
            let n = self.samples.len();
            let pts: Vec<(f32, f32)> = if n == 1 {
                let py = chart_bot + ((self.samples[0].mbps as f32 / max_mbps).clamp(0.0, 1.0)) * (chart_h - 14.0) + 4.0;
                vec![(chart_x, py), (chart_x + chart_w, py)]
            } else {
                self.samples
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let px = chart_x + (i as f32 / (n - 1) as f32) * chart_w;
                        let py = chart_bot + ((s.mbps as f32 / max_mbps).clamp(0.0, 1.0)) * (chart_h - 14.0) + 4.0;
                        (px, py)
                    })
                    .collect()
            };

            // Area fill under curve
            let _ = writeln!(stream, "0.84 0.94 0.98 rg");
            let _ = writeln!(stream, "{:.1} {chart_bot:.1} m", pts[0].0);
            for (px, py) in &pts {
                let _ = writeln!(stream, "{px:.1} {py:.1} l");
            }
            let _ = writeln!(stream, "{:.1} {chart_bot:.1} l h f", pts.last().unwrap().0);

            // Primary line stroke
            let _ = writeln!(stream, "0.01 0.52 0.78 RG 1.8 w");
            let _ = writeln!(stream, "{:.1} {:.1} m", pts[0].0, pts[0].1);
            for (px, py) in &pts[1..] {
                let _ = writeln!(stream, "{px:.1} {py:.1} l");
            }
            let _ = writeln!(stream, "S");
        }
        stream.push_str("Q\n");

        // Y-axis scale labels on chart
        stream.push_str("BT\n");
        let _ = writeln!(
            stream,
            "/F1 8 Tf 0.4 0.45 0.52 rg 1 0 0 1 {:.1} {:.1} Tm ({:.0} Mbps) Tj",
            chart_x + 6.0,
            chart_top - 12.0,
            max_mbps
        );
        let _ = writeln!(
            stream,
            "/F1 8 Tf 0.4 0.45 0.52 rg 1 0 0 1 {:.1} {:.1} Tm ({:.0} Mbps) Tj",
            chart_x + 6.0,
            mid_y + 3.0,
            max_mbps * 0.5
        );
        let _ = writeln!(
            stream,
            "/F1 8 Tf 0.4 0.45 0.52 rg 1 0 0 1 {:.1} {:.1} Tm (0 Mbps) Tj",
            chart_x + 6.0,
            chart_bot + 4.0
        );

        // Interval table below chart
        y = chart_bot - 22.0;
        let _ = writeln!(
            stream,
            "/F2 11 Tf 0 0 0 rg 1 0 0 1 48 {y:.1} Tm (INTERVAL SAMPLES \\(up to 28 of {}\\)) Tj",
            self.samples.len()
        );
        y -= 15.0;
        let _ = writeln!(
            stream,
            "/F1 9.5 Tf 1 0 0 1 48 {y:.1} Tm (Interval \\(s\\)      Speed \\(Mbps\\)     Retrans     Jitter\\(ms\\)     Loss\\(%\\)) Tj"
        );
        y -= 13.5;

        for s in self.samples.iter().take(28) {
            if y < 44.0 {
                break;
            }
            let iv = format!("{:.2}-{:.2}", s.start_sec, s.end_sec);
            let r = s.retransmits.map(|v| v.to_string()).unwrap_or_else(|| "-".into());
            let j = s.jitter_ms.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into());
            let l = s.loss_percent.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into());
            let row = pdf_escape(&format!(
                "{iv:<16}  {:>10.2}     {:>7}     {:>10}     {:>7}",
                s.mbps, r, j, l
            ));
            let _ = writeln!(stream, "/F1 9 Tf 1 0 0 1 48 {y:.1} Tm ({row}) Tj");
            y -= 12.5;
        }
        stream.push_str("ET\n");

        let obj1 = "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        let obj2 = "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
        let obj3 = "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>\nendobj\n";
        let obj4 = format!("4 0 obj\n<< /Length {} >>\nstream\n{stream}endstream\nendobj\n", stream.len());
        let obj5 = "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>\nendobj\n";
        let obj6 = "6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>\nendobj\n";

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(6);
        for obj in [obj1, obj2, obj3, obj4.as_str(), obj5, obj6] {
            offsets.push(pdf.len());
            pdf.extend_from_slice(obj.as_bytes());
        }

        let xref_pos = pdf.len();
        let _ = writeln!(pdf, "xref\n0 7\n0000000000 65535 f ");
        for off in offsets {
            let _ = writeln!(pdf, "{off:010} 00000 n ");
        }
        let _ = writeln!(
            pdf,
            "trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF"
        );
        pdf
    }
}

fn pdf_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            c if c.is_ascii() => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::build_verdict;

    fn sample_report() -> SessionReport {
        let cfg = TestConfig::default();
        let verdict = build_verdict(Some(1), Some(0.4), Some(0.0));
        SessionReport::from_session(
            &cfg,
            941.5,
            960.2,
            Some(1),
            Some(0.4),
            Some(0.0),
            &verdict,
            vec![IntervalSample {
                start_sec: 0.0,
                end_sec: 1.0,
                mbps: 941.5,
                retransmits: Some(1),
                jitter_ms: Some(0.4),
                loss_percent: Some(0.0),
                omitted: false,
            }],
        )
    }

    #[test]
    fn exports_valid_csv_text_and_pdf() {
        let rep = sample_report();
        let csv = rep.to_csv();
        assert!(csv.starts_with("start_sec,end_sec,mbps,"));
        assert!(csv.contains("941.500"));

        let txt = rep.to_text();
        assert!(txt.contains("PULSE NETWORK DIAGNOSTIC REPORT"));
        assert!(txt.contains("941.50 Mbps"));

        let pdf = rep.to_pdf();
        assert!(pdf.starts_with(b"%PDF-1.4\n"));
        assert!(String::from_utf8_lossy(&pdf).contains("%%EOF"));
    }
}
