use std::{env, fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_panel_border() -> bool {
    true
}

fn default_scan_interval() -> String {
    "60".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct Config {
    pub host: String,
    pub port: String,
    pub theme: String,
    pub color: String,
    #[serde(rename = "btnwidth")]
    pub btn_width: String,
    /// Page auto-refresh interval in seconds (browser-side)
    #[serde(rename = "webrefresh")]
    pub web_refresh: String,
    /// Uptime scan interval in seconds (server-side background task)
    #[serde(rename = "scan_interval", default = "default_scan_interval")]
    pub scan_interval: String,
    #[serde(rename = "dbtrimdays")]
    pub db_trim_days: String,
    #[serde(default)]
    pub panel_gap: String,
    #[serde(default = "default_true")]
    pub center_columns: bool,
    #[serde(default = "default_panel_border")]
    pub panel_border: bool,
    #[serde(rename = "nav_font_size", default)]
    pub nav_font_size: String,
    #[serde(rename = "btn_font_size", default)]
    pub btn_font_size: String,
    /// Gap between buttons inside a panel
    #[serde(rename = "btn_gap", default)]
    pub btn_gap: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: "8849".to_string(),
            theme: "minty".to_string(),
            color: "auto".to_string(),
            btn_width: "180px".to_string(),
            web_refresh: "60".to_string(),
            scan_interval: "60".to_string(),
            db_trim_days: "30".to_string(),
            panel_gap: "12px".to_string(),
            center_columns: true,
            panel_border: true,
            nav_font_size: "0.85rem".to_string(),
            btn_font_size: "0.8rem".to_string(),
            btn_gap: "8px".to_string(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = if path.exists() {
            let data = fs::read_to_string(path)
                .with_context(|| format!("failed to read config file {}", path.display()))?;
            serde_yaml::from_str(&data).with_context(|| format!("failed to parse config file {}", path.display()))?
        } else {
            tracing::warn!(path = %path.display(), "config file not found, using defaults");
            Self::default()
        };

        config.apply_env_overrides();
        Ok(config)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(value) = env::var("HOST") {
            self.host = value;
        }
        if let Ok(value) = env::var("PORT") {
            self.port = value;
        }
        if let Ok(value) = env::var("THEME") {
            self.theme = value;
        }
        if let Ok(value) = env::var("COLOR") {
            self.color = value;
        }
        if let Ok(value) = env::var("BTNWIDTH") {
            self.btn_width = value;
        }
        if let Ok(value) = env::var("WEBREFRESH") {
            self.web_refresh = value;
        }
        if let Ok(value) = env::var("SCAN_INTERVAL") {
            self.scan_interval = value;
        }
        if let Ok(value) = env::var("DBTRIMDAYS") {
            self.db_trim_days = value;
        }
        if let Ok(value) = env::var("PANEL_GAP") {
            self.panel_gap = value;
        }
        if let Ok(value) = env::var("CENTER_COLUMNS") {
            self.center_columns = value == "true" || value == "1";
        }
        if let Ok(value) = env::var("PANEL_BORDER") {
            self.panel_border = value == "true" || value == "1";
        }
        if let Ok(value) = env::var("NAV_FONT_SIZE") {
            self.nav_font_size = value;
        }
        if let Ok(value) = env::var("BTN_FONT_SIZE") {
            self.btn_font_size = value;
        }
        if let Ok(value) = env::var("BTN_GAP") {
            self.btn_gap = value;
        }
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let data = serde_yaml::to_string(self).context("failed to serialize config file")?;
        fs::write(path, data)
            .with_context(|| format!("failed to write config file {}", path.display()))?;
        Ok(())
    }
}
