use crate::config::{Direction, Mode, Protocol, StopCondition, TestConfig};
use crate::parser::{
    CpuUtil, EndData, EndStreamPair, EndStreamStats, Endpoint, IntervalData, IperfEvent,
    StartData, StreamTick, SumTick, TelemetryMsg, TestStartInfo,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const COOKIE_SIZE: usize = 37;
const DEFAULT_TCP_BLKSIZE: usize = 128 * 1024;
const DEFAULT_UDP_BLKSIZE: usize = 1460;

// iperf3 control protocol states (from src/iperf_api.h)
const TEST_START: i8 = 1;
const TEST_RUNNING: i8 = 2;
const TEST_END: i8 = 4;
const PARAM_EXCHANGE: i8 = 9;
const CREATE_STREAMS: i8 = 10;
const EXCHANGE_RESULTS: i8 = 13;
const DISPLAY_RESULTS: i8 = 14;
const IPERF_DONE: i8 = 16;
const ACCESS_DENIED: i8 = -1;
const SERVER_ERROR: i8 = -2;

#[derive(Debug, Serialize, Deserialize)]
struct WireParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    tcp: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    udp: Option<bool>,
    #[serde(default = "default_omit")]
    omit: u32,
    #[serde(default = "default_time")]
    time: u32,
    #[serde(default)]
    num: u64,
    #[serde(default)]
    blockcount: u64,
    #[serde(default = "default_parallel")]
    parallel: u8,
    #[serde(default = "default_len")]
    len: usize,
    #[serde(default)]
    bandwidth: u64,
    #[serde(default)]
    reverse: bool,
    #[serde(default)]
    bidirectional: bool,
    #[serde(default = "default_client_version")]
    client_version: String,
}

fn default_omit() -> u32 { 0 }
fn default_time() -> u32 { 10 }
fn default_parallel() -> u8 { 1 }
fn default_len() -> usize { DEFAULT_TCP_BLKSIZE }
fn default_client_version() -> String { "3.17".into() }

#[derive(Debug, Serialize, Deserialize, Default)]
struct WireResults {
    cpu_util_total: f64,
    cpu_util_user: f64,
    cpu_util_system: f64,
    sender_has_retransmits: i32,
    #[serde(default)]
    streams: Vec<WireStreamResult>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WireStreamResult {
    id: i32,
    bytes: u64,
    retransmits: i64,
    jitter: f64,
    errors: u64,
    packets: u64,
    start_time: f64,
    end_time: f64,
}

#[derive(Default)]
struct StreamCounters {
    bytes: AtomicU64,
    packets: AtomicU64,
    lost: AtomicU64,
    jitter_us: AtomicU64,
}

fn generate_cookie() -> [u8; COOKIE_SIZE] {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5eed_cafe);
    let mut state = nanos ^ (std::process::id() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ SEQ.fetch_add(1, Ordering::Relaxed);

    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut cookie = [0u8; COOKIE_SIZE];
    for slot in &mut cookie[..COOKIE_SIZE - 1] {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *slot = ALPHABET[(state as usize) % ALPHABET.len()];
    }
    cookie[COOKIE_SIZE - 1] = 0;
    cookie
}

async fn read_state(stream: &mut TcpStream) -> std::io::Result<i8> {
    let mut buf = [0u8; 1];
    stream.read_exact(&mut buf).await?;
    Ok(buf[0] as i8)
}

async fn write_state(stream: &mut TcpStream, state: i8) -> std::io::Result<()> {
    stream.write_all(&[state as u8]).await
}

async fn write_json_frame<T: Serialize>(stream: &mut TcpStream, val: &T) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(val)?;
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&payload).await?;
    Ok(())
}

