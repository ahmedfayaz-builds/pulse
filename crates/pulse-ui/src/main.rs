#![windows_subsystem = "windows"]

use pulse_core::config::{CongestionAlgo, Direction, Mode, Preset, Protocol, StopCondition, TestConfig};
use pulse_core::engine::run_native;
use pulse_core::health::{build_verdict, HealthStatus};
use pulse_core::history::{AppHistory, RunRecord};
use pulse_core::parser::{IperfEvent, TelemetryMsg};
use pulse_core::report::{IntervalSample, SessionReport};
use pulse_core::runner::run_iperf;
use slint::{ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::f64::consts::PI;
use std::net::IpAddr;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

slint::include_modules!();

fn parse_bps_rate(s: &str) -> u64 {
    let t = s.trim().to_uppercase();
    if let Some(num) = t.strip_suffix('G') {
        (num.parse::<f64>().unwrap_or(10.0) * 1_000_000_000.0) as u64
    } else if let Some(num) = t.strip_suffix('M') {
        (num.parse::<f64>().unwrap_or(10.0) * 1_000_000.0) as u64
    } else if let Some(num) = t.strip_suffix('K') {
        (num.parse::<f64>().unwrap_or(10.0) * 1_000.0) as u64
    } else {
        t.parse::<u64>().unwrap_or(10_000_000)
    }
}

fn parse_size_bytes(s: &str) -> Option<u64> {
    let t = s.trim().to_uppercase();
    if t.is_empty() {
        return None;
    }
    if let Some(num) = t.strip_suffix('M') {
        num.parse::<u64>().ok().map(|v| v * 1024 * 1024)
    } else if let Some(num) = t.strip_suffix('K') {
        num.parse::<u64>().ok().map(|v| v * 1024)
    } else {
        t.parse::<u64>().ok()
    }
}

fn build_config_from_ui(ui: &AppWindow) -> TestConfig {
    let port = ui.get_port().trim().parse::<u16>().unwrap_or(5201);
    let mode = if ui.get_is_server() {
        Mode::Server {
            one_off: ui.get_server_one_off(),
        }
    } else {
        Mode::Client {
            host: ui.get_host().to_string(),
        }
    };

    let direction = match ui.get_direction_idx() {
        0 => Direction::Upload,
        1 => Direction::ReverseDownload,
        _ => Direction::Bidirectional,
    };

    let congestion = match ui.get_congestion_algo().trim().to_lowercase().as_str() {
        "cubic" => Some(CongestionAlgo::Cubic),
        "bbr" => Some(CongestionAlgo::Bbr),
        "reno" => Some(CongestionAlgo::Reno),
        _ => None,
    };

    let protocol = match ui.get_protocol_idx() {
        1 => Protocol::Udp {
            target_bps: parse_bps_rate(&ui.get_udp_bandwidth()),
            burst_packets: None,
        },
        2 => Protocol::Sctp,
        _ => Protocol::Tcp { congestion },
    };

    TestConfig {
        mode,
        port,
        bind_addr: ui.get_bind_ip().trim().parse::<IpAddr>().ok(),
        protocol,
        direction,
        parallel_streams: ui.get_parallel_streams().trim().parse::<u8>().unwrap_or(1),
        window_bytes: parse_size_bytes(&ui.get_window_size()),
        mss: ui.get_mss_size().trim().parse::<u16>().ok(),
        dscp: {
            let d = ui.get_dscp_val().trim().to_string();
            if d.is_empty() { None } else { Some(d) }
        },
        stop: StopCondition::Seconds(ui.get_duration_secs().trim().parse::<u32>().unwrap_or(10)),
        omit_seconds: ui.get_omit_secs().trim().parse::<u32>().unwrap_or(0),
        interval_secs: ui.get_interval_secs().trim().parse::<f32>().unwrap_or(1.0),
    }
}

fn apply_preset_to_ui(ui: &AppWindow, idx: i32) {
    let preset = match idx {
        0 => Preset::MaxSpeed,
        1 => Preset::VoipGaming,
        _ => Preset::WifiRoaming,
    };
    let cfg = TestConfig::from_preset(preset, ui.get_host().to_string());
    ui.set_active_preset(idx);
    ui.set_direction_idx(match cfg.direction {
        Direction::Upload => 0,
        Direction::ReverseDownload => 1,
        Direction::Bidirectional => 2,
    });
    ui.set_direction_label(match cfg.direction {
        Direction::Upload => "UPLOAD".into(),
        Direction::ReverseDownload => "DOWNLOAD".into(),
        Direction::Bidirectional => "BIDIRECTIONAL".into(),
    });
    match cfg.protocol {
        Protocol::Tcp { .. } => ui.set_protocol_idx(0),
        Protocol::Udp { .. } => {
            ui.set_protocol_idx(1);
            ui.set_udp_bandwidth("10M".into());
        }
        Protocol::Sctp => ui.set_protocol_idx(2),
    }
    ui.set_parallel_streams(cfg.parallel_streams.to_string().into());
    if let StopCondition::Seconds(s) = cfg.stop {
        ui.set_duration_secs(s.to_string().into());
    }
    ui.set_omit_secs(cfg.omit_seconds.to_string().into());
    ui.set_interval_secs(format!("{:.1}", cfg.interval_secs).into());
    ui.set_dscp_val(cfg.dscp.clone().unwrap_or_default().into());
    ui.set_cli_preview(cfg.to_cli_string().into());
}

/// Generates 29 radial tick marks across the 270° gauge arc (135° to 405°).
fn build_gauge_ticks() -> String {
    let mut cmds = String::new();
    let cx = 120.0;
    let cy = 120.0;
    let total = 28;
    for i in 0..=total {
        let frac = i as f64 / total as f64;
        let deg = 135.0 + frac * 270.0;
        let rad = deg * PI / 180.0;
        let r_out = 96.0;
        let r_in = if i % 4 == 0 { 84.0 } else { 90.0 };
        let x1 = cx + r_in * rad.cos();
        let y1 = cy + r_in * rad.sin();
        let x2 = cx + r_out * rad.cos();
        let y2 = cy + r_out * rad.sin();
        cmds.push_str(&format!("M {x1:.1} {y1:.1} L {x2:.1} {y2:.1} "));
    }
    cmds
}

/// Logarithmic speedometer sweep (0..2000+ Mbps) mapped to 0°..270° arc.
fn build_gauge_arc(mbps: f64) -> String {
    if mbps <= 0.05 {
        return String::new();
    }
    // Log scale: 1 Mbps -> ~0.18, 50 Mbps -> ~0.58, 500 Mbps -> ~0.85, 2000+ Mbps -> 1.0
    let frac = ((1.0 + mbps).log10() / (1.0 + 2000.0_f64).log10()).clamp(0.02, 0.995);
    let cx = 120.0;
    let cy = 120.0;
    let r = 110.0;
    let start_deg = 135.0;
    let sweep_deg = frac * 270.0;
    let end_deg = start_deg + sweep_deg;

    let start_rad = start_deg * PI / 180.0;
    let end_rad = end_deg * PI / 180.0;

    let x1 = cx + r * start_rad.cos();
    let y1 = cy + r * start_rad.sin();
    let x2 = cx + r * end_rad.cos();
    let y2 = cy + r * end_rad.sin();
    let large_arc = if sweep_deg > 180.0 { 1 } else { 0 };

    format!("M {x1:.1} {y1:.1} A {r:.1} {r:.1} 0 {large_arc} 1 {x2:.1} {y2:.1}")
}

/// Smooth cubic-bezier wave paths (stroke + closed area fill) for 900x280 hero canvas.
fn build_wave_paths(samples_mbps: &[f64]) -> (String, String) {
    if samples_mbps.is_empty() {
        return (String::new(), String::new());
    }
    let max_val = samples_mbps.iter().copied().fold(10.0_f64, f64::max) * 1.25;
    let w = 900.0;
    let h_bot = 280.0;
    let h_top = 80.0; // keep wave gracefully below header pills
    let span_y = h_bot - h_top;

    let pts: Vec<(f64, f64)> = if samples_mbps.len() == 1 {
        let y = h_bot - (samples_mbps[0] / max_val).clamp(0.0, 1.0) * span_y * 0.65;
        vec![(0.0, y), (w, y)]
    } else {
        samples_mbps
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let x = (i as f64 / (samples_mbps.len() - 1) as f64) * w;
                let y = h_bot - (v / max_val).clamp(0.0, 1.0) * span_y * 0.72 - 18.0;
                (x, y)
            })
            .collect()
    };

    let mut stroke = format!("M {:.1} {:.1}", pts[0].0, pts[0].1);
    for pair in pts.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        let cx = (x0 + x1) * 0.5;
        stroke.push_str(&format!(" C {cx:.1} {y0:.1}, {cx:.1} {y1:.1}, {x1:.1} {y1:.1}"));
    }

    let mut fill = stroke.clone();
    fill.push_str(&format!(" L {w:.1} {h_bot:.1} L 0.0 {h_bot:.1} Z"));
    (stroke, fill)
}

