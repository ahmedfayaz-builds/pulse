use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub timestamp_unix: u64,
    pub host: String,
    pub port: u16,
    pub protocol: String,
    pub avg_mbps: f64,
    pub peak_mbps: f64,
    pub health_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppHistory {
    pub last_host: String,
    pub last_port: u16,
    pub dark_mode: bool,
    pub recent_hosts: Vec<String>,
    pub runs: Vec<RunRecord>,
}

impl Default for AppHistory {
    fn default() -> Self {
        Self {
            last_host: "127.0.0.1".into(),
            last_port: 5201,
            dark_mode: true,
            recent_hosts: vec!["127.0.0.1".into()],
            runs: Vec::new(),
        }
    }
}

impl AppHistory {
    pub fn storage_path() -> PathBuf {
        #[cfg(windows)]
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        #[cfg(not(windows))]
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));

        base.join("Pulse").join("history.json")
    }

    pub fn load() -> Self {
        let path = Self::storage_path();
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(parsed) = serde_json::from_str::<Self>(&raw) {
                return parsed;
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        let path = Self::storage_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Records a used host IP/hostname at the front of `recent_hosts` (deduped, max 10).
    pub fn record_host(&mut self, host: &str, port: u16) {
        let h = host.trim().to_string();
        if h.is_empty() {
            return;
        }
        self.last_host = h.clone();
        self.last_port = port;
        self.recent_hosts.retain(|x| x != &h);
        self.recent_hosts.insert(0, h);
        self.recent_hosts.truncate(10);
        self.save();
    }

    /// Records a completed test run (max 30 entries, newest first).
    pub fn record_run(&mut self, run: RunRecord) {
        self.runs.insert(0, run);
        self.runs.truncate(30);
        self.save();
    }

    /// Clears saved run history and resets recent targets to the current host.
    pub fn clear(&mut self) {
        self.recent_hosts = vec![self.last_host.clone()];
        self.runs.clear();
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_host_deduplicates_and_caps_at_ten() {
        let mut h = AppHistory::default();
        for i in 1..=12 {
            h.recent_hosts.retain(|x| x != &format!("10.0.0.{i}"));
            h.recent_hosts.insert(0, format!("10.0.0.{i}"));
            h.recent_hosts.truncate(10);
        }
        assert_eq!(h.recent_hosts.len(), 10);
        assert_eq!(h.recent_hosts[0], "10.0.0.12");
    }
}