async fn read_json_frame<T: for<'de> Deserialize<'de>>(stream: &mut TcpStream) -> anyhow::Result<T> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 4 * 1024 * 1024 {
        anyhow::bail!("Control JSON frame too large: {len} bytes");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

/// Runs Pulse's native pure-Rust iPerf3-compatible engine (Client or Server).
pub async fn run_native(
    cfg: TestConfig,
    tx: mpsc::Sender<TelemetryMsg>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let res = match &cfg.mode {
        Mode::Client { .. } => run_native_client(&cfg, &tx, &cancel).await,
        Mode::Server { one_off } => run_native_server(&cfg, *one_off, &tx, &cancel).await,
    };
    let exit_code = if res.is_ok() { 0 } else { 1 };
    if let Err(e) = &res {
        let _ = tx.send(TelemetryMsg::ErrorLog(format!("{e}"))).await;
    }
    let _ = tx.send(TelemetryMsg::Exited(Some(exit_code))).await;
    res
}

async fn run_native_client(
    cfg: &TestConfig,
    tx: &mpsc::Sender<TelemetryMsg>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let host = match &cfg.mode {
        Mode::Client { host } => host.trim(),
        _ => anyhow::bail!("Expected Client mode"),
    };

    let target = format!("{host}:{}", cfg.port);
    let mut ctrl = TcpStream::connect(&target).await?;
    ctrl.set_nodelay(true)?;
    let peer_addr = ctrl.peer_addr()?;

    let cookie = generate_cookie();
    ctrl.write_all(&cookie).await?;

    // Wait for PARAM_EXCHANGE
    match read_state(&mut ctrl).await? {
        PARAM_EXCHANGE => {}
        ACCESS_DENIED => anyhow::bail!("Server busy (access denied)"),
        SERVER_ERROR => anyhow::bail!("Remote server error during handshake"),
        other => anyhow::bail!("Unexpected handshake state: {other}"),
    }

    let (is_udp, blksize, bandwidth) = match cfg.protocol {
        Protocol::Tcp { .. } | Protocol::Sctp => (false, DEFAULT_TCP_BLKSIZE, 0),
        Protocol::Udp { target_bps, .. } => (true, DEFAULT_UDP_BLKSIZE, target_bps),
    };

    let (time, num, blockcount) = match cfg.stop {
        StopCondition::Seconds(s) => (s, 0, 0),
        StopCondition::Bytes(n) => (0, n, 0),
        StopCondition::Blocks(k) => (0, 0, k),
    };

    let params = WireParams {
        tcp: if !is_udp { Some(true) } else { None },
        udp: if is_udp { Some(true) } else { None },
        omit: cfg.omit_seconds,
        time,
        num,
        blockcount,
        parallel: cfg.parallel_streams,
        len: blksize,
        bandwidth,
        reverse: cfg.direction == Direction::ReverseDownload,
        bidirectional: cfg.direction == Direction::Bidirectional,
        client_version: "3.17-pulse".into(),
    };
    write_json_frame(&mut ctrl, &params).await?;

    if read_state(&mut ctrl).await? != CREATE_STREAMS {
        anyhow::bail!("Server rejected parameters or failed CREATE_STREAMS");
    }

    // Create data streams
    let mut tcp_streams = Vec::new();
    let mut udp_sockets = Vec::new();
    let mut counters = Vec::new();

    for _ in 0..cfg.parallel_streams {
        counters.push(Arc::new(StreamCounters::default()));
        if is_udp {
            let bind_ip = if peer_addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
            let sock = UdpSocket::bind(bind_ip).await?;
            sock.connect(peer_addr).await?;
            // Standard iperf3 UDP connect magic: 4-byte 123456789
            sock.send(&123_456_789_u32.to_be_bytes()).await?;
            udp_sockets.push(Arc::new(sock));
        } else {
            let mut s = TcpStream::connect(peer_addr).await?;
            s.set_nodelay(true)?;
            s.write_all(&cookie).await?;
            tcp_streams.push(s);
        }
    }

    if read_state(&mut ctrl).await? != TEST_START {
        anyhow::bail!("Expected TEST_START from server");
    }
    if read_state(&mut ctrl).await? != TEST_RUNNING {
        anyhow::bail!("Expected TEST_RUNNING from server");
    }

    let _ = tx
        .send(TelemetryMsg::Event(IperfEvent::Start(StartData {
            version: Some("pulse-native 1.0 (iperf3-wire)".into()),
            connecting_to: Some(Endpoint {
                host: host.to_string(),
                port: cfg.port,
            }),
            test_start: Some(TestStartInfo {
                protocol: Some(if is_udp { "UDP" } else { "TCP" }.into()),
                num_streams: Some(cfg.parallel_streams as u32),
                duration: Some(time),
                reverse: Some(if cfg.direction == Direction::ReverseDownload { 1 } else { 0 }),
                bidir: Some(if cfg.direction == Direction::Bidirectional { 1 } else { 0 }),
            }),
        })))
        .await;

    let stream_cancel = cancel.child_token();
    let mut worker_handles = Vec::new();
    let total_limit = match cfg.stop {
        StopCondition::Bytes(b) => Some(b),
        StopCondition::Blocks(k) => Some(k * blksize as u64),
        StopCondition::Seconds(_) => None,
    };

    if is_udp {
        let per_stream_bps = (bandwidth / cfg.parallel_streams.max(1) as u64).max(1_000);
        for (idx, sock) in udp_sockets.into_iter().enumerate() {
            let c = Arc::clone(&counters[idx]);
            let tok = stream_cancel.clone();
            let dir = cfg.direction;
            worker_handles.push(tokio::spawn(async move {
                if dir == Direction::ReverseDownload {
                    run_udp_recv_loop(sock, c, tok).await;
                } else {
                    run_udp_send_loop(sock, blksize, per_stream_bps, total_limit, c, tok).await;
                }
            }));
        }
    } else {
        for (idx, stream) in tcp_streams.into_iter().enumerate() {
            let c = Arc::clone(&counters[idx]);
            let tok = stream_cancel.clone();
            let dir = cfg.direction;
            worker_handles.push(tokio::spawn(async move {
                match dir {
                    Direction::Upload => run_tcp_send_loop(stream, blksize, total_limit, c, tok).await,
                    Direction::ReverseDownload => run_tcp_recv_loop(stream, blksize, c, tok).await,
                    Direction::Bidirectional => run_tcp_bidir_loop(stream, blksize, total_limit, c, tok).await,
                }
            }));
        }
    }

    // Telemetry ticker loop
    let started_at = Instant::now();
    let interval_dur = Duration::from_secs_f32(cfg.interval_secs.clamp(0.1, 60.0));
    let mut ticker = tokio::time::interval(interval_dur);
    ticker.tick().await; // skip immediate t=0 tick

    let mut prev_bytes = vec![0u64; counters.len()];
    let mut prev_elapsed = 0.0_f64;
    let max_dur = match cfg.stop {
        StopCondition::Seconds(s) => Some(Duration::from_secs(s as u64)),
        _ => None,
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                let now_sec = started_at.elapsed().as_secs_f64();
                let dt = (now_sec - prev_elapsed).max(0.001);
                let mut sum_bytes = 0u64;
                let mut stream_ticks = Vec::with_capacity(counters.len());
                let omitted = (now_sec as u32) < cfg.omit_seconds;

                for (i, c) in counters.iter().enumerate() {
                    let total = c.bytes.load(Ordering::Relaxed);
                    let delta = total.saturating_sub(prev_bytes[i]);
                    prev_bytes[i] = total;
                    sum_bytes += delta;
                    let bps = (delta as f64 * 8.0) / dt;
                    let jitter_ms = c.jitter_us.load(Ordering::Relaxed) as f64 / 1000.0;
                    let lost = c.lost.load(Ordering::Relaxed);
                    let pkts = c.packets.load(Ordering::Relaxed);
                    stream_ticks.push(StreamTick {
                        socket: Some(i as i32 + 1),
                        start: prev_elapsed,
                        end: now_sec,
                        bits_per_second: bps,
                        retransmits: if is_udp { None } else { Some(0) },
                        snd_cwnd: None,
                        jitter_ms: if is_udp { Some(jitter_ms) } else { None },
                        lost_packets: if is_udp { Some(lost) } else { None },
                        packets: if is_udp { Some(pkts) } else { None },
                        omitted: Some(omitted),
                    });
                }

                let sum_bps = (sum_bytes as f64 * 8.0) / dt;
                let avg_jitter = if is_udp {
                    let j: f64 = stream_ticks.iter().filter_map(|s| s.jitter_ms).sum();
                    Some(j / stream_ticks.len().max(1) as f64)
                } else {
                    None
                };
                let total_lost: u64 = counters.iter().map(|c| c.lost.load(Ordering::Relaxed)).sum();
                let total_pkts: u64 = counters.iter().map(|c| c.packets.load(Ordering::Relaxed)).sum();
                let loss_pct = if is_udp && total_pkts > 0 {
                    Some((total_lost as f64 / total_pkts as f64) * 100.0)
                } else if is_udp {
                    Some(0.0)
                } else {
                    None
                };

                let _ = tx.send(TelemetryMsg::Event(IperfEvent::Interval(IntervalData {
                    streams: stream_ticks,
                    sum: Some(SumTick {
                        start: prev_elapsed,
                        end: now_sec,
                        bits_per_second: sum_bps,
                        retransmits: if is_udp { None } else { Some(0) },
                        jitter_ms: avg_jitter,
                        lost_packets: if is_udp { Some(total_lost) } else { None },
                        packets: if is_udp { Some(total_pkts) } else { None },
                        lost_percent: loss_pct,
                        omitted: Some(omitted),
                    }),
                }))).await;

                prev_elapsed = now_sec;
                if let Some(limit) = max_dur {
                    if started_at.elapsed() >= limit {
                        break;
                    }
                }
                if worker_handles.iter().all(|h| h.is_finished()) {
                    break;
                }
            }
        }
    }

    stream_cancel.cancel();
    for h in worker_handles {
        let _ = h.await;
    }

    let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
    let _ = write_state(&mut ctrl, TEST_END).await;

    // Exchange results with server
    let mut remote_results: Option<WireResults> = None;
    if let Ok(EXCHANGE_RESULTS) = read_state(&mut ctrl).await {
        let local_res = build_wire_results(&counters, elapsed, is_udp);
        let _ = write_json_frame(&mut ctrl, &local_res).await;
        remote_results = read_json_frame::<WireResults>(&mut ctrl).await.ok();
        let _ = read_state(&mut ctrl).await; // DISPLAY_RESULTS
        let _ = write_state(&mut ctrl, IPERF_DONE).await;
    }

    let total_bytes: u64 = counters.iter().map(|c| c.bytes.load(Ordering::Relaxed)).sum();
    let local_bps = (total_bytes as f64 * 8.0) / elapsed;
    let (final_jitter, final_lost, final_pkts, final_loss_pct, remote_cpu) =
        if let Some(rem) = remote_results {
            let r_lost: u64 = rem.streams.iter().map(|s| s.errors).sum();
            let r_pkts: u64 = rem.streams.iter().map(|s| s.packets).sum();
            let r_jit: f64 = if rem.streams.is_empty() {
                0.0
            } else {
                rem.streams.iter().map(|s| s.jitter * 1000.0).sum::<f64>() / rem.streams.len() as f64
            };
            let pct = if r_pkts > 0 { (r_lost as f64 / r_pkts as f64) * 100.0 } else { 0.0 };
            (
                if is_udp { Some(r_jit) } else { None },
                if is_udp { Some(r_lost) } else { None },
                if is_udp { Some(r_pkts) } else { None },
                if is_udp { Some(pct) } else { None },
                Some(rem.cpu_util_total),
            )
        } else {
            (None, None, None, None, None)
        };

    let sum_tick = SumTick {
        start: 0.0,
        end: elapsed,
        bits_per_second: local_bps,
        retransmits: if is_udp { None } else { Some(0) },
        jitter_ms: final_jitter,
        lost_packets: final_lost,
        packets: final_pkts,
        lost_percent: final_loss_pct,
        omitted: Some(false),
    };

    let _ = tx
        .send(TelemetryMsg::Event(IperfEvent::End(EndData {
            streams: vec![EndStreamPair {
                sender: Some(EndStreamStats {
                    bits_per_second: Some(local_bps),
                    retransmits: if is_udp { None } else { Some(0) },
                    max_rtt: None,
                    min_rtt: None,
                    mean_rtt: None,
                }),
                receiver: None,
            }],
            sum_sent: if is_udp { None } else { Some(sum_tick.clone()) },
            sum_received: if is_udp { None } else { Some(sum_tick.clone()) },
            sum: if is_udp { Some(sum_tick) } else { None },
            cpu_utilization_percent: remote_cpu.map(|r| CpuUtil {
                host_total: 0.0,
                remote_total: r,
            }),
        })))
        .await;

    Ok(())
}

async fn run_native_server(
    cfg: &TestConfig,
    one_off: bool,
    tx: &mpsc::Sender<TelemetryMsg>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let bind_ip = cfg
        .bind_addr
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "0.0.0.0".into());
    let bind_addr: SocketAddr = format!("{bind_ip}:{}", cfg.port).parse()?;
    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;

    let _ = tx
        .send(TelemetryMsg::RawLog(format!(
            "Pulse native server listening on {local_addr}"
        )))
        .await;

    loop {
        let (mut ctrl, peer) = tokio::select! {
            _ = cancel.cancelled() => break,
            res = listener.accept() => res?,
        };
        ctrl.set_nodelay(true)?;

        let mut cookie = [0u8; COOKIE_SIZE];
        if ctrl.read_exact(&mut cookie).await.is_err() {
            continue;
        }

        write_state(&mut ctrl, PARAM_EXCHANGE).await?;
        let params: WireParams = read_json_frame(&mut ctrl).await?;
        write_state(&mut ctrl, CREATE_STREAMS).await?;

        let is_udp = params.udp.unwrap_or(false);
        let n_streams = params.parallel.max(1) as usize;
        let mut counters = Vec::with_capacity(n_streams);
        let stream_cancel = cancel.child_token();
        let mut workers = Vec::new();

        if is_udp {
            let udp_sock = Arc::new(UdpSocket::bind(local_addr).await?);
            let mut probe = [0u8; 64];
            let (_, client_udp_addr) = tokio::select! {
                _ = cancel.cancelled() => break,
                res = udp_sock.recv_from(&mut probe) => res?,
            };
            udp_sock.connect(client_udp_addr).await?;
            counters.push(Arc::new(StreamCounters::default()));
            let c = Arc::clone(&counters[0]);
            let tok = stream_cancel.clone();
            let blk = params.len.max(64);
            let bw = params.bandwidth.max(1_000_000);
            let rev = params.reverse;
            workers.push(tokio::spawn(async move {
                if rev {
                    run_udp_send_loop(udp_sock, blk, bw, None, c, tok).await;
                } else {
                    run_udp_recv_loop(udp_sock, c, tok).await;
                }
            }));
        } else {
            for _ in 0..n_streams {
                let (mut s, _) = tokio::select! {
                    _ = cancel.cancelled() => break,
                    res = listener.accept() => res?,
                };
                s.set_nodelay(true)?;
                let mut scookie = [0u8; COOKIE_SIZE];
                s.read_exact(&mut scookie).await?;
                if scookie != cookie {
                    continue;
                }
                let c = Arc::new(StreamCounters::default());
                counters.push(Arc::clone(&c));
                let tok = stream_cancel.clone();
                let blk = params.len.max(1024);
                let rev = params.reverse;
                let bidir = params.bidirectional;
                workers.push(tokio::spawn(async move {
                    if bidir {
                        run_tcp_bidir_loop(s, blk, None, c, tok).await;
                    } else if rev {
                        run_tcp_send_loop(s, blk, None, c, tok).await;
                    } else {
                        run_tcp_recv_loop(s, blk, c, tok).await;
                    }
                }));
            }
        }

        write_state(&mut ctrl, TEST_START).await?;
        write_state(&mut ctrl, TEST_RUNNING).await?;

        let _ = tx
            .send(TelemetryMsg::Event(IperfEvent::Start(StartData {
                version: Some("pulse-native 1.0 (server)".into()),
                connecting_to: Some(Endpoint {
                    host: peer.ip().to_string(),
                    port: peer.port(),
                }),
                test_start: None,
            })))
            .await;

        let started_at = Instant::now();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.tick().await;
        let mut prev_bytes = vec![0u64; counters.len()];
        let mut prev_t = 0.0_f64;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                state_res = read_state(&mut ctrl) => {
                    if matches!(state_res, Ok(TEST_END) | Err(_)) {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    let now = started_at.elapsed().as_secs_f64();
                    let dt = (now - prev_t).max(0.001);
                    let mut sum_delta = 0u64;
                    for (i, c) in counters.iter().enumerate() {
                        let cur = c.bytes.load(Ordering::Relaxed);
                        sum_delta += cur.saturating_sub(prev_bytes[i]);
                        prev_bytes[i] = cur;
                    }
                    let bps = (sum_delta as f64 * 8.0) / dt;
                    let _ = tx.send(TelemetryMsg::Event(IperfEvent::Interval(IntervalData {
                        streams: vec![],
                        sum: Some(SumTick {
                            start: prev_t,
                            end: now,
                            bits_per_second: bps,
                            retransmits: if is_udp { None } else { Some(0) },
                            jitter_ms: None,
                            lost_packets: None,
                            packets: None,
                            lost_percent: None,
                            omitted: Some(false),
                        }),
                    }))).await;
                    prev_t = now;
                }
            }
        }

        stream_cancel.cancel();
        for w in workers {
            let _ = w.await;
        }

        let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
        let _ = write_state(&mut ctrl, EXCHANGE_RESULTS).await;
        let _ = read_json_frame::<WireResults>(&mut ctrl).await;
        let server_res = build_wire_results(&counters, elapsed, is_udp);
        let _ = write_json_frame(&mut ctrl, &server_res).await;
        let _ = write_state(&mut ctrl, DISPLAY_RESULTS).await;
        let _ = read_state(&mut ctrl).await; // IPERF_DONE

        if one_off {
            break;
        }
    }

    Ok(())
}

