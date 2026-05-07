use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context, Result};
use crate::{config::Config, models::Links};

pub type SharedState = std::sync::Arc<tokio::sync::RwLock<AppState>>;

pub struct AppState {
    pub config: Config,
    pub links: Links,
    pub config_path: PathBuf,
    pub board_path: PathBuf,
    pub db_path: PathBuf,
    /// Prevents concurrent /scan requests from stacking up
    pub scan_running: Arc<AtomicBool>,
}

impl AppState {
    pub fn new(config: Config, links: Links, config_path: PathBuf, board_path: PathBuf) -> Result<Self, anyhow::Error> {
        let db_path = config_path
            .parent()
            .map(|p| p.join("tinyboard.redb"))
            .context("failed to construct database path")?;

        Ok(Self {
            config,
            links,
            config_path,
            board_path,
            db_path,
            scan_running: Arc::new(AtomicBool::new(false)),
        })
    }
}
