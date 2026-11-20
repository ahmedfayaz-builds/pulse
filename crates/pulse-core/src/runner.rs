use crate::parser::{parse_stdout_line, TelemetryMsg};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Resolves the iperf3 binary: custom path -> adjacent to Pulse executable -> system PATH.
pub fn resolve_iperf3_binary(custom_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = custom_path {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            let pb = PathBuf::from(trimmed);
            if pb.is_file() {
                return Some(pb);
            }
        }
    }

    let bin_name = if cfg!(windows) { "iperf3.exe" } else { "iperf3" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join(bin_name);
            if bundled.is_file() {
                return Some(bundled);
            }
        }
    }

    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin_name))
            .find(|p| p.is_file())
    })
}

/// Spawns `iperf3` asynchronously, streams parsed telemetry, and supports graceful cancellation.
pub async fn run_iperf(
    bin_path: PathBuf,
    args: Vec<String>,
    tx: mpsc::Sender<TelemetryMsg>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut cmd = Command::new(&bin_path);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    #[cfg(windows)]
    {
        // Prevent console window flash when launched from GUI subsystem.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let tx_err = tx.clone();
    tokio::spawn(async move {
        let mut err_lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = err_lines.next_line().await {
            let _ = tx_err.send(TelemetryMsg::ErrorLog(line)).await;
        }
    });

    let mut out_lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                send_graceful_interrupt(&child);
                let _ = tokio::time::timeout(std::time::Duration::from_millis(1200), child.wait()).await;
                let _ = child.kill().await;
                break;
            }
            line = out_lines.next_line() => {
                match line? {
                    Some(l) => {
                        let msg = parse_stdout_line(&l);
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    let status = child.wait().await?;
    let _ = tx.send(TelemetryMsg::Exited(status.code())).await;
    Ok(())
}

fn send_graceful_interrupt(child: &tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGINT);
        }
    }
    #[cfg(windows)]
    {
        let _ = child.id();
        // CREATE_NO_WINDOW processes lack an attached console; fallback kill follows timeout.
    }
}
