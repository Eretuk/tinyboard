use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::{db::Database, models::Host, state::AppState};

pub struct UptimeMonitor {
    client: reqwest::Client,
}

impl UptimeMonitor {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    /// Probe a single host.
    /// - check_url starts with http(s):// → HTTP GET (any response = online)
    /// - check_url is an IP/hostname (no scheme) → TCP connect on port 80
    ///   (true ICMP ping requires root; TCP connect is a practical substitute)
    /// Returns (online, duration_ms).
    pub async fn scan_host(&self, host: &Host) -> (bool, i64) {
        let target = if !host.check_url.is_empty() {
            host.check_url.clone()
        } else if !host.url.is_empty() {
            host.url.clone()
        } else if !host.addr.is_empty() {
            host.addr.clone()
        } else {
            warn!("Host '{}' has no check URL configured, skipping", host.name);
            return (false, 0);
        };

        let start = Instant::now();

        if target.starts_with("http://") || target.starts_with("https://") {
            self.http_check(&target, &host.name, start).await
        } else {
            let addr = if target.contains(':') {
                target.clone()
            } else {
                format!("{}:80", target)
            };
            self.tcp_check(&addr, &host.name, start).await
        }
    }

    async fn http_check(&self, url: &str, name: &str, start: Instant) -> (bool, i64) {
        match self.client.get(url).send().await {
            Ok(resp) => {
                let ms = start.elapsed().as_millis() as i64;
                info!("Host '{}' online via HTTP {} ({} ms)", name, resp.status(), ms);
                (true, ms)
            }
            Err(e) => {
                let ms = start.elapsed().as_millis() as i64;
                warn!("Host '{}' HTTP check failed: {} ({} ms)", name, e, ms);
                (false, ms)
            }
        }
    }

    async fn tcp_check(&self, addr: &str, name: &str, start: Instant) -> (bool, i64) {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(_) => {
                let ms = start.elapsed().as_millis() as i64;
                info!("Host '{}' online via TCP ({} ms)", name, ms);
                (true, ms)
            }
            Err(e) => {
                let ms = start.elapsed().as_millis() as i64;
                warn!("Host '{}' TCP check failed: {} ({} ms)", name, e, ms);
                (false, ms)
            }
        }
    }
}

/// A single host entry tracked by the per-host scheduler.
#[derive(Clone)]
struct TrackedHost {
    panel: String,
    host: Host,
    interval_secs: u64,
}

