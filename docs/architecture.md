# Architecture

Pulse is split into two crates:

```text
crates/
├── pulse-core/          # Engine, protocol, config, health, history, export
│   └── src/
│       ├── config.rs    # TestConfig, Preset, validation, CLI arg builder
│       ├── engine.rs    # Pure-Rust iperf3 wire-protocol client & server
│       ├── runner.rs    # Optional external iperf3 subprocess runner
│       ├── parser.rs    # NDJSON (--json-stream) + console regex fallback
│       ├── health.rs    # HealthStatus thresholds & verdict builder
│       ├── report.rs    # CSV, plain-text, and PDF 1.4 vector report generator
│       └── history.rs   # Persistent recent hosts, theme, and run history
└── pulse-ui/            # Native Slint 1.9 desktop frontend
    ├── build.rs         # Compiles ui/app.slint and embeds Windows .ico
    ├── assets/          # pulse.png (window icon) & pulse.ico (Win32 exe icon)
    ├── ui/app.slint     # Declarative UI, Light/Dark palette, Meter/Graph views
    └── src/main.rs      # Tokio runtime + Slint event-loop bridge + rfd dialogs
```

## Pure-Rust `iperf3` Wire Engine (`pulse-core::engine`)

`run_native` implements the `iperf3` TCP control channel and parallel data streams using `tokio::net`:

1. **Handshake:** Client connects to `host:port` over TCP and sends a 37-byte ASCII session cookie.
2. **Parameter Exchange (`PARAM_EXCHANGE = 9`):** Server requests parameters; client sends a 4-byte big-endian length-prefixed JSON payload (`tcp`/`udp`, `parallel`, `len`, `time`, `num`, `blockcount`, `bandwidth`, `reverse`, `bidirectional`, `omit`).
3. **Stream Setup (`CREATE_STREAMS = 10`):** Client opens `parallel` TCP data sockets (sending the 37-byte cookie on each) or UDP sockets (sending the 4-byte `123456789` probe).
4. **Execution (`TEST_START = 1`, `TEST_RUNNING = 2`):** Data workers run concurrently while a ticker emits `IperfEvent::Interval` telemetry over an `mpsc` channel.
5. **Teardown (`TEST_END = 4`, `EXCHANGE_RESULTS = 13`, `IPERF_DONE = 16`):** Client and server exchange final JSON `WireResults` (bytes, jitter, packet loss) and emit `IperfEvent::End`.

## External `iperf3` Fallback (`pulse-core::runner`)

If a user supplies a custom `iperf3` binary path in the Pro drawer, `run_iperf` spawns it with `--json --json-stream --forceflush`, reads stdout line-by-line via `BufReader`, and falls back to `parse_console_fallback` for older `iperf3` releases without `--json-stream`.

## UI Threading Model

- `tokio::runtime::Runtime` runs all network I/O and timers off the main thread.
- `AppWindow` callbacks spawn tasks onto the Tokio handle and hold a `CancellationToken` for clean test abort.
- Telemetry updates marshal back to the Slint UI thread via `slint::invoke_from_event_loop`.