fn build_sparkline(samples: &[f64]) -> String {
    if samples.len() < 2 {
        return String::new();
    }
    let max_v = samples.iter().copied().fold(1.0_f64, f64::max) * 1.15;
    let mut path = String::new();
    let n = samples.len();
    for (i, &v) in samples.iter().enumerate() {
        let x = (i as f64 / (n - 1) as f64) * 100.0;
        let y = 30.0 - (v / max_v).clamp(0.0, 1.0) * 26.0;
        if i == 0 {
            path.push_str(&format!("M {x:.1} {y:.1}"));
        } else {
            path.push_str(&format!(" L {x:.1} {y:.1}"));
        }
    }
    path
}

fn readiness_labels(mbps: f64, jitter: Option<f64>, loss: Option<f64>) -> (&'static str, &'static str, &'static str) {
    let stream = if mbps >= 25.0 { "4K Ready" } else if mbps >= 5.0 { "HD Ready" } else { "Limited" };
    let jit = jitter.unwrap_or(0.0);
    let lst = loss.unwrap_or(0.0);
    let calls = if mbps >= 3.0 && jit < 20.0 && lst <= 1.0 { "Clear" } else { "Unstable" };
    let gaming = if mbps >= 5.0 && jit < 10.0 && lst <= 0.2 { "Optimal" } else if jit < 25.0 { "Playable" } else { "Laggy" };
    (stream, calls, gaming)
}