fn build_wire_results(counters: &[Arc<StreamCounters>], elapsed: f64, is_udp: bool) -> WireResults {
    let streams = counters
        .iter()
        .enumerate()
        .map(|(i, c)| WireStreamResult {
            id: i as i32 + 1,
            bytes: c.bytes.load(Ordering::Relaxed),
            retransmits: if is_udp { -1 } else { 0 },
            jitter: c.jitter_us.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            errors: c.lost.load(Ordering::Relaxed),
            packets: c.packets.load(Ordering::Relaxed),
            start_time: 0.0,
            end_time: elapsed,
        })
        .collect();
    WireResults {
        streams,
        ..Default::default()
    }
}

async fn run_tcp_send_loop(
    mut stream: TcpStream,
    blksize: usize,
    limit: Option<u64>,
    counter: Arc<StreamCounters>,
    cancel: CancellationToken,
) {
    let buf = vec![0xA5u8; blksize];
    let mut sent = 0u64;
    while !cancel.is_cancelled() {
        let to_write = match limit {
            Some(max) if sent >= max => break,
            Some(max) => ((max - sent) as usize).min(blksize),
            None => blksize,
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            res = stream.write_all(&buf[..to_write]) => {
                if res.is_err() { break; }
                sent += to_write as u64;
                counter.bytes.fetch_add(to_write as u64, Ordering::Relaxed);
            }
        }
    }
}