/// Background task: each monitored host gets its own independent timer.
/// Hosts are scanned in parallel — a slow/timing-out host never blocks others.
/// The UI read lock is never held during network I/O.
pub async fn start_uptime_monitoring(state: Arc<RwLock<AppState>>) {
    info!("Uptime monitoring task started");

    // last_scan: (panel, host_name) → Instant of last completed scan
    let mut last_scan: HashMap<(String, String), Instant> = HashMap::new();
    // Stagger first run so all hosts don't fire simultaneously on startup
    let mut first_run = true;

    loop {
        // ── 1. Snapshot state without holding the lock during I/O ────────────
        let (tracked, db_path, global_interval, trim_days) = {
            let s = state.read().await;
            let global_interval = s.config.scan_interval.parse::<u64>().unwrap_or(60);
            let trim_days = s.config.db_trim_days.parse::<i64>().unwrap_or(30);
            let db_path = s.db_path.clone();

            let mut tracked: Vec<TrackedHost> = Vec::new();
            for (panel_name, panel) in &s.links.panels {
                for host in panel.hosts.values() {
                    if !host.scan { continue; }
                    let interval = if host.scan_interval > 0 {
                        host.scan_interval
                    } else {
                        global_interval
                    };
                    tracked.push(TrackedHost {
                        panel: panel_name.clone(),
                        host: host.clone(),
                        interval_secs: interval,
                    });
                }
            }
            (tracked, db_path, global_interval, trim_days)
        };
        // State lock is released here — all I/O below is lock-free

        if tracked.is_empty() {
            sleep(Duration::from_secs(global_interval.max(10))).await;
            continue;
        }

        // ── 2. Determine which hosts are due for a scan ───────────────────────
        let now = Instant::now();
        let due: Vec<TrackedHost> = tracked.into_iter().filter(|t| {
            if first_run { return true; }
            let key = (t.panel.clone(), t.host.name.clone());
            match last_scan.get(&key) {
                None => true,
                Some(last) => now.duration_since(*last).as_secs() >= t.interval_secs,
            }
        }).collect();
        first_run = false;

        if !due.is_empty() {
            // ── 3. Scan all due hosts in parallel ─────────────────────────────
            let monitor = Arc::new(UptimeMonitor::new());
            let db_path_clone = db_path.clone();

            let tasks: Vec<_> = due.iter().map(|t| {
                let monitor = Arc::clone(&monitor);
                let host = t.host.clone();
                let panel = t.panel.clone();
                let db_path = db_path_clone.clone();
                tokio::spawn(async move {
                    let (status, duration_ms) = monitor.scan_host(&host).await;
                    save_record(&db_path, &panel, &host.name, status, duration_ms);
                    (panel, host.name, status, duration_ms)
                })
            }).collect();

            // Await all parallel scans
            for (t, task) in due.iter().zip(tasks) {
                match task.await {
                    Ok((panel, host_name, _, _)) => {
                        last_scan.insert((panel, host_name), Instant::now());
                    }
                    Err(e) => {
                        error!("Scan task panicked for host '{}': {}", t.host.name, e);
                    }
                }
            }

            // ── 4. Trim old records once per cycle ────────────────────────────
            if let Ok(db) = Database::new(&db_path) {
                if let Err(e) = db.trim_old_records(trim_days) {
                    error!("Failed to trim old uptime records: {}", e);
                }
            }
        }

        // ── 5. Sleep until the next host is due ───────────────────────────────
        // Find the minimum remaining time across all tracked hosts
        let sleep_secs = {
            let s = state.read().await;
            let global = s.config.scan_interval.parse::<u64>().unwrap_or(60);
            let now2 = Instant::now();
            let mut min_wait = global;
            for (panel_name, panel) in &s.links.panels {
                for host in panel.hosts.values() {
                    if !host.scan { continue; }
                    let interval = if host.scan_interval > 0 { host.scan_interval } else { global };
                    let key = (panel_name.clone(), host.name.clone());
                    let elapsed = last_scan.get(&key)
                        .map(|l| now2.duration_since(*l).as_secs())
                        .unwrap_or(interval);
                    let remaining = interval.saturating_sub(elapsed);
                    min_wait = min_wait.min(remaining);
                }
            }
            min_wait.max(1) // never spin faster than 1s
        };

        sleep(Duration::from_secs(sleep_secs)).await;
    }
}

/// Write a single uptime record to the database.
/// Opens a fresh connection per write so the DB handle is never shared across threads.
pub fn save_record_pub(db_path: &Path, panel: &str, host_name: &str, status: bool, duration_ms: i64) {
    let record = crate::db::UptimeRecord {
        panel: panel.to_string(),
        host: host_name.to_string(),
        status,
        timestamp: time::OffsetDateTime::now_utc(),
        duration: duration_ms,
    };
    match Database::new(db_path) {
        Ok(db) => {
            if let Err(e) = db.save_uptime_record(&record) {
                error!("Failed to save uptime record for '{}': {}", host_name, e);
            }
        }
        Err(e) => error!("Failed to open DB for '{}': {}", host_name, e),
    }
}

// Keep save_record as private alias used internally
fn save_record(db_path: &Path, panel: &str, host_name: &str, status: bool, duration_ms: i64) {
    save_record_pub(db_path, panel, host_name, status, duration_ms);
}
