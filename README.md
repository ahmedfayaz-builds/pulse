# Pulse

Standalone desktop network throughput and stability tester written in Rust and Slint.

Pulse talks the `iperf3` wire protocol out of the box—no Java runtime, no Electron, and no separate `iperf3` binary required. You can run Client-to-Server tests between two Pulse instances or point Pulse at any existing `iperf3` server.

## Features

- **Single-binary standalone:** Built-in `tokio` TCP/UDP engine (`pulse-core::engine::run_native`) implementing the `iperf3` control handshake and data streams. Optional external `iperf3` binary override is available in the Pro drawer.
- **Meter & Graph views:** Switch between a 270° logarithmic speedometer dial with an ambient wave and a full-height real-time area graph.
- **Diagnostic presets:**
  - **Max Speed:** 4 parallel TCP streams, 10s, 2s warm-up omitted (`-O 2`), reverse download (`-R`).
  - **VoIP / Gaming:** 10 Mbps paced UDP, bidirectional, DSCP `EF`, measures jitter and packet loss.
  - **Wi-Fi Roaming:** Single TCP stream, 30s at 0.5s sample resolution to spot signal drops while moving.
- **Plain-English health verdicts:** Translates raw retransmits, jitter, and packet loss into `HEALTHY` / `DEGRADED` / `CRITICAL` status and readiness pills (`4K Stream`, `Calls`, `Gaming`).
- **Session exports:** Export completed runs as a **PDF** report (with embedded vector throughput graph and interval table), **CSV** raw data, or **TXT** summary.
- **History & themes:** Remembers recent target IPs, past test runs, and Light/Dark mode preference in local app data.

## Quick Start

1. Download the binary for your OS from [Releases](../../releases).
2. Run Pulse on one machine in **Server** mode and press **Listen** (default port `5201`).
3. Run Pulse on a second machine in **Client** mode, enter the server IP, pick a preset, and press **Start Test**.

## Building from Source

Requires Rust (`1.75+`). On Linux, install Slint's font/windowing dev packages:

```bash
sudo apt-get install -y libfontconfig1-dev libxkbcommon-dev libwayland-dev libx11-dev libgl1-mesa-dev
```

Build and run:

```bash
cargo test --workspace
cargo build --release -p pulse-ui
```

The release binary is written to `target/release/pulse-ui` (`pulse-ui.exe` on Windows).

## Documentation

- [Architecture](docs/architecture.md) — Workspace layout, pure-Rust `iperf3` wire state machine, and Slint UI bridge.
- [Protocol & Metrics](docs/protocol-and-metrics.md) — TCP vs UDP metrics, health thresholds, export formats, and local history storage.

## License

[MIT](LICENSE)