async fn run_tcp_recv_loop(
    mut stream: TcpStream,
    blksize: usize,
    counter: Arc<StreamCounters>,
    cancel: CancellationToken,
) {
    let mut buf = vec![0u8; blksize];
    while !cancel.is_cancelled() {
        tokio::select! {
            _ = cancel.cancelled() => break,
            res = stream.read(&mut buf) => {
                match res {
                    Ok(0) | Err(_) => break,
                    Ok(n) => { counter.bytes.fetch_add(n as u64, Ordering::Relaxed); }
                }
            }
        }
    }
}

async fn run_tcp_bidir_loop(
    stream: TcpStream,
    blksize: usize,
    limit: Option<u64>,
    counter: Arc<StreamCounters>,
    cancel: CancellationToken,
) {
    let (mut rd, mut wr) = stream.into_split();
    let c_rd = Arc::clone(&counter);
    let t_rd = cancel.clone();
    let r_task = tokio::spawn(async move {
        let mut buf = vec![0u8; blksize];
        while !t_rd.is_cancelled() {
            tokio::select! {
                _ = t_rd.cancelled() => break,
                res = rd.read(&mut buf) => {
                    match res {
                        Ok(0) | Err(_) => break,
                        Ok(n) => { c_rd.bytes.fetch_add(n as u64, Ordering::Relaxed); }
                    }
                }
            }
        }
    });

    let buf = vec![0x5Au8; blksize];
    let mut sent = 0u64;
    while !cancel.is_cancelled() {
        if let Some(max) = limit {
            if sent >= max { break; }
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            res = wr.write_all(&buf) => {
                if res.is_err() { break; }
                sent += blksize as u64;
                counter.bytes.fetch_add(blksize as u64, Ordering::Relaxed);
            }
        }
    }
    let _ = r_task.await;
}

