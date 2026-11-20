# Protocol & Metrics

## Why Jitter & Packet Loss Show `N/A (TCP)` in TCP Mode

TCP and UDP measure different transport properties:

- **TCP (`Max Speed`, `Wi-Fi Roaming`):** The OS kernel guarantees ordered, lossless delivery by resending dropped segments. `Retransmits` tracks those retries; UDP jitter and datagram loss do not apply.
- **UDP (`VoIP / Gaming`):** Sends fixed-size datagrams with a `(sec, usec, seq)` header at a target bit rate (`-b`). The receiver computes inter-arrival **jitter** (RFC 3550 EWMA) and counts sequence gaps as **packet loss**.

## Health Thresholds (`pulse-core::health`)

| Metric | Healthy | Warning (`DEGRADED`) | Critical |
| :--- | :--- | :--- | :--- |
| **TCP Retransmits** | `0 – 5` / interval | `6 – 50` / interval | `> 50` / interval |
| **UDP Jitter** | `< 5.0 ms` | `5.0 – 20.0 ms` | `> 20.0 ms` |
| **Packet Loss** | `<= 0.1%` | `0.1% – 1.0%` | `> 1.0%` |

The overall verdict takes the worst severity across active metrics.

## Report Exports (`pulse-core::report`)

Completed sessions can be exported via native OS save dialogs:

- **PDF (`.pdf`):** Pure-Rust PDF 1.4 document containing test metadata, health verdict, a native vector throughput area chart, and up to 28 interval rows.
- **CSV (`.csv`):** Raw interval rows (`start_sec,end_sec,mbps,retransmits,jitter_ms,loss_percent,omitted`).
- **Text (`.txt`):** Plain-text summary and formatted interval table.

## Local History Storage (`pulse-core::history`)

Pulse stores recent targets (up to 10), theme preference, and the last 30 completed runs in JSON:

- **Windows:** `%LOCALAPPDATA%\Pulse\history.json`
- **Linux / macOS:** `$XDG_DATA_HOME/Pulse/history.json` (or `~/.local/share/Pulse/history.json`)
