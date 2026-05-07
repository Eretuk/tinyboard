/// Uptime storage backed by redb — a pure-Rust embedded database.
///
/// Table layout
/// ────────────
/// Table name : "uptime"
/// Key        : [u8; N]  — big-endian u64 timestamp (8 bytes)
///                          + '\0' separator (1 byte)
///                          + panel name bytes
///                          + '\0' separator (1 byte)
///                          + host name bytes
///
/// Value      : [u8; N]  — status (1 byte: 0/1)
///                          + duration_ms as big-endian i64 (8 bytes)
///
/// Because keys are stored in lexicographic order and the timestamp is the
/// first 8 bytes in big-endian, range scans by time are O(log n) + result set.
use std::path::Path;

use anyhow::{Context, Result};
use redb::{Database as ReDb, ReadableTable, TableDefinition};
use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone)]
pub struct UptimeRecord {
    pub panel: String,
    pub host: String,
    pub status: bool,
    pub timestamp: OffsetDateTime,
    pub duration: i64,
}

const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("uptime");

pub struct Database {
    db: ReDb,
}

// ── Key / value encoding ──────────────────────────────────────────────────────

fn encode_key(ts: i64, panel: &str, host: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(8 + 1 + panel.len() + 1 + host.len());
    key.extend_from_slice(&ts.to_be_bytes());
    key.push(0);
    key.extend_from_slice(panel.as_bytes());
    key.push(0);
    key.extend_from_slice(host.as_bytes());
    key
}

fn encode_value(status: bool, duration_ms: i64) -> [u8; 9] {
    let mut val = [0u8; 9];
    val[0] = status as u8;
    val[1..9].copy_from_slice(&duration_ms.to_be_bytes());
    val
}

fn decode_key(key: &[u8]) -> Option<(i64, String, String)> {
    if key.len() < 10 { return None; }
    let ts = i64::from_be_bytes(key[..8].try_into().ok()?);
    let rest = &key[9..]; // skip separator byte
    let sep = rest.iter().position(|&b| b == 0)?;
    let panel = String::from_utf8(rest[..sep].to_vec()).ok()?;
    let host = String::from_utf8(rest[sep + 1..].to_vec()).ok()?;
    Some((ts, panel, host))
}

fn decode_value(val: &[u8]) -> Option<(bool, i64)> {
    if val.len() < 9 { return None; }
    let status = val[0] != 0;
    let duration = i64::from_be_bytes(val[1..9].try_into().ok()?);
    Some((status, duration))
}

// ── Database impl ─────────────────────────────────────────────────────────────

impl Database {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db = ReDb::create(db_path)
            .with_context(|| format!("failed to open redb at {}", db_path.display()))?;
        // Ensure the table exists
        let write_txn = db.begin_write()?;
        write_txn.open_table(TABLE)?;
        write_txn.commit()?;
        Ok(Self { db })
    }

    pub fn save_uptime_record(&self, record: &UptimeRecord) -> Result<()> {
        let ts = record.timestamp.unix_timestamp();
        let key = encode_key(ts, &record.panel, &record.host);
        let val = encode_value(record.status, record.duration);

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TABLE)?;
            table.insert(key.as_slice(), val.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Return the last `limit` records for a panel, newest first.
    pub fn get_uptime_records(&self, panel: &str, limit: usize) -> Result<Vec<UptimeRecord>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE)?;

        // Collect all records for this panel, then take the last `limit`
        // (full scan is acceptable — records are small and the table is bounded by trim_days)
        let mut records: Vec<UptimeRecord> = table
            .iter()?
            .filter_map(|entry| {
                let (k, v) = entry.ok()?;
                let (ts, p, host) = decode_key(k.value())?;
                if p != panel { return None; }
                let (status, duration) = decode_value(v.value())?;
                let timestamp = OffsetDateTime::from_unix_timestamp(ts).ok()?;
                Some(UptimeRecord { panel: p, host, status, timestamp, duration })
            })
            .collect();

        // Sort newest first, take limit
        records.sort_unstable_by(|a, b| b.timestamp.cmp(&a.timestamp));
        records.truncate(limit);
        Ok(records)
    }

    /// Return all records for a specific host within the last `days` days, oldest first.
    pub fn get_host_history_days(&self, panel: &str, host: &str, days: i64) -> Result<Vec<UptimeRecord>> {
        let cutoff = (OffsetDateTime::now_utc() - Duration::days(days)).unix_timestamp();
        let cutoff_key = encode_key(cutoff, "", "");

        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TABLE)?;

        let mut records: Vec<UptimeRecord> = table
            .range(cutoff_key.as_slice()..)?
            .filter_map(|entry| {
                let (k, v) = entry.ok()?;
                let (ts, p, h) = decode_key(k.value())?;
                if p != panel || h != host { return None; }
                let (status, duration) = decode_value(v.value())?;
                let timestamp = OffsetDateTime::from_unix_timestamp(ts).ok()?;
                Some(UptimeRecord { panel: p, host: h, status, timestamp, duration })
            })
            .collect();

        records.sort_unstable_by_key(|r| r.timestamp);
        Ok(records)
    }

    /// Delete all records older than `days` days.
    pub fn trim_old_records(&self, days: i64) -> Result<()> {
        let cutoff = (OffsetDateTime::now_utc() - Duration::days(days)).unix_timestamp();
        let cutoff_key = encode_key(cutoff, "", "");

        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TABLE)?;
            // Collect keys to delete (can't mutate while iterating)
            let to_delete: Vec<Vec<u8>> = table
                .range::<&[u8]>(..cutoff_key.as_slice())?
                .filter_map(|e| e.ok().map(|(k, _)| k.value().to_vec()))
                .collect();
            for key in to_delete {
                table.remove(key.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }
}