async fn run_udp_send_loop(
    sock: Arc<UdpSocket>,
    blksize: usize,
    target_bps: u64,
    limit: Option<u64>,
    counter: Arc<StreamCounters>,
    cancel: CancellationToken,
) {
    let mut buf = vec![0u8; blksize.max(16)];
    let pkt_bits = (blksize as f64) * 8.0;
    let interval = Duration::from_secs_f64((pkt_bits / target_bps as f64).max(0.000_05));
    let mut seq: u64 = 1;
    let mut sent_bytes: u64 = 0;
    let start = Instant::now();

    while !cancel.is_cancelled() {
        if let Some(max) = limit {
            if sent_bytes >= max { break; }
        }
        let elapsed = start.elapsed();
        let sec = elapsed.as_secs() as u32;
        let usec = elapsed.subsec_micros();
        buf[0..4].copy_from_slice(&sec.to_be_bytes());
        buf[4..8].copy_from_slice(&usec.to_be_bytes());
        buf[8..16].copy_from_slice(&seq.to_be_bytes());

        if sock.send(&buf).await.is_err() {
            break;
        }
        seq += 1;
        sent_bytes += buf.len() as u64;
        counter.bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
        counter.packets.fetch_add(1, Ordering::Relaxed);

        let next_at = interval.saturating_mul(seq as u32);
        if let Some(wait) = next_at.checked_sub(start.elapsed()) {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }
}

async fn run_udp_recv_loop(
    sock: Arc<UdpSocket>,
    counter: Arc<StreamCounters>,
    cancel: CancellationToken,
) {
    let mut buf = vec![0u8; 65535];
    let mut expected_seq: u64 = 1;
    let mut prev_transit: Option<f64> = None;
    let mut jitter_us: f64 = 0.0;
    let start = Instant::now();

    while !cancel.is_cancelled() {
        tokio::select! {
            _ = cancel.cancelled() => break,
            res = sock.recv(&mut buf) => {
                match res {
                    Ok(n) if n >= 16 => {
                        counter.bytes.fetch_add(n as u64, Ordering::Relaxed);
                        counter.packets.fetch_add(1, Ordering::Relaxed);
                        let send_sec = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as f64;
                        let send_usec = u32::from_be_bytes(buf[4..8].try_into().unwrap()) as f64;
                        let seq = u64::from_be_bytes(buf[8..16].try_into().unwrap());
                        if seq > expected_seq {
                            counter.lost.fetch_add(seq - expected_seq, Ordering::Relaxed);
                        }
                        if seq >= expected_seq {
                            expected_seq = seq + 1;
                        }
                        let send_us = send_sec * 1_000_000.0 + send_usec;
                        let recv_us = start.elapsed().as_micros() as f64;
                        let transit = recv_us - send_us;
                        if let Some(prev) = prev_transit {
                            let d = (transit - prev).abs();
                            jitter_us += (d - jitter_us) / 16.0;
                            counter.jitter_us.store(jitter_us as u64, Ordering::Relaxed);
                        }
                        prev_transit = Some(transit);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Preset;

    #[tokio::test]
    async fn native_tcp_loopback_transfer_succeeds() {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let srv_cfg = TestConfig {
            mode: Mode::Server { one_off: true },
            port,
            ..TestConfig::default()
        };
        let mut cli_cfg = TestConfig::from_preset(Preset::MaxSpeed, "127.0.0.1".into());
        cli_cfg.port = port;
        cli_cfg.stop = StopCondition::Seconds(1);
        cli_cfg.omit_seconds = 0;
        cli_cfg.interval_secs = 0.25;
        cli_cfg.parallel_streams = 2;

        let (srv_tx, _srv_rx) = mpsc::channel(64);
        let (cli_tx, mut cli_rx) = mpsc::channel(64);
        let srv_cancel = CancellationToken::new();
        let cli_cancel = CancellationToken::new();

        let srv_handle = tokio::spawn(run_native(srv_cfg, srv_tx, srv_cancel.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let cli_handle = tokio::spawn(run_native(cli_cfg, cli_tx, cli_cancel));

        let mut saw_end = false;
        let mut max_bps = 0.0_f64;
        while let Some(msg) = cli_rx.recv().await {
            match msg {
                TelemetryMsg::Event(IperfEvent::Interval(iv)) => {
                    if let Some(s) = iv.sum {
                        max_bps = max_bps.max(s.bits_per_second);
                    }
                }
                TelemetryMsg::Event(IperfEvent::End(end)) => {
                    saw_end = true;
                    if let Some(s) = end.sum_sent {
                        max_bps = max_bps.max(s.bits_per_second);
                    }
                }
                TelemetryMsg::Exited(code) => {
                    assert_eq!(code, Some(0));
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_end);
        assert!(max_bps > 1_000_000.0, "expected >1 Mbps loopback, got {max_bps}");
        srv_cancel.cancel();
        let _ = srv_handle.await;
        let _ = cli_handle.await;
    }

    #[tokio::test]
    async fn native_udp_loopback_reports_jitter_and_loss() {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let srv_cfg = TestConfig {
            mode: Mode::Server { one_off: true },
            port,
            ..TestConfig::default()
        };
        let mut cli_cfg = TestConfig::from_preset(Preset::VoipGaming, "127.0.0.1".into());
        cli_cfg.port = port;
        cli_cfg.direction = Direction::Upload;
        cli_cfg.stop = StopCondition::Seconds(1);
        cli_cfg.interval_secs = 0.25;

        let (srv_tx, _srv_rx) = mpsc::channel(64);
        let (cli_tx, mut cli_rx) = mpsc::channel(64);
        let srv_cancel = CancellationToken::new();
        let cli_cancel = CancellationToken::new();

        let srv_handle = tokio::spawn(run_native(srv_cfg, srv_tx, srv_cancel.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let cli_handle = tokio::spawn(run_native(cli_cfg, cli_tx, cli_cancel));

        let mut saw_udp_end = false;
        while let Some(msg) = cli_rx.recv().await {
            match msg {
                TelemetryMsg::Event(IperfEvent::End(end)) => {
                    if let Some(s) = end.sum {
                        saw_udp_end = s.jitter_ms.is_some() && s.lost_percent.is_some();
                    }
                }
                TelemetryMsg::Exited(code) => {
                    assert_eq!(code, Some(0));
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_udp_end);
        srv_cancel.cancel();
        let _ = srv_handle.await;
        let _ = cli_handle.await;
    }
}
