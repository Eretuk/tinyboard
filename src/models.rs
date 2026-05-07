use std::{collections::HashMap, fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Host {
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub icon: String,
    /// URL or IP for uptime check. If starts with http(s):// → HTTP GET, otherwise → TCP connect.
    /// If empty, monitoring is skipped even if scan=true.
    #[serde(default)]
    pub check_url: String,
    /// Enable uptime monitoring for this host
    #[serde(default)]
    pub scan: bool,
    /// Per-host scan interval in seconds. 0 = use global scan_interval from config.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub scan_interval: u64,
    // Legacy fields — kept for YAML backwards compatibility, never written back
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub addr: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub port: String,
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    pub state: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Panel {
    pub name: String,
    #[serde(default)]
    pub hosts: HashMap<i32, Host>,
    // Legacy fields — kept for YAML backwards compatibility, never written back
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scan: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub timeout: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Tab {
    pub name: String,
    #[serde(default)]
    pub refresh: String,
    #[serde(default)]
    pub horiz: bool,
    #[serde(default)]
    pub panels: HashMap<i32, String>,
    // Legacy field — kept for YAML backwards compatibility, never written back
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_auth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MonPanel {
    #[serde(default)]
    pub retries: usize,
    #[serde(default)]
    pub notify: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct Uptime {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub show: usize,
    #[serde(default)]
    pub notify: HashMap<String, String>,
    #[serde(default)]
    pub panels: HashMap<String, MonPanel>,
    // Legacy field — kept for YAML backwards compatibility, never written back
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_auth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Links {
    #[serde(default)]
    pub tabs: HashMap<i32, Tab>,
    #[serde(default)]
    pub panels: HashMap<String, Panel>,
    #[serde(default)]
    pub uptime: Uptime,
}

impl Links {
    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read_to_string(path)
            .with_context(|| format!("failed to read board file {}", path.display()))?;
        let links = serde_yaml::from_str(&data)
            .with_context(|| format!("failed to parse board file {}", path.display()))?;
        Ok(links)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let data = serde_yaml::to_string(self)
            .context("failed to serialize board file")?;
        fs::write(path, data)
            .with_context(|| format!("failed to write board file {}", path.display()))?;
        Ok(())
    }
}