fn health_level_int(status: HealthStatus) -> i32 {
    match status {
        HealthStatus::Unknown => 0,
        HealthStatus::Healthy => 1,
        HealthStatus::Warning => 2,
        HealthStatus::Critical => 3,
    }
}

fn sync_history_to_ui(ui: &AppWindow, hist: &AppHistory) {
    let hosts: Vec<SharedString> = hist.recent_hosts.iter().map(SharedString::from).collect();
    ui.set_recent_hosts(ModelRc::new(VecModel::from(hosts)));

    let runs: Vec<SharedString> = hist
        .runs
        .iter()
        .map(|r| {
            SharedString::from(format!(
                "{}:{} · {} · Avg {:.1} Mbps (Peak {:.1}) · {}",
                r.host, r.port, r.protocol, r.avg_mbps, r.peak_mbps, r.health_label
            ))
        })
        .collect();
    ui.set_run_history(ModelRc::new(VecModel::from(runs)));
}

fn main() -> Result<(), slint::PlatformError> {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let ui = AppWindow::new()?;
    ui.set_gauge_ticks_path(build_gauge_ticks().into());

    let history = Arc::new(Mutex::new(AppHistory::load()));
    {
        let h = history.lock().unwrap();
        ui.set_host(h.last_host.clone().into());
        ui.set_port(h.last_port.to_string().into());
        ui.set_dark_mode(h.dark_mode);
        sync_history_to_ui(&ui, &h);
    }

    let active_cancel: Rc<RefCell<Option<CancellationToken>>> = Rc::new(RefCell::new(None));
    let last_report: Arc<Mutex<Option<SessionReport>>> = Arc::new(Mutex::new(None));

    // Recent host selection
    {
        let ui_weak = ui.as_weak();
        ui.on_select_recent_host(move |h| {
            if let Some(ui) = ui_weak.upgrade() {
                if !ui.get_is_running() {
                    ui.set_host(h);
                    let cfg = build_config_from_ui(&ui);
                    ui.set_cli_preview(cfg.to_cli_string().into());
                }
            }
        });
    }

    // Persist theme toggle
    {
        let hist_slot = Arc::clone(&history);
        ui.on_theme_toggled(move |dark| {
            let mut h = hist_slot.lock().unwrap();
            h.dark_mode = dark;
            h.save();
        });
    }

    // Export session report (pdf / csv / txt)
    {
        let ui_weak = ui.as_weak();
        let rep_slot = Arc::clone(&last_report);
        ui.on_export_report(move |fmt| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let guard = rep_slot.lock().unwrap();
            let Some(report) = guard.as_ref() else {
                ui.set_health_summary("Run a test first before exporting.".into());
                return;
            };
            let ext = fmt.as_str();
            let filter_name = match ext {
                "pdf" => "PDF Report",
                "csv" => "CSV Data",
                _ => "Text Report",
            };
            let default_name = format!("pulse-report.{ext}");
            if let Some(path) = rfd::FileDialog::new()
                .add_filter(filter_name, &[ext])
                .set_file_name(&default_name)
                .save_file()
            {
                let res = match ext {
                    "pdf" => std::fs::write(&path, report.to_pdf()),
                    "csv" => std::fs::write(&path, report.to_csv()),
                    _ => std::fs::write(&path, report.to_text()),
                };
                match res {
                    Ok(()) => ui.set_health_summary(format!("Exported report to {}", path.display()).into()),
                    Err(e) => ui.set_health_summary(format!("Export failed: {e}").into()),
                }
            }
        });
    }

    // Refresh CLI preview & direction label when config changes
    {
        let ui_weak = ui.as_weak();
        ui.on_config_changed(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let cfg = build_config_from_ui(&ui);
                ui.set_cli_preview(cfg.to_cli_string().into());
                ui.set_direction_label(match cfg.direction {
                    Direction::Upload => "UPLOAD".into(),
                    Direction::ReverseDownload => "DOWNLOAD".into(),
                    Direction::Bidirectional => "BIDIRECTIONAL".into(),
                });
            }
        });
    }

    // Preset selection
    {
        let ui_weak = ui.as_weak();
        ui.on_select_preset(move |idx| {
            if let Some(ui) = ui_weak.upgrade() {
                if !ui.get_is_running() {
                    apply_preset_to_ui(&ui, idx);
                }
            }
        });
    }

    // Run / Stop test
    {
        let ui_weak = ui.as_weak();
        let cancel_slot = Rc::clone(&active_cancel);
        let rep_slot = Arc::clone(&last_report);
        let hist_slot = Arc::clone(&history);
        let handle = rt.handle().clone();

        ui.on_run_or_stop(move || {
            let Some(ui) = ui_weak.upgrade() else { return };

            if ui.get_is_running() {
                if let Some(token) = cancel_slot.borrow_mut().take() {
                    token.cancel();
                }
                ui.set_status_text("STOPPING".into());
                return;
            }

            let cfg = build_config_from_ui(&ui);
            if let Err(err) = cfg.validate() {
                ui.set_status_text("INVALID".into());
                ui.set_status_kind(3);
                ui.set_health_summary(err.to_string().into());
                return;
            }

            let custom = ui.get_custom_iperf_path().trim().to_string();
            let custom_bin = if custom.is_empty() {
                None
            } else {
                let pb = std::path::PathBuf::from(&custom);
                if !pb.is_file() {
                    ui.set_status_text("BAD PATH".into());
                    ui.set_status_kind(3);
                    ui.set_health_summary("Custom iperf3 path does not exist.".into());
                    return;
                }
                Some(pb)
            };

            let args = cfg.to_cli_args();
            if let Mode::Client { ref host } = cfg.mode {
                let mut h = hist_slot.lock().unwrap();
                h.record_host(host, cfg.port);
                sync_history_to_ui(&ui, &h);
            }
            ui.set_cli_preview(cfg.to_cli_string().into());
            ui.set_is_running(true);
            ui.set_status_text(if ui.get_is_server() { "LISTENING" } else { "RUNNING" }.into());
            ui.set_status_kind(1);
            ui.set_health_badge("TESTING".into());
            ui.set_health_level(0);
            ui.set_health_summary("Streaming real-time telemetry...".into());
            ui.set_gauge_value("0.0".into());
            ui.set_gauge_arc_path("".into());
            ui.set_wave_stroke_path("".into());
            ui.set_wave_fill_path("".into());
            ui.set_spark_throughput("".into());
            ui.set_has_report(false);
            ui.set_log_output("".into());
            match cfg.protocol {
                Protocol::Udp { .. } => {
                    ui.set_metric_retransmits("N/A (UDP)".into());
                    ui.set_metric_retransmits_sub("TCP only".into());
                    ui.set_metric_jitter("0.00 ms".into());
                    ui.set_metric_jitter_sub("Arrival variance".into());
                    ui.set_metric_loss("0.00%".into());
                    ui.set_metric_loss_sub("Dropped datagrams".into());
                }
                _ => {
                    ui.set_metric_retransmits("0".into());
                    ui.set_metric_retransmits_sub("TCP retries".into());
                    ui.set_metric_jitter("N/A (TCP)".into());
                    ui.set_metric_jitter_sub("Use VoIP / Gaming (UDP)".into());
                    ui.set_metric_loss("N/A (TCP)".into());
                    ui.set_metric_loss_sub("Use VoIP / Gaming (UDP)".into());
                }
            }

            let token = CancellationToken::new();
            *cancel_slot.borrow_mut() = Some(token.clone());

            let (tx, mut rx) = mpsc::channel::<TelemetryMsg>(128);
            let ui_stream_weak = ui.as_weak();
            let cfg_for_report = cfg.clone();
            let rep_for_task = Arc::clone(&rep_slot);
            let hist_for_task = Arc::clone(&hist_slot);

            handle.spawn(async move {
                let res = match custom_bin {
                    Some(bin) => run_iperf(bin, args, tx.clone(), token).await,
                    None => run_native(cfg, tx.clone(), token).await,
                };
                if let Err(e) = res {
                    let _ = tx.send(TelemetryMsg::ErrorLog(format!("Engine error: {e}"))).await;
                    let _ = tx.send(TelemetryMsg::Exited(Some(1))).await;
                }
            });

            handle.spawn(async move {
                let mut samples: Vec<f64> = Vec::with_capacity(64);
                let mut report_samples: Vec<IntervalSample> = Vec::with_capacity(64);
                let mut peak = 0.0_f64;

                while let Some(msg) = rx.recv().await {
                    let ui_w = ui_stream_weak.clone();
                    match msg {
                        TelemetryMsg::Event(IperfEvent::Interval(iv)) => {
                            if let Some(sum) = iv.sum {
                                let mbps = sum.bits_per_second / 1_000_000.0;
                                if mbps > peak { peak = mbps; }
                                samples.push(mbps);
                                report_samples.push(IntervalSample {
                                    start_sec: sum.start,
                                    end_sec: sum.end,
                                    mbps,
                                    retransmits: sum.retransmits,
                                    jitter_ms: sum.jitter_ms,
                                    loss_percent: sum.lost_percent,
                                    omitted: sum.omitted.unwrap_or(false),
                                });
                                let arc = build_gauge_arc(mbps);
                                let (wave_s, wave_f) = build_wave_paths(&samples);
                                let spark = build_sparkline(&samples);
                                let verdict = build_verdict(sum.retransmits, sum.jitter_ms, sum.lost_percent);
                                let (r_stream, r_calls, r_game) = readiness_labels(mbps, sum.jitter_ms, sum.lost_percent);
                                let peak_val = peak;
                                let scale_max = (peak_val * 1.25).max(10.0);
                                let scale_mid = scale_max * 0.5;

                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(u) = ui_w.upgrade() {
                                        u.set_gauge_value(format!("{mbps:.1}").into());
                                        u.set_gauge_arc_path(arc.into());
                                        u.set_wave_stroke_path(wave_s.into());
                                        u.set_wave_fill_path(wave_f.into());
                                        u.set_spark_throughput(spark.into());
                                        u.set_graph_max_label(format!("{scale_max:.0} Mbps").into());
                                        u.set_graph_mid_label(format!("{scale_mid:.0} Mbps").into());
                                        u.set_live_mbps(format!("{mbps:.1} Mbps").into());
                                        u.set_peak_mbps(format!("Peak {peak_val:.1}").into());
                                        u.set_health_badge(verdict.overall.label().into());
                                        u.set_health_level(health_level_int(verdict.overall));
                                        u.set_health_summary(verdict.summary_line.into());
                                        u.set_ready_stream(r_stream.into());
                                        u.set_ready_calls(r_calls.into());
                                        u.set_ready_gaming(r_game.into());
                                        if let Some(r) = sum.retransmits {
                                            u.set_metric_retransmits(r.to_string().into());
                                        }
                                        if let Some(j) = sum.jitter_ms {
                                            u.set_metric_jitter(format!("{j:.2} ms").into());
                                        }
                                        if let Some(l) = sum.lost_percent {
                                            u.set_metric_loss(format!("{l:.2}%").into());
                                        }
                                    }
                                });
                            }
                        }
                        TelemetryMsg::Event(IperfEvent::End(end)) => {
                            let summary_tick = end.sum_received.or(end.sum_sent).or(end.sum);
                            if let Some(sum) = summary_tick {
                                let mbps = sum.bits_per_second / 1_000_000.0;
                                let arc = build_gauge_arc(mbps);
                                let verdict = build_verdict(sum.retransmits, sum.jitter_ms, sum.lost_percent);
                                let (r_stream, r_calls, r_game) = readiness_labels(mbps, sum.jitter_ms, sum.lost_percent);
                                let report = SessionReport::from_session(
                                    &cfg_for_report,
                                    mbps,
                                    peak.max(mbps),
                                    sum.retransmits,
                                    sum.jitter_ms,
                                    sum.lost_percent,
                                    &verdict,
                                    report_samples.clone(),
                                );
                                {
                                    let mut h = hist_for_task.lock().unwrap();
                                    h.record_run(RunRecord {
                                        timestamp_unix: report.timestamp_unix,
                                        host: report.target.clone(),
                                        port: report.port,
                                        protocol: report.protocol_label.clone(),
                                        avg_mbps: report.avg_mbps,
                                        peak_mbps: report.peak_mbps,
                                        health_label: report.health_label.clone(),
                                    });
                                }
                                *rep_for_task.lock().unwrap() = Some(report);
                                let hist_ui = Arc::clone(&hist_for_task);
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(u) = ui_w.upgrade() {
                                        sync_history_to_ui(&u, &hist_ui.lock().unwrap());
                                        u.set_has_report(true);
                                        u.set_gauge_value(format!("{mbps:.1}").into());
                                        u.set_gauge_arc_path(arc.into());
                                        u.set_live_mbps(format!("{mbps:.1} Mbps").into());
                                        u.set_health_badge(verdict.overall.label().into());
                                        u.set_health_level(health_level_int(verdict.overall));
                                        u.set_health_summary(verdict.summary_line.into());
                                        u.set_ready_stream(r_stream.into());
                                        u.set_ready_calls(r_calls.into());
                                        u.set_ready_gaming(r_game.into());
                                        if let Some(r) = sum.retransmits {
                                            u.set_metric_retransmits(r.to_string().into());
                                        }
                                        if let Some(j) = sum.jitter_ms {
                                            u.set_metric_jitter(format!("{j:.2} ms").into());
                                        }
                                        if let Some(l) = sum.lost_percent {
                                            u.set_metric_loss(format!("{l:.2}%").into());
                                        }
                                    }
                                });
                            }
                        }
                        TelemetryMsg::RawLog(line) | TelemetryMsg::ErrorLog(line) => {
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(u) = ui_w.upgrade() {
                                    let mut cur = u.get_log_output().to_string();
                                    cur.push_str(&line);
                                    cur.push('\n');
                                    u.set_log_output(cur.into());
                                }
                            });
                        }
                        TelemetryMsg::Exited(code) => {
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(u) = ui_w.upgrade() {
                                    u.set_is_running(false);
                                    let ok = code == Some(0);
                                    u.set_status_text(if ok { "COMPLETE" } else { "STOPPED" }.into());
                                    u.set_status_kind(if ok { 2 } else { 3 });
                                }
                            });
                            break;
                        }
                        _ => {}
                    }
                }
            });
        });
    }

    ui.run()
}
