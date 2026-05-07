use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;

use axum::{routing::get, Router, serve};
use clap::Parser;
use tokio::sync::RwLock;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;
use tracing_subscriber::{fmt, EnvFilter};

mod config;
mod models;
mod routes;
mod state;
mod db;
mod uptime;

use config::Config;
use models::Links;
use state::AppState;

#[derive(Parser, Debug)]
#[command(author, version, about = "tinyboard — lightweight homelab dashboard", long_about = None)]
struct Cli {
    #[arg(short = 'c', long, default_value = "/data/tinyboard/config.yaml", help = "Path to config yaml file")]
    config: PathBuf,

    #[arg(short = 'b', long, default_value = "/data/tinyboard/board.yaml", help = "Path to board yaml file")]
    board: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging: writes to stdout (Docker-friendly).
    // Level is controlled by RUST_LOG env var, e.g.:
    //   RUST_LOG=info          — standard output
    //   RUST_LOG=debug         — verbose including tower_http request traces
    //   RUST_LOG=rustboard=debug,tower_http=info  — fine-grained
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("tinyboard=debug,tower_http=info,info")),
        )
        .with_writer(std::io::stdout)   // explicit stdout for Docker log drivers
        .with_ansi(false)               // no ANSI colour codes in container logs
        .with_target(true)              // show module path (rustboard::routes etc.)
        .init();

    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    let links = Links::load(&cli.board)?;
    let state = Arc::new(RwLock::new(AppState::new(config, links, cli.config, cli.board).context("failed to initialize application state")?));

    let app = Router::new()
        .route("/", get(routes::index_handler))
        .route("/api/links", get(routes::links_handler))
        .route("/reload", get(routes::reload_handler).post(routes::reload_handler))
        .route("/tabs", get(routes::tabs_page_handler).post(routes::tabs_handler))
        .route("/tab_edit", get(routes::tab_edit_page_handler).post(routes::tab_edit_handler))
        .route("/panels", get(routes::panels_page_handler).post(routes::panels_handler))
        .route("/panel_edit", get(routes::panel_edit_page_handler).post(routes::panel_edit_handler))
        .route("/host_edit", get(routes::host_edit_page_handler).post(routes::host_edit_handler))
        .route("/config", get(routes::config_page_handler).post(routes::config_save_handler))
        .route("/board_edit", get(routes::board_edit_page_handler).post(routes::board_edit_save_handler))
        .route("/about", get(routes::about_page_handler))
        .route("/uptime", get(routes::uptime_handler))
        .route("/scan", get(routes::scan_handler))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state.clone());

    let addr: SocketAddr = state.read().await.config.address().parse()?;
    tracing::info!(%addr, "starting tinyboard");

    // Start uptime monitoring in background
    let state_clone = state.clone();
    tokio::spawn(async move {
        uptime::start_uptime_monitoring(state_clone).await;
    });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    serve(listener, app).await.context("failed to start server")?;

    Ok(())
}