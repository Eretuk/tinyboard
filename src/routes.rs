use std::collections::{BTreeMap, HashMap};
use std::fs;

use axum::{extract::{Form, Query, RawForm, State}, http::StatusCode, response::{Html, IntoResponse, Json, Redirect}};
use tracing::{error, info};

use crate::{config::Config, db::Database, models::{Host, Links, Panel, Tab}, state::SharedState, uptime::UptimeMonitor};

pub async fn index_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Html<String> {
    let state = state.read().await;
    let tab_id = params.get("tab").and_then(|value| value.parse::<i32>().ok());

    // Load latest uptime record per host for status dots
    let mut uptime_history: std::collections::HashMap<String, Vec<crate::db::UptimeRecord>> = std::collections::HashMap::new();
    if let Ok(db) = Database::new(&state.db_path) {
        for panel_name in state.links.panels.keys() {
            if let Ok(records) = db.get_uptime_records(panel_name, 10) {
                uptime_history.insert(panel_name.clone(), records);
            }
        }
    }

    Html(render_dashboard(&state.config, &state.links, tab_id, &uptime_history))
}

pub async fn links_handler(State(state): State<SharedState>) -> impl IntoResponse {
    let state = state.read().await;
    Json(state.links.clone())
}

pub async fn reload_handler(State(state): State<SharedState>) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut state = state.write().await;
    let config = Config::load(&state.config_path).map_err(|err| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to reload config: {err}"))
    })?;
    let links = Links::load(&state.board_path).map_err(|err| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to reload board: {err}"))
    })?;

    state.config = config;
    state.links = links;
    Ok(Redirect::to("/"))
}



#[derive(serde::Deserialize)]
pub struct PanelForm {
    oldkey: Option<String>,
    key: String,
    host_id: Option<i32>,
    host_name: Option<String>,
    host_url: Option<String>,
    host_check_url: Option<String>,
    host_icon: Option<String>,
    host_scan: Option<String>,
    action: Option<String>,
}

pub async fn tabs_page_handler(State(state): State<SharedState>) -> Html<String> {
    let state = state.read().await;
    Html(render_tabs_page(&state.config, &state.links))
}

pub async fn tabs_handler(
    State(state): State<SharedState>,
    RawForm(bytes): RawForm,
) -> Result<Redirect, (StatusCode, String)> {
    let mut state = state.write().await;

    // Parse form data manually to handle duplicate keys
    let form_str = String::from_utf8_lossy(&bytes);
    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    for pair in form_str.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            // application/x-www-form-urlencoded encodes spaces as '+', urlencoding::decode only handles %20
            let key = urlencoding::decode(&key.replace('+', " ")).unwrap_or_default().to_string();
            let value = urlencoding::decode(&value.replace('+', " ")).unwrap_or_default().to_string();
            params.entry(key).or_default().push(value);
        }
    }

    // Handle reordering
    if let Some(up_str) = params.get("up").and_then(|v| v.first()) {
        if let Ok(up) = up_str.parse::<i32>() {
            // Find the previous key in sorted order (not necessarily up-1)
            let mut sorted_keys: Vec<i32> = state.links.tabs.keys().copied().collect();
            sorted_keys.sort_unstable();
            if let Some(pos) = sorted_keys.iter().position(|&k| k == up) {
                if pos > 0 {
                    let prev = sorted_keys[pos - 1];
                    if let (Some(current), Some(previous)) = (
                        state.links.tabs.get(&up).cloned(),
                        state.links.tabs.get(&prev).cloned(),
                    ) {
                        state.links.tabs.insert(prev, current);
                        state.links.tabs.insert(up, previous);
                    }
                }
            }
        }
    } else {
        let name = params.get("name").and_then(|v| v.first()).cloned().unwrap_or_default();
        if !name.trim().is_empty() {
            let id = params.get("id")
                .and_then(|v| v.first()?.parse::<i32>().ok())
                .or_else(|| state.links.tabs.keys().max().map(|k| k + 1))
                .unwrap_or(0);

            let mut tab = Tab {
                name: name.trim().to_string(),
                refresh: params.get("refresh").and_then(|v| v.first()).cloned().unwrap_or_default(),
                horiz: params.contains_key("horiz"),
                panels: HashMap::new(),
                needs_auth: false,
            };

            if let Some(panels) = params.get("panels") {
                for (index, panel_key) in panels.iter().enumerate() {
                    tab.panels.insert(index as i32, panel_key.clone());
                }
            }

            state.links.tabs.insert(id, tab);
        }
    }

    state
        .links
        .save(&state.board_path)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Redirect::to("/tabs"))
}

pub async fn tab_edit_page_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Html<String> {
    let state = state.read().await;
    let tab_id = params.get("tab").and_then(|value| value.parse::<i32>().ok());

    if let Some(tab_id) = tab_id {
        if let Some(tab) = state.links.tabs.get(&tab_id) {
            return Html(render_tab_edit_page(&state.config, &state.links, tab_id, tab));
        }
    }

    Html(render_tabs_page(&state.config, &state.links))
}

pub async fn tab_edit_handler(
    State(state): State<SharedState>,
    RawForm(bytes): RawForm,
) -> Result<Redirect, (StatusCode, String)> {
    let mut state = state.write().await;

    // Parse form data manually to handle duplicate keys (panels[])
    let form_str = String::from_utf8_lossy(&bytes);
    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    for pair in form_str.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            let key = urlencoding::decode(&key.replace('+', " ")).unwrap_or_default().to_string();
            let value = urlencoding::decode(&value.replace('+', " ")).unwrap_or_default().to_string();
            params.entry(key).or_default().push(value);
        }
    }

    let tab_id = params.get("tab").and_then(|v| v.first()?.parse::<i32>().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing tab id".to_string()))?;
    let name = params.get("name").and_then(|v| v.first()).cloned().unwrap_or_default();
    let refresh = params.get("refresh").and_then(|v| v.first()).cloned().unwrap_or_default();
    let horiz = params.contains_key("horiz");
    let action = params.get("action").and_then(|v| v.first()).cloned();

    if action.as_deref() == Some("delete") {
        state.links.tabs.remove(&tab_id);
        state
            .links
            .save(&state.board_path)
            .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
        return Ok(Redirect::to("/tabs"));
    }

    let mut tab = Tab {
        name: name.trim().to_string(),
        refresh: refresh.trim().to_string(),
        horiz,
        panels: HashMap::new(),
        needs_auth: false,
    };

    if let Some(panels) = params.get("panels") {
        for (index, panel_key) in panels.iter().enumerate() {
            tab.panels.insert(index as i32, panel_key.clone());
        }
    }

    state.links.tabs.insert(tab_id, tab);
    state
        .links
        .save(&state.board_path)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Redirect::to("/tabs"))
}

pub async fn panels_page_handler(State(state): State<SharedState>) -> Html<String> {
    let state = state.read().await;
    Html(render_panels_page(&state.config, &state.links))
}

pub async fn panels_handler(
    State(state): State<SharedState>,
    Form(form): Form<PanelForm>,
) -> Result<Redirect, (StatusCode, String)> {
    let mut state = state.write().await;
    let key = form.key.trim().to_string();

    if !key.is_empty() {
        let mut panel = Panel {
            name: key.clone(),
            hosts: HashMap::new(),
            scan: false,
            timeout: String::new(),
        };

        if let Some(oldkey) = form.oldkey.filter(|k| !k.is_empty()) {
            if oldkey != key {
                if let Some(existing) = state.links.panels.remove(&oldkey) {
                    panel.hosts = existing.hosts;
                }
            } else if let Some(existing) = state.links.panels.get(&key).cloned() {
                panel.hosts = existing.hosts;
            }
        } else if let Some(existing) = state.links.panels.get(&key).cloned() {
            panel.hosts = existing.hosts;
        }

        state.links.panels.insert(key, panel);
    }

    state
        .links
        .save(&state.board_path)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Redirect::to("/panels"))
}

pub async fn panel_edit_page_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Html<String> {
    let state = state.read().await;
    let edit = params.get("edit").cloned();

    if let Some(edit) = edit {
        if let Some(panel) = state.links.panels.get(&edit) {
            return Html(render_panel_edit_page(&state.config, &edit, panel));
        }
    }

    Html(render_panels_page(&state.config, &state.links))
}

pub async fn panel_edit_handler(
    State(state): State<SharedState>,
    Form(form): Form<PanelForm>,
) -> Result<Redirect, (StatusCode, String)> {
    let mut state = state.write().await;
    let key = form.key.trim().to_string();

    if form.action.as_deref() == Some("delete") {
        if let Some(oldkey) = form.oldkey.filter(|k| !k.is_empty()) {
            state.links.panels.remove(&oldkey);
            state
                .links
                .save(&state.board_path)
                .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
        }
        return Ok(Redirect::to("/panels"));
    }

    if !key.is_empty() {
        if form.action.as_deref() == Some("delete_host") {
            if let Some(host_id) = form.host_id {
                if let Some(panel) = state.links.panels.get_mut(&key) {
                    panel.hosts.remove(&host_id);
                }
            }
            state
                .links
                .save(&state.board_path)
                .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
            return Ok(Redirect::to(&format!("/panel_edit?edit={}", urlencoding::encode(&key))));
        }

        if matches!(form.action.as_deref(), Some("host_up") | Some("host_down")) {
            if let Some(host_id) = form.host_id {
                if let Some(panel) = state.links.panels.get_mut(&key) {
                    let direction = if form.action.as_deref() == Some("host_up") { -1 } else { 1 };
                    move_host_order(panel, host_id, direction);
                }
            }
            state
                .links
                .save(&state.board_path)
                .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
            return Ok(Redirect::to(&format!("/panel_edit?edit={}", urlencoding::encode(&key))));
        }

        if matches!(form.action.as_deref(), Some("add_host") | Some("save_host")) {
            let host_name = form.host_name.unwrap_or_default().trim().to_string();
            if !host_name.is_empty() {
                let panel = state
                    .links
                    .panels
                    .get_mut(&key)
                    .ok_or_else(|| (StatusCode::NOT_FOUND, format!("panel not found: {}", key)))?;
                let host_id = match form.host_id {
                    Some(id) if panel.hosts.contains_key(&id) => id,
                    Some(id) => id,
                    None => panel.hosts.keys().copied().max().map_or(0, |id| id + 1),
                };
                panel.hosts.insert(
                    host_id,
                    crate::models::Host {
                        name: host_name,
                        url: form.host_url.unwrap_or_default().trim().to_string(),
                        check_url: form.host_check_url.unwrap_or_default().trim().to_string(),
                        icon: form.host_icon.unwrap_or_default().trim().to_string(),
                        scan: form.host_scan.is_some(),
                        scan_interval: 0,
                        addr: String::new(),
                        port: String::new(),
                        state: false,
                    },
                );
                state
                    .links
                    .save(&state.board_path)
                    .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
            }
            return Ok(Redirect::to(&format!("/panel_edit?edit={}", urlencoding::encode(&key))));
        }

        let mut panel = Panel {
            name: key.clone(),
            hosts: HashMap::new(),
            scan: false,
            timeout: String::new(),
        };

        if let Some(oldkey) = form.oldkey.filter(|k| !k.is_empty()) {
            if oldkey != key {
                // Rename: move hosts from old key, don't overwrite with target's hosts
                if let Some(existing) = state.links.panels.remove(&oldkey) {
                    panel.hosts = existing.hosts;
                }
            } else {
                // Same key: preserve existing hosts
                if let Some(existing) = state.links.panels.get(&key).cloned() {
                    panel.hosts = existing.hosts;
                }
            }
        } else if let Some(existing) = state.links.panels.get(&key).cloned() {
            panel.hosts = existing.hosts;
        }

        state.links.panels.insert(key, panel);
        state
            .links
            .save(&state.board_path)
            .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    }

    Ok(Redirect::to("/panels"))
}

pub async fn host_edit_page_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Html<String> {
    let state = state.read().await;
    let panel_key = params.get("panel").cloned().unwrap_or_default();
    let host_id = params.get("host").and_then(|value| value.parse::<i32>().ok());

    if let (Some(panel), Some(host_id)) = (state.links.panels.get(&panel_key), host_id) {
        if let Some(host) = panel.hosts.get(&host_id) {
            return Html(render_host_edit_page(&state.config, &panel_key, host_id, host));
        }
    }

    Html(render_panels_page(&state.config, &state.links))
}

pub async fn host_edit_handler(
    State(state): State<SharedState>,
    Form(form): Form<HostEditForm>,
) -> Result<Redirect, (StatusCode, String)> {
    info!(panel = %form.panel, host_id = %form.host_id, name = %form.name, action = ?form.action, "host_edit: received form");

    let mut state = state.write().await;
    let panel = state
        .links
        .panels
        .get_mut(&form.panel)
        .ok_or_else(|| {
            error!(panel = %form.panel, "host_edit: panel not found");
            (StatusCode::NOT_FOUND, format!("panel not found: {}", form.panel))
        })?;

    if form.action.as_deref() == Some("delete") {
        info!(panel = %form.panel, host_id = %form.host_id, "host_edit: deleting host");
        panel.hosts.remove(&form.host_id);
        state
            .links
            .save(&state.board_path)
            .map_err(|err| {
                error!("host_edit: failed to save after delete: {}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
            })?;
        return Ok(Redirect::to(&format!("/panel_edit?edit={}", urlencoding::encode(&form.panel))));
    }

    if form.action.as_deref() == Some("up") || form.action.as_deref() == Some("down") {
        let direction = if form.action.as_deref() == Some("up") { -1 } else { 1 };
        move_host_order(panel, form.host_id, direction);
        state
            .links
            .save(&state.board_path)
            .map_err(|err| {
                error!("host_edit: failed to save after reorder: {}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
            })?;
        return Ok(Redirect::to(&format!("/host_edit?panel={}&host={}", urlencoding::encode(&form.panel), form.host_id)));
    }

    info!(panel = %form.panel, host_id = %form.host_id, check_url = %form.check_url, scan = ?form.scan, "host_edit: saving host");
    panel.hosts.insert(
        form.host_id,
        crate::models::Host {
            name: form.name.trim().to_string(),
            url: form.url.trim().to_string(),
            check_url: form.check_url.trim().to_string(),
            icon: form.icon.trim().to_string(),
            scan: form.scan.is_some(),
            scan_interval: form.scan_interval.unwrap_or(0),
            addr: String::new(),
            port: String::new(),
            state: false,
        },
    );
    state
        .links
        .save(&state.board_path)
        .map_err(|err| {
            error!("host_edit: failed to save: {}", err);
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        })?;
    info!(panel = %form.panel, host_id = %form.host_id, "host_edit: saved successfully");
    Ok(Redirect::to(&format!("/host_edit?panel={}&host={}", urlencoding::encode(&form.panel), form.host_id)))
}

pub async fn uptime_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Html<String> {
    let state = state.read().await;
    let show = state.links.uptime.show.max(20);
    let filter_panel = params.get("panel").cloned();
    let filter_host = params.get("host").cloned();
    let trim_days = state.config.db_trim_days.parse::<i64>().unwrap_or(30);

    let mut history: Vec<(String, Vec<crate::db::UptimeRecord>)> = Vec::new();
    if let Ok(db) = Database::new(&state.db_path) {
        let mut panel_names: Vec<String> = state.links.panels.iter()
            .filter(|(_, p)| p.hosts.values().any(|h| h.scan))
            .map(|(k, _)| k.clone())
            .collect();
        panel_names.sort();

        for panel_name in &panel_names {
            if let Some(ref fp) = filter_panel {
                if panel_name != fp { continue; }
            }
            // Host detail view: load full history for the period
            if let (Some(fh), Some(fp)) = (&filter_host, &filter_panel) {
                if panel_name == fp.as_str() {
                    if let Ok(records) = db.get_host_history_days(panel_name, fh, trim_days) {
                        history.push((panel_name.clone(), records));
                    }
                }
                continue;
            }
            // Overview: load last N records per panel
            if let Ok(records) = db.get_uptime_records(panel_name, show) {
                let records = if let Some(ref fh) = filter_host {
                    records.into_iter().filter(|r| &r.host == fh).collect()
                } else {
                    records
                };
                history.push((panel_name.clone(), records));
            }
        }
    }

    Html(render_uptime_page(&state.config, &state.links, &history, filter_host.as_deref(), trim_days))
}

pub async fn scan_handler(State(state): State<SharedState>) -> Result<Json<Vec<ScanResult>>, (StatusCode, String)> {
    // Prevent concurrent scans — return 429 if one is already running
    let scan_running = {
        let s = state.read().await;
        s.scan_running.clone()
    };
    if scan_running.compare_exchange(false, true, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst).is_err() {
        return Err((StatusCode::TOO_MANY_REQUESTS, "scan already in progress".to_string()));
    }

    // Snapshot panels without holding the lock during I/O
    let (panels_snapshot, db_path) = {
        let s = state.read().await;
        let panels: Vec<(String, crate::models::Panel)> = s.links.panels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        (panels, s.db_path.clone())
    };

    let monitor = std::sync::Arc::new(UptimeMonitor::new());
    let mut tasks = Vec::new();

    for (panel_name, panel) in panels_snapshot {
        for host in panel.hosts.into_values() {
            if !host.scan { continue; }
            let monitor = std::sync::Arc::clone(&monitor);
            let panel_name = panel_name.clone();
            let db_path = db_path.clone();
            tasks.push(tokio::spawn(async move {
                let (status, duration_ms) = monitor.scan_host(&host).await;
                crate::uptime::save_record(&db_path, &panel_name, &host.name, status, duration_ms);
                ScanResult { panel: panel_name, host: host.name, status, duration: duration_ms as u64 }
            }));
        }
    }

    let mut results = Vec::new();
    for task in tasks {
        if let Ok(r) = task.await {
            results.push(r);
        }
    }

    scan_running.store(false, std::sync::atomic::Ordering::SeqCst);
    Ok(Json(results))
}

pub async fn config_page_handler(State(state): State<SharedState>) -> Html<String> {
    let state = state.read().await;
    Html(render_config_page(&state.config))
}

pub async fn config_save_handler(
    State(state): State<SharedState>,
    Form(form): Form<ConfigForm>,
) -> Result<Redirect, (StatusCode, String)> {
    let mut state = state.write().await;
    state.config.host = form.host;
    state.config.port = form.port;
    state.config.theme = safe_theme(&form.theme).to_string();
    state.config.color = safe_css_color(&form.color).to_string();
    state.config.btn_width = normalize_css_size(&form.btn_width, "180px");
    state.config.web_refresh = form.web_refresh;
    // Clamp scan_interval to [10, 86400] seconds
    state.config.scan_interval = form.scan_interval
        .parse::<u64>()
        .unwrap_or(60)
        .clamp(10, 86400)
        .to_string();
    // Clamp db_trim_days to [1, 365] days
    state.config.db_trim_days = form.db_trim_days
        .parse::<u64>()
        .unwrap_or(30)
        .clamp(1, 365)
        .to_string();
    state.config.panel_gap = normalize_css_size(&form.panel_gap, "12px");
    state.config.center_columns = form.center_columns.is_some();
    state.config.panel_border = form.panel_border.is_some();
    state.config.nav_font_size = normalize_css_size(&form.nav_font_size, "0.85rem");
    state.config.btn_font_size = normalize_css_size(&form.btn_font_size, "0.8rem");
    state.config.btn_gap = normalize_css_size(&form.btn_gap, "8px");
    state
        .config
        .save(&state.config_path)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Redirect::to("/config"))
}

pub async fn board_edit_page_handler(State(state): State<SharedState>) -> Result<Html<String>, (StatusCode, String)> {
    let state = state.read().await;
    let content = fs::read_to_string(&state.board_path)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to read board file: {err}")))?;
    Ok(Html(render_board_edit_page(&state.config, &content)))
}

pub async fn board_edit_save_handler(
    State(state): State<SharedState>,
    Form(form): Form<BoardEditForm>,
) -> Result<Redirect, (StatusCode, String)> {
    // Validate YAML first — only write to disk if parsing succeeds
    let links = Links::load_from_str(&form.content)
        .map_err(|err| (StatusCode::BAD_REQUEST, format!("invalid YAML: {err}")))?;
    let mut state = state.write().await;
    fs::write(&state.board_path, &form.content)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to write board file: {err}")))?;
    state.links = links;
    Ok(Redirect::to("/board_edit"))
}

pub async fn about_page_handler(State(state): State<SharedState>) -> Html<String> {
    let state = state.read().await;
    Html(render_about_page(&state.config))
}

#[derive(serde::Deserialize)]
pub struct ConfigForm {
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: String,
    #[serde(default)]
    theme: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    btn_width: String,
    #[serde(default)]
    web_refresh: String,
    #[serde(default)]
    scan_interval: String,
    #[serde(default)]
    db_trim_days: String,
    #[serde(default)]
    panel_gap: String,
    center_columns: Option<String>,
    panel_border: Option<String>,
    #[serde(default)]
    nav_font_size: String,
    #[serde(default)]
    btn_font_size: String,
    #[serde(default)]
    btn_gap: String,
}

#[derive(serde::Deserialize)]
pub struct BoardEditForm {
    content: String,
}



#[derive(serde::Deserialize)]
pub struct HostEditForm {
    panel: String,
    host_id: i32,
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    check_url: String,
    #[serde(default)]
    icon: String,
    scan: Option<String>,
    scan_interval: Option<u64>,
    action: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ScanResult {
    panel: String,
    host: String,
    status: bool,
    duration: u64,
}

fn render_management_menu() -> String {
    r#"<div class="dropdown"><button class="menu-btn js-dropdown-toggle" type="button" aria-expanded="false">☰</button><div class="dropdown-content"><a href="/config">Config</a><hr class="menu-divider"><a href="/panels">Edit panels</a><a href="/tabs">Edit tabs</a><a href="/uptime">Uptime stats</a><hr class="menu-divider"><a href="/board_edit">Edit board file</a><a href="/reload">Reload</a><hr class="menu-divider"><a href="/about">About</a></div></div>"#.to_string()
}

fn render_dashboard(config: &Config, links: &Links, selected_tab: Option<i32>, uptime_history: &std::collections::HashMap<String, Vec<crate::db::UptimeRecord>>) -> String {
    let (accent, accent_dark) = theme_accent(&config.theme);
    let ordered_tabs: BTreeMap<_, _> = links.tabs.iter().collect();
    let active_tab = selected_tab.or_else(|| ordered_tabs.keys().next().map(|k| **k)).unwrap_or(0);
    let mut tabs_html = String::new();
    for (&tab_id, tab) in &ordered_tabs {
        let active_class = if *tab_id == active_tab { "active" } else { "" };
        tabs_html.push_str(&format!(
            r#"<a class="tab-pill {}" href="/?tab={}">{}</a>"#,
            active_class, tab_id, html_escape(&tab.name)
        ));
    }

    let zero_gap = config.panel_gap.trim() == "0" || config.panel_gap.trim() == "0px";
    let panel_columns_class = match (config.center_columns, zero_gap) {
        (true, true) => "panel-columns centered tight",
        (true, false) => "panel-columns centered",
        (false, true) => "panel-columns flow tight",
        (false, false) => "panel-columns flow",
    };
    let panel_card_class = if config.panel_border { "panel-card" } else { "panel-card no-border" };
    let mut html = String::new();
    html.push_str(&format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>tinyboard</title>
<link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'><rect width='32' height='32' rx='6' fill='%2378c2ad'/><rect x='6' y='8' width='20' height='3' rx='1.5' fill='white'/><rect x='6' y='14' width='15' height='3' rx='1.5' fill='white'/><rect x='6' y='20' width='10' height='3' rx='1.5' fill='white'/></svg>">
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/bootswatch@5.3.3/dist/{theme}/bootstrap.min.css">
<style>
:root {{ --accent: {accent}; --accent-dark: {accent_dark}; --on: #4ade80; --off: #f87171; --btn-width: {btn_width}; --panel-gap: {panel_gap}; --btn-gap: {btn_gap}; --nav-font-size: {nav_font_size}; --btn-font-size: {btn_font_size}; }}
[data-theme="dark"] {{ --bg: #1a1d23; --panel: #22262d; --panel-border: #2d333b; --text: #c9d1d9; --muted: #8b949e; --button-bg: #2d333b; --button-border: #444c56; --topbar-bg: var(--accent); --topbar-text: #ffffff; }}
[data-theme="light"] {{ --bg: #f6f8fa; --panel: #ffffff; --panel-border: #d0d7de; --text: #24292f; --muted: #57606a; --button-bg: #f3f4f6; --button-border: #d0d7de; --topbar-bg: var(--accent); --topbar-text: #1a1d23; }}
@media (prefers-color-scheme: dark) {{ [data-theme="auto"] {{ --bg: #1a1d23; --panel: #22262d; --panel-border: #2d333b; --text: #c9d1d9; --muted: #8b949e; --button-bg: #2d333b; --button-border: #444c56; --topbar-bg: var(--accent); --topbar-text: #ffffff; }} }}
@media (prefers-color-scheme: light) {{ [data-theme="auto"] {{ --bg: #f6f8fa; --panel: #ffffff; --panel-border: #d0d7de; --text: #24292f; --muted: #57606a; --button-bg: #f3f4f6; --button-border: #d0d7de; --topbar-bg: var(--accent); --topbar-text: #1a1d23; }} }}
* {{ box-sizing: border-box; }}
body {{ background: var(--bg); color: var(--text); font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; margin: 0; padding: 0; min-height: 100vh; }}
.container {{ max-width: 100%; margin: 0 auto; padding: 0; }}
.topbar {{ display: grid; grid-template-columns: 260px 1fr 260px; align-items: center; gap: 0.5rem; padding: 0.95rem 1.5rem; background: var(--topbar-bg); color: var(--topbar-text); box-shadow: 0 1px 3px rgba(0,0,0,0.1); }}
.nav-side {{ display: inline-flex; align-items: center; gap: 0.5rem; }}
.nav-side.right {{ justify-content: flex-end; }}
.home-icon {{ width: 32px; height: 32px; border-radius: 6px; border: none; display: inline-flex; align-items: center; justify-content: center; text-decoration: none; color: var(--topbar-text); background: rgba(255,255,255,0.2); font-size: 1rem; transition: background 0.2s; }}
.home-icon:hover {{ background: rgba(255,255,255,0.35); }}
.dropdown {{ position: relative; display: inline-block; }}
.menu-btn {{ border: none; background: rgba(255,255,255,0.2); color: var(--topbar-text); border-radius: 6px; padding: 0.4rem 0.6rem; cursor: pointer; transition: background 0.2s; }}
.menu-btn:hover {{ background: rgba(255,255,255,0.35); }}
.dropdown-content {{ display: none; position: absolute; top: 100%; left: 0; min-width: 180px; margin-top: 0.35rem; background: var(--panel); border: 1px solid var(--panel-border); border-radius: 10px; padding: 0.35rem 0; z-index: 20; box-shadow: 0 4px 12px rgba(0,0,0,0.15); }}
.dropdown-content a {{ display: block; color: var(--text); text-decoration: none; padding: 0.5rem 0.8rem; font-size: 0.9rem; }}
.dropdown-content a:hover {{ background: var(--accent); opacity: 0.9; }}
.dropdown-content .menu-divider {{ margin: 0.25rem 0.5rem; border: none; border-top: 1px solid var(--panel-border); }}
.dropdown.open .dropdown-content {{ display: block; }}
.tab-strip {{ display: flex; flex-wrap: wrap; gap: 0.25rem; justify-content: center; flex: 1; }}
.tab-pill {{ display: inline-flex; align-items: center; padding: 0.4rem 0.8rem; border-radius: 6px; border: none; background: rgba(0,0,0,0.1); color: var(--topbar-text); text-decoration: none; font-weight: 500; font-size: var(--nav-font-size); transition: background 0.2s; }}
.tab-pill:hover {{ background: rgba(0,0,0,0.2); }}
.tab-pill.active {{ background: var(--topbar-text); color: var(--topbar-bg); }}
.tab-card {{ background: var(--bg); padding: 1.5rem 2rem; min-height: calc(100vh - 60px); }}
.panel-columns {{ display: grid; gap: var(--panel-gap); max-width: 1800px; margin: 0 auto; --panel-col-width: calc(var(--btn-width) + 44px); }}
.panel-columns.centered {{ grid-template-columns: repeat(auto-fit, minmax(var(--panel-col-width), var(--panel-col-width))); justify-content: center; }}
.panel-columns.flow {{ grid-template-columns: repeat(auto-fill, minmax(var(--panel-col-width), var(--panel-col-width))); justify-content: center; }}
.panel-card {{ width: var(--panel-col-width); background: rgba(255,255,255,0.03); border: 1px solid rgba(148,163,184,0.12); border-radius: 20px; padding: 0.75rem 0.6rem; margin: 0; }}
.panel-columns.tight .panel-card {{ padding: 0.35rem 0.2rem; border-radius: 12px; }}
.panel-card h3 {{ margin: 0 0 0.85rem 0; font-size: 1rem; color: var(--text); }}
.panel-card h3 a {{ color: var(--text); text-decoration: underline; text-underline-offset: 2px; }}
.panel-card.no-border {{ border-color: transparent; background: transparent; box-shadow: none; }}
.panel-card h3 {{ text-align: center; }}
.host-grid {{ display: grid; gap: var(--btn-gap, 8px); grid-template-columns: minmax(var(--btn-width, 180px), var(--btn-width, 180px)); justify-content: center; }}
.host-button {{ display: flex; gap: 0.45rem; align-items: center; justify-content: center; text-align: center; width: var(--btn-width, 180px); min-height: 38px; padding: 0.45rem 0.6rem; border-radius: 6px; background: var(--accent); border: none; color: var(--topbar-text); text-decoration: none; font-family: Montserrat, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; font-weight: 500; transition: transform 0.1s, filter 0.2s; }}
.host-button:hover {{ transform: translateY(-1px); filter: brightness(1.05); }}
.host-icon {{ width: 26px; height: 26px; border-radius: 8px; background: rgba(255,255,255,0.25); display: inline-flex; align-items: center; justify-content: center; overflow: hidden; flex-shrink: 0; }}
.host-icon img {{ width: 100%; height: 100%; object-fit: contain; display: block; }}
.host-title {{ margin: 0; font-size: var(--btn-font-size); font-weight: 400; line-height: 1.1; color: var(--topbar-text); }}
.host-subtitle {{ margin: 0.1rem 0 0; color: var(--topbar-text); opacity: 0.72; font-size: calc(var(--btn-font-size) * 0.85); word-break: break-all; line-height: 1.05; }}
.host-button > div {{ display: flex; flex-direction: column; align-items: center; justify-content: center; min-width: 0; }}
.host-wrap {{ position: relative; display: inline-block; width: var(--btn-width, 180px); }}
.status-dot {{ position: absolute; bottom: -4px; right: -4px; width: 12px; height: 12px; border-radius: 50%; background: var(--off); border: 2px solid var(--bg, #1a1d23); z-index: 2; text-decoration: none; display: block; transition: transform 0.15s; }}
.status-dot.online {{ background: var(--on); }}
.status-dot:hover {{ transform: scale(1.4); }}
.tooltip-dot.on {{ background: var(--on); }}
.tooltip-dot.off {{ background: var(--off); }}
@media (max-width: 920px) {{ .topbar {{ grid-template-columns: 1fr; }} .nav-side.right {{ display: none; }} .tab-strip {{ margin-top: 0.35rem; }} }}
@media (max-width: 720px) {{ .host-button {{ grid-template-columns: 44px 1fr; }} }}
</style>
</head>
<body data-theme="{color}">
<div class="container">
<div class="topbar">
<div class="nav-side"><a class="home-icon" href="/" title="Home">🦀</a>{}<span style="font-size:var(--nav-font-size);">tinyboard</span></div>
<div class="tab-strip">{}</div>
<div class="nav-side right"></div>
</div>
<div class=\"tab-card\">"#,
        render_management_menu(),
        tabs_html,
        color = safe_css_color(&config.color),
        btn_width = safe_css_size(&config.btn_width, "180px"),
        panel_gap = safe_css_size(&config.panel_gap, "12px"),
        btn_gap = safe_css_size(if config.btn_gap.is_empty() { "8px" } else { &config.btn_gap }, "8px"),
        nav_font_size = safe_css_size(if config.nav_font_size.is_empty() { "0.85rem" } else { &config.nav_font_size }, "0.85rem"),
        btn_font_size = safe_css_size(if config.btn_font_size.is_empty() { "0.8rem" } else { &config.btn_font_size }, "0.8rem"),
        accent = accent,
        accent_dark = accent_dark,
        theme = safe_theme(&config.theme),
    ));

    if let Some(tab) = ordered_tabs.get(&active_tab) {
        html.push_str(&format!("<div class=\"{}\">", panel_columns_class));
        let ordered_panels: BTreeMap<_, _> = tab.panels.iter().collect();
        for (_place, panel_id) in ordered_panels {
            if let Some(panel) = links.panels.get(panel_id) {
                html.push_str(&format!(
                    r#"<div class="{}"><h3><a href="/panel_edit?edit={}">{}</a></h3><div class="host-grid">"#,
                    panel_card_class,
                    urlencoding::encode(panel_id),
                    html_escape(&panel.name),
                ));
                for (_host_id, host) in panel.hosts.iter().collect::<std::collections::BTreeMap<_,_>>() {
                    let url = if host.url.is_empty() { "#".to_string() } else { safe_url(&host.url) };

                    // Icon: hide entirely if empty, show image or emoji otherwise
                    let icon_html = if host.icon.is_empty() {
                        String::new()
                    } else if let Some(src) = safe_img_src(&host.icon) {
                        format!(r#"<span class="host-icon"><img src="{}" alt="" loading="lazy"></span>"#, src)
                    } else {
                        format!(r#"<span class="host-icon">{}</span>"#, html_escape(&host.icon))
                    };

                    // Status dot — click navigates to uptime stats filtered to this host
                    let status_html = if host.scan {
                        let records: Vec<_> = uptime_history
                            .get(panel_id)
                            .map(|recs| recs.iter().filter(|r| r.host == host.name).take(1).collect())
                            .unwrap_or_default();

                        let is_online = records.first().map(|r| r.status).unwrap_or(false);
                        let dot_class = if is_online { "status-dot online" } else { "status-dot" };
                        let uptime_url = format!("/uptime?panel={}&host={}",
                            urlencoding::encode(panel_id),
                            urlencoding::encode(&host.name));

                        format!(
                            r#"<a class="{}" href="{}" title="View uptime stats"></a>"#,
                            dot_class, uptime_url
                        )
                    } else {
                        String::new()
                    };

                    html.push_str(&format!(
                        r#"<div class="host-wrap"><a class="host-button" href="{url}" target="_blank" rel="noreferrer noopener">{icon}<div><p class="host-title">{name}</p></div></a>{status}</div>"#,
                        url = url,
                        icon = icon_html,
                        name = html_escape(&host.name),
                        status = status_html,
                    ));
                }
                html.push_str("</div></div>");
            }
        }
        html.push_str("</div>");
    }

    html.push_str(r#"</div>
</div>
<script>
document.addEventListener("click", function(event) {{
  var toggles = document.querySelectorAll(".js-dropdown-toggle");
  for (var i = 0; i < toggles.length; i++) {{
    var toggle = toggles[i];
    var dropdown = toggle.closest(".dropdown");
    if (toggle.contains(event.target)) {{
      var isOpen = dropdown.classList.contains("open");
      dropdown.classList.toggle("open", !isOpen);
      toggle.setAttribute("aria-expanded", String(!isOpen));
    }} else if (!dropdown.contains(event.target)) {{
      dropdown.classList.remove("open");
      toggle.setAttribute("aria-expanded", "false");
    }}
  }}
}});
</script>
</body>
</html>"#);
    html
}

fn render_page_shell(title: &str, content: String, color: &str, theme: &str) -> String {
    let (accent, accent_hover) = theme_accent(theme);
    format!(r#"<!DOCTYPE html>
<html lang="en" data-theme="{color}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{}</title>
<style>
:root {{ --accent: {accent}; --accent-hover: {accent_hover}; --on: #4ade80; --off: #f87171; }}
[data-theme="dark"], html {{ --bg: #1a1d23; --panel: #22262d; --panel-border: #2d333b; --text: #c9d1d9; --muted: #8b949e; --button-bg: #2d333b; --button-border: #444c56; --topbar-text: #1a1d23; }}
[data-theme="light"] {{ --bg: #f6f8fa; --panel: #ffffff; --panel-border: #d0d7de; --text: #24292f; --muted: #57606a; --button-bg: #f3f4f6; --button-border: #d0d7de; --topbar-text: #1a1d23; }}
@media (prefers-color-scheme: dark) {{ html:not([data-theme="light"]) {{ --bg: #1a1d23; --panel: #22262d; --panel-border: #2d333b; --text: #c9d1d9; --muted: #8b949e; --button-bg: #2d333b; --button-border: #444c56; --topbar-text: #1a1d23; }} }}
@media (prefers-color-scheme: light) {{ html:not([data-theme="dark"]) {{ --bg: #f6f8fa; --panel: #ffffff; --panel-border: #d0d7de; --text: #24292f; --muted: #57606a; --button-bg: #f3f4f6; --button-border: #d0d7de; --topbar-text: #1a1d23; }} }}
* {{ box-sizing: border-box; }}
body {{ background: var(--bg); color: var(--text); font-family: Inter, system-ui, sans-serif; margin: 0; padding: 0; }}
.container {{ max-width: 100%; margin: 0 auto; padding: 0; }}
.topbar {{ display: flex; flex-wrap: wrap; align-items: center; justify-content: center; gap: 0.5rem; padding: 0.95rem 1.5rem; background: var(--accent); color: #1a1d23; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }}
.topbar h1 {{ margin: 0; font-size: clamp(1.35rem, 2vw, 1.8rem); }}
.left-head {{ display: inline-flex; align-items: center; justify-content: center; gap: 0.65rem; }}
.home-icon {{ width: 32px; height: 32px; border-radius: 6px; border: none; display: inline-flex; align-items: center; justify-content: center; text-decoration: none; color: #1a1d23; background: rgba(255,255,255,0.2); font-size: 1rem; transition: background 0.2s; }}
.home-icon:hover {{ background: rgba(255,255,255,0.35); }}
.dropdown {{ position: relative; display: inline-block; }}
.menu-btn {{ border: none; background: rgba(255,255,255,0.2); color: #1a1d23; border-radius: 6px; padding: 0.4rem 0.6rem; cursor: pointer; transition: background 0.2s; }}
.menu-btn:hover {{ background: rgba(255,255,255,0.35); }}
.dropdown-content {{ display: none; position: absolute; top: 100%; left: 0; min-width: 180px; margin-top: 0.35rem; background: var(--panel); border: 1px solid var(--panel-border); border-radius: 10px; padding: 0.35rem 0; z-index: 20; box-shadow: 0 4px 12px rgba(0,0,0,0.15); }}
.dropdown-content a {{ display: block; color: var(--text); text-decoration: none; padding: 0.5rem 0.8rem; font-size: 0.9rem; }}
.dropdown-content a:hover {{ background: var(--accent); opacity: 0.9; }}
.dropdown-content .menu-divider {{ margin: 0.25rem 0.5rem; border: none; border-top: 1px solid var(--panel-border); }}
.dropdown.open .dropdown-content {{ display: block; }}
.button {{ display: inline-flex; align-items: center; justify-content: center; gap: 0.5rem; padding: 0.75rem 1rem; border-radius: 999px; border: 1px solid var(--button-border); background: var(--button-bg); color: var(--text); text-decoration: none; font-weight: 600; transition: background 0.12s ease, border-color 0.12s ease; }}
.button:hover {{ background: var(--accent); border-color: var(--accent); color: #020617; }}
.button.small {{ padding: 0.45rem 0.85rem; font-size: 0.9rem; }}
.button.danger {{ background: #dc2626; border-color: #991b1b; color: white; }}
.notice {{ margin-bottom: 1rem; padding: 1rem 1.1rem; border: 1px solid rgba(148,163,184,0.2); border-radius: 16px; background: rgba(56,189,248,0.08); color: var(--text); }}
.table {{ width: 100%; border-collapse: collapse; margin-top: 1rem; }}
.table th, .table td {{ border: 1px solid rgba(148,163,184,0.16); padding: 0.85rem; text-align: left; }}
.table th {{ color: var(--muted); font-weight: 700; }}
.form-row {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 0.9rem; margin-top: 0.9rem; }}
.fieldset {{ border: 1px solid rgba(148,163,184,0.16); border-radius: 18px; padding: 1rem; margin-top: 1rem; }}
.fieldset legend {{ padding: 0 0.5rem; }}
label {{ display: block; margin-bottom: 0.35rem; color: var(--muted); }}
input[type="text"], select, textarea {{ width: 100%; border: 1px solid rgba(148,163,184,0.18); border-radius: 12px; padding: 0.8rem; background: rgba(255,255,255,0.04); color: var(--text); }}
textarea {{ min-height: 120px; resize: vertical; }}
.checkbox-row {{ display: inline-flex; align-items: center; gap: 0.5rem; margin-top: 0.75rem; }}
.checkbox-list {{ display: grid; gap: 0.5rem; border: 1px solid rgba(148,163,184,0.18); border-radius: 12px; padding: 0.8rem; background: rgba(255,255,255,0.04); }}
.checkbox-list label {{ display: inline-flex; align-items: center; gap: 0.55rem; margin: 0; color: var(--text); }}
.checkbox-list input {{ margin: 0; }}
.panel-key {{ color: var(--muted); font-size: 0.85rem; }}
.form-actions {{ display: flex; flex-wrap: wrap; gap: 0.75rem; margin-top: 1.25rem; align-items: center; }}
.content-wrap {{ max-width: 1200px; margin: 0.9rem auto 0; padding: 0 1.1rem 1.5rem; }}
@media (max-width: 720px) {{ .form-row {{ grid-template-columns: 1fr; }} .topbar {{ flex-direction: column; align-items: stretch; }} }}
</style>
</head>
<body>
<div class="container">
<div class="topbar">
<div class="left-head"><a class="home-icon" href="/" title="Home">🦀</a>{}<h1>{}</h1></div>
</div>
<div class="content-wrap">{}</div>
</div>
<script>
document.addEventListener("click", function(event) {{
  var toggles = document.querySelectorAll(".js-dropdown-toggle");
  for (var i = 0; i < toggles.length; i++) {{
    var toggle = toggles[i];
    var dropdown = toggle.closest(".dropdown");
    if (toggle.contains(event.target)) {{
      var isOpen = dropdown.classList.contains("open");
      dropdown.classList.toggle("open", !isOpen);
      toggle.setAttribute("aria-expanded", String(!isOpen));
    }} else if (!dropdown.contains(event.target)) {{
      dropdown.classList.remove("open");
      toggle.setAttribute("aria-expanded", "false");
    }}
  }}
}});
</script>
</body>
</html>"#, title, render_management_menu(), title, content, color = color, accent = accent, accent_hover = accent_hover)
}

fn render_tabs_page(_config: &Config, links: &Links) -> String {
    let mut content = String::new();
    content.push_str("<h2>Edit Tabs</h2>");
    content.push_str(r#"<table class="table"><thead><tr><th>ID</th><th>Name</th><th>Refresh</th><th>Horizontal</th><th>Panels</th><th>Actions</th></tr></thead><tbody>"#);

    let ordered_tabs: BTreeMap<_, _> = links.tabs.iter().collect();
    for (&tab_id, tab) in &ordered_tabs {
        let panel_names = tab.panels.values().cloned().collect::<Vec<_>>().join(", ");
        content.push_str(&format!(
            r#"<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><form action="/tabs" method="post" style="display:inline;margin:0;"><input type="hidden" name="up" value="{}"><button type="submit" class="button small">Up</button></form> <a class="button small" href="/tab_edit?tab={}">Edit</a></td></tr>"#,
            tab_id,
            html_escape(&tab.name),
            html_escape(&tab.refresh),
            if tab.horiz { "yes" } else { "no" },
            html_escape(&panel_names),
            tab_id,
            tab_id,
        ));
    }
    content.push_str("</tbody></table>");

    content.push_str(r#"<section class="fieldset"><legend>Add new tab</legend><form action="/tabs" method="post"><div class="form-row"><span><label>Name</label><input type="text" name="name" placeholder="Tab name" required></span><span><label>Refresh</label><input type="text" name="refresh" placeholder="60"></span><span class="checkbox-row"><input type="checkbox" name="horiz" id="horiz_new"><label for="horiz_new">Horizontal pane layout</label></span></div><label>Panels</label><div class="checkbox-list">"#);

    let ordered_panels: BTreeMap<_, _> = links.panels.iter().collect();
    for (key, panel) in ordered_panels {
        content.push_str(&format!(r#"<label><input type="checkbox" name="panels" value="{}"><span>{}</span><span class="panel-key">({})</span></label>"#, html_escape(key), html_escape(&panel.name), html_escape(key)));
    }

    content.push_str(r#"</div><div class="form-actions"><button type="submit" class="button">Create tab</button></div></form></section>"#);
    render_page_shell("Edit Tabs", content, &_config.color, &_config.theme)
}

fn render_tab_edit_page(_config: &Config, links: &Links, tab_id: i32, tab: &Tab) -> String {
    // Build ordered list of currently assigned panels (by position key)
    let ordered_assigned: std::collections::BTreeMap<_, _> = tab.panels.iter().collect();
    let assigned_panels: Vec<&String> = ordered_assigned.values().copied().collect();
    let assigned_set: std::collections::HashSet<&String> = assigned_panels.iter().copied().collect();

    let mut content = String::new();
    content.push_str(&format!(
        r#"<h2>Edit Tab: {name}</h2>
<form action="/tab_edit" method="post">
<input type="hidden" name="tab" value="{tab_id}">
<div class="form-row">
  <span><label>Name</label><input type="text" name="name" value="{name}" required></span>
  <span><label>Refresh (seconds)</label><input type="text" name="refresh" value="{refresh}" placeholder="60"></span>
  <span class="checkbox-row"><input type="checkbox" id="horiz_edit" name="horiz" {horiz_checked}><label for="horiz_edit">Horizontal layout</label></span>
</div>"#,
        name = html_escape(&tab.name),
        tab_id = tab_id,
        refresh = html_escape(&tab.refresh),
        horiz_checked = if tab.horiz { "checked" } else { "" },
    ));

    // --- Assigned panels with order controls ---
    content.push_str(r#"<label style="margin-top:1rem;display:block;">Panel order</label>
<div class="panel-order-list">"#);

    let last_idx = assigned_panels.len().saturating_sub(1);
    for (pos, panel_key) in assigned_panels.iter().enumerate() {
        let display_name = links.panels.get(*panel_key)
            .map(|p| p.name.as_str())
            .unwrap_or(panel_key.as_str());

        // Each assigned panel is submitted as a hidden input; Up/Down reorder via JS
        content.push_str(&format!(
            r#"<div class="panel-order-item" data-pos="{pos}">
  <input type="hidden" name="panels" value="{key}">
  <span class="panel-order-name">{display}</span>
  <span class="panel-key">({key})</span>
  <span class="panel-order-btns">
    <button type="button" class="button small" onclick="movePanelUp(this)" {up_disabled}>↑</button>
    <button type="button" class="button small" onclick="movePanelDown(this)" {down_disabled}>↓</button>
    <button type="button" class="button small danger" onclick="removePanel(this)">✕</button>
  </span>
</div>"#,
            pos = pos,
            key = html_escape(panel_key),
            display = html_escape(display_name),
            up_disabled = if pos == 0 { "disabled" } else { "" },
            down_disabled = if pos == last_idx { "disabled" } else { "" },
        ));
    }
    content.push_str("</div>");

    // --- Add panel dropdown (only panels not yet assigned) ---
    let available: Vec<_> = {
        let mut all: Vec<_> = links.panels.keys().collect();
        all.sort();
        all.into_iter().filter(|k| !assigned_set.contains(k)).collect()
    };

    if !available.is_empty() {
        content.push_str(r#"<div class="form-row" style="margin-top:0.75rem;align-items:flex-end;">"#);
        content.push_str(r#"<span><label>Add panel</label><select id="panel-add-select">"#);
        content.push_str(r#"<option value="">— select —</option>"#);
        for key in &available {
            let display = links.panels.get(*key).map(|p| p.name.as_str()).unwrap_or(key.as_str());
            content.push_str(&format!(r#"<option value="{key}">{display} ({key})</option>"#, key = html_escape(key), display = html_escape(display)));
        }
        content.push_str(r#"</select></span>"#);
        content.push_str(r#"<span><button type="button" class="button small" onclick="addPanel()">Add</button></span>"#);
        content.push_str("</div>");
    }

    content.push_str(r#"<div class="form-actions" style="margin-top:1.25rem;">
  <button type="submit" class="button">Save tab</button>
  <button type="submit" name="action" value="delete" class="button danger">Delete tab</button>
  <a class="button" href="/tabs">Cancel</a>
</div>
</form>

<style>
.panel-order-list { display:flex; flex-direction:column; gap:0.4rem; margin-top:0.5rem; border:1px solid rgba(148,163,184,0.18); border-radius:12px; padding:0.6rem; background:rgba(255,255,255,0.04); }
.panel-order-item { display:flex; align-items:center; gap:0.6rem; padding:0.4rem 0.5rem; border-radius:8px; background:rgba(255,255,255,0.04); }
.panel-order-name { font-weight:600; flex:1; }
.panel-order-btns { display:flex; gap:0.3rem; margin-left:auto; }
</style>

<script>
function getList() { return document.querySelector('.panel-order-list'); }

function refreshButtons() {
  var items = getList().querySelectorAll('.panel-order-item');
  items.forEach(function(item, i) {
    item.querySelectorAll('button')[0].disabled = (i === 0);
    item.querySelectorAll('button')[1].disabled = (i === items.length - 1);
  });
}

function movePanelUp(btn) {
  var item = btn.closest('.panel-order-item');
  var prev = item.previousElementSibling;
  if (prev) { getList().insertBefore(item, prev); refreshButtons(); }
}

function movePanelDown(btn) {
  var item = btn.closest('.panel-order-item');
  var next = item.nextElementSibling;
  if (next) { getList().insertBefore(next, item); refreshButtons(); }
}

function removePanel(btn) {
  var item = btn.closest('.panel-order-item');
  var key = item.querySelector('input[name="panels"]').value;
  item.remove();
  refreshButtons();
  // Re-add the key to the dropdown if it exists
  var sel = document.getElementById('panel-add-select');
  if (sel) {
    var opt = document.createElement('option');
    opt.value = key;
    opt.textContent = key;
    sel.appendChild(opt);
  }
}

function addPanel() {
  var sel = document.getElementById('panel-add-select');
  var key = sel.value;
  if (!key) return;
  var displayText = sel.options[sel.selectedIndex].textContent;

  var list = getList();
  var div = document.createElement('div');
  div.className = 'panel-order-item';
  // Use textContent assignment to avoid XSS — never use innerHTML with user data
  var hiddenInput = document.createElement('input');
  hiddenInput.type = 'hidden';
  hiddenInput.name = 'panels';
  hiddenInput.value = key;
  var nameSpan = document.createElement('span');
  nameSpan.className = 'panel-order-name';
  nameSpan.textContent = displayText;
  var keySpan = document.createElement('span');
  keySpan.className = 'panel-key';
  keySpan.textContent = '(' + key + ')';
  var btnsSpan = document.createElement('span');
  btnsSpan.className = 'panel-order-btns';
  var upBtn = document.createElement('button');
  upBtn.type = 'button'; upBtn.className = 'button small';
  upBtn.textContent = '↑'; upBtn.onclick = function() { movePanelUp(upBtn); };
  var downBtn = document.createElement('button');
  downBtn.type = 'button'; downBtn.className = 'button small';
  downBtn.textContent = '↓'; downBtn.onclick = function() { movePanelDown(downBtn); };
  var removeBtn = document.createElement('button');
  removeBtn.type = 'button'; removeBtn.className = 'button small danger';
  removeBtn.textContent = '✕'; removeBtn.onclick = function() { removePanel(removeBtn); };
  btnsSpan.appendChild(upBtn);
  btnsSpan.appendChild(downBtn);
  btnsSpan.appendChild(removeBtn);
  div.appendChild(hiddenInput);
  div.appendChild(nameSpan);
  div.appendChild(keySpan);
  div.appendChild(btnsSpan);
  list.appendChild(div);

  // Remove from dropdown
  sel.remove(sel.selectedIndex);
  sel.value = '';
  refreshButtons();
}
</script>"#);

    render_page_shell("Edit Tab", content, &_config.color, &_config.theme)
}

fn render_panels_page(_config: &Config, links: &Links) -> String {
    let mut content = String::new();
    content.push_str("<h2>Edit Panels</h2>");
    content.push_str(r#"<table class="table"><thead><tr><th>Key</th><th>Name</th><th>Hosts</th><th>Monitored</th><th>Actions</th></tr></thead><tbody>"#);

    let ordered_panels: BTreeMap<_, _> = links.panels.iter().collect();
    for (key, panel) in ordered_panels {
        let monitored = panel.hosts.values().filter(|h| h.scan).count();
        let monitored_cell = if monitored > 0 {
            format!(r#"<span style="color:var(--on)">● {}</span>"#, monitored)
        } else {
            "—".to_string()
        };
        content.push_str(&format!(
            r#"<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><a class="button small" href="/panel_edit?edit={}">Edit</a></td></tr>"#,
            html_escape(key),
            html_escape(&panel.name),
            panel.hosts.len(),
            monitored_cell,
            urlencoding::encode(key),
        ));
    }
    content.push_str("</tbody></table>");

    content.push_str(r#"<section class="fieldset"><legend>Add new panel</legend><form action="/panels" method="post"><div class="form-row"><span><label>Panel key</label><input type="text" name="key" placeholder="panel_key" required></span></div><div class="form-actions"><button type="submit" class="button">Create panel</button></div></form></section>"#);
    render_page_shell("Edit Panels", content, &_config.color, &_config.theme)
}

fn render_panel_edit_page(_config: &Config, panel_key: &str, panel: &Panel) -> String {
    let mut content = String::new();
    content.push_str(&format!(r#"<h2>Edit Panel: {}</h2><form action="/panel_edit" method="post"><input type="hidden" name="oldkey" value="{}"><div class="form-row"><span><label>Panel key</label><input type="text" name="key" value="{}" required></span></div><div class="form-actions"><button type="submit" class="button">Save panel</button><button type="submit" name="action" value="delete" class="button danger">Delete panel</button><a class="button" href="/panels">Cancel</a></div></form>"#,
        html_escape(&panel.name),
        html_escape(panel_key),
        html_escape(panel_key),
    ));

    content.push_str(r#"<section class="fieldset"><legend>Hosts</legend>"#);
    content.push_str(r#"<table class="table"><thead><tr><th>ID</th><th>Name</th><th>Link</th><th>Monitor</th><th>Actions</th></tr></thead><tbody>"#);
    let ordered_hosts: BTreeMap<_, _> = panel.hosts.iter().collect();
    for (&host_id, host) in ordered_hosts {
        let enc_key = urlencoding::encode(panel_key);
        let check_display = if !host.check_url.is_empty() {
            host.check_url.as_str()
        } else if !host.url.is_empty() {
            host.url.as_str()
        } else {
            "—"
        };
        let monitor_cell = if host.scan {
            let method = if check_display.starts_with("http://") || check_display.starts_with("https://") { "HTTP" } else { "TCP" };
            format!(r#"<span style="color:var(--on)">●</span> <span style="color:var(--muted);font-size:0.8em">{}</span>"#, method)
        } else {
            "—".to_string()
        };
        content.push_str(&format!(
            r#"<tr><td>{host_id}</td><td>{name}</td><td style="color:var(--muted);font-size:0.85em;max-width:200px;overflow:hidden;text-overflow:ellipsis">{url}</td><td>{monitor}</td><td><a class="button small" href="/host_edit?panel={enc_key}&host={host_id}">Edit</a> <form action="/panel_edit" method="post" style="display:inline;margin:0;"><input type="hidden" name="key" value="{panel_key}"><input type="hidden" name="oldkey" value="{panel_key}"><input type="hidden" name="host_id" value="{host_id}"><button type="submit" name="action" value="host_up" class="button small">↑</button> <button type="submit" name="action" value="host_down" class="button small">↓</button> <button type="submit" name="action" value="delete_host" class="button small danger">✕</button></form></td></tr>"#,
            host_id = host_id,
            name = html_escape(&host.name),
            url = html_escape(&host.url),
            monitor = monitor_cell,
            enc_key = enc_key,
            panel_key = html_escape(panel_key),
        ));
    }
    content.push_str("</tbody></table>");

    content.push_str(&format!(
        r#"<h3>Add host</h3><form action="/panel_edit" method="post"><input type="hidden" name="key" value="{key}"><input type="hidden" name="oldkey" value="{key}"><div class="form-row"><span><label>Name</label><input type="text" name="host_name" required></span><span><label>Link URL</label><input type="text" name="host_url" placeholder="https://example.com"></span><span><label>Icon</label><input type="text" name="host_icon" placeholder="emoji or image URL"></span></div><div class="form-row"><span><label>Health check address <span style="color:var(--muted);font-weight:400;font-size:0.8em">— http(s):// → HTTP GET, otherwise → TCP connect</span></label><input type="text" name="host_check_url" placeholder="https://example.com  or  192.168.1.1"></span></div><div class="form-row"><span class="checkbox-row"><input type="checkbox" name="host_scan" id="host_scan_add"><label for="host_scan_add">Monitor uptime</label></span></div><div class="form-actions"><button type="submit" name="action" value="add_host" class="button">Add host</button></div></form>"#,
        key = panel_key,
    ));
    content.push_str("</section>");
    render_page_shell("Edit Panel", content, &_config.color, &_config.theme)
}

fn render_host_edit_page(config: &Config, panel_key: &str, host_id: i32, host: &Host) -> String {
    let check_hint = if host.check_url.is_empty() && !host.url.is_empty() {
        host.url.clone()
    } else {
        host.check_url.clone()
    };
    let check_type = if check_hint.starts_with("http://") || check_hint.starts_with("https://") {
        "HTTP(S)".to_string()
    } else if !check_hint.is_empty() {
        // Show the actual port that will be used for TCP connect
        if check_hint.contains(':') {
            let port = check_hint.rsplit(':').next().unwrap_or("80");
            format!("TCP :{}", port)
        } else {
            "TCP :80".to_string()
        }
    } else {
        "—".to_string()
    };

    let mut content = String::new();
    content.push_str(&format!(
        r#"<h2>Edit host: {name}</h2>
<form action="/host_edit" method="post">
<input type="hidden" name="panel" value="{panel_key}">
<input type="hidden" name="host_id" value="{host_id}">
<div class="form-row">
  <span><label>Name</label><input type="text" name="name" value="{name}" required></span>
  <span><label>Link URL</label><input type="text" name="url" value="{url}" placeholder="https://example.com"></span>
  <span>
    <label>Icon
      <span style="color:var(--muted);font-weight:400;font-size:0.8em"> — emoji or image URL</span>
    </label>
    <div style="display:flex;gap:0.5rem;align-items:center">
      <input type="text" name="icon" id="icon_input" value="{icon}" placeholder="🖥️  or  https://…/icon.png" oninput="updateIconPreview(this.value)" style="flex:1">
      <span id="icon_preview" style="width:32px;height:32px;display:flex;align-items:center;justify-content:center;border-radius:8px;background:rgba(148,163,184,0.1);font-size:1.2rem;overflow:hidden;flex-shrink:0">{icon_preview}</span>
    </div>
  </span>
</div>
<div class="form-row">
  <span>
    <label>Health check address
      <span style="color:var(--muted);font-weight:400;font-size:0.8em"> — starts with http(s):// → HTTP GET, otherwise → TCP connect</span>
    </label>
    <input type="text" name="check_url" value="{check_url}" placeholder="https://example.com  or  192.168.1.1">
  </span>
  <span style="align-self:flex-end;padding-bottom:0.8rem;color:var(--muted);font-size:0.85rem">
    Method: <strong>{check_type}</strong>
  </span>
</div>
<div class="form-row">
  <span class="checkbox-row">
    <input type="checkbox" name="scan" id="host_scan_edit" {scan_checked}>
    <label for="host_scan_edit">Monitor uptime</label>
  </span>
  <span>
    <label>Scan interval (seconds)
      <span style="color:var(--muted);font-weight:400;font-size:0.8em"> — 0 = use global default ({global_interval}s)</span>
    </label>
    <input type="text" name="scan_interval" value="{scan_interval}" placeholder="0">
  </span>
</div>
<div class="form-actions">
  <button type="submit" class="button">Save host</button>
  <button type="submit" name="action" value="delete" class="button danger">Delete host</button>
  <a class="button" href="/panel_edit?edit={panel_key_enc}">Back to panel</a>
</div>
</form>
<script>
function updateIconPreview(val) {{
  var el = document.getElementById('icon_preview');
  if (!val) {{ el.innerHTML = ''; el.style.background = 'rgba(148,163,184,0.1)'; return; }}
  if (val.startsWith('http')) {{
    el.innerHTML = '<img src="' + val + '" style="width:100%;height:100%;object-fit:contain" onerror="this.parentNode.innerHTML=\'?\'">';
    el.style.background = 'transparent';
  }} else {{
    el.textContent = val;
    el.style.background = 'rgba(148,163,184,0.1)';
  }}
}}
</script>"#,
        name = html_escape(&host.name),
        panel_key = html_escape(panel_key),
        host_id = host_id,
        url = html_escape(&host.url),
        icon = html_escape(&host.icon),
        icon_preview = if host.icon.is_empty() {
            String::new()
        } else if let Some(src) = safe_img_src(&host.icon) {
            format!(r#"<img src="{}" style="width:100%;height:100%;object-fit:contain">"#, src)
        } else {
            html_escape(&host.icon)
        },
        check_url = html_escape(&check_hint),
        check_type = check_type,
        scan_checked = if host.scan { "checked" } else { "" },
        scan_interval = host.scan_interval,
        global_interval = config.scan_interval,
        panel_key_enc = urlencoding::encode(panel_key),
    ));
    render_page_shell("Edit Host", content, &config.color, &config.theme)
}

fn move_host_order(panel: &mut Panel, host_id: i32, direction: i32) {
    let mut ids: Vec<i32> = panel.hosts.keys().copied().collect();
    ids.sort_unstable();
    let Some(pos) = ids.iter().position(|id| *id == host_id) else {
        return;
    };
    let target_pos = if direction < 0 {
        pos.checked_sub(1)
    } else if pos + 1 < ids.len() {
        Some(pos + 1)
    } else {
        None
    };
    let Some(target_pos) = target_pos else {
        return;
    };
    let other_id = ids[target_pos];
    let current = panel.hosts.remove(&host_id);
    let other = panel.hosts.remove(&other_id);
    match (current, other) {
        (Some(current_host), Some(other_host)) => {
            panel.hosts.insert(other_id, current_host);
            panel.hosts.insert(host_id, other_host);
        }
        (Some(current_host), None) => {
            panel.hosts.insert(host_id, current_host);
        }
        (None, Some(other_host)) => {
            panel.hosts.insert(other_id, other_host);
        }
        (None, None) => {}
    }
}



pub fn render_uptime_page(_config: &Config, links: &Links, history: &[(String, Vec<crate::db::UptimeRecord>)], filter_host: Option<&str>, trim_days: i64) -> String {
    let mut content = String::new();

    if let Some(host) = filter_host {
        content.push_str(&format!(
            r#"<h2>Uptime: {}</h2><p><a href="/uptime" class="button small">← All hosts</a></p>"#,
            html_escape(host)
        ));
    } else {
        content.push_str("<h2>Uptime Statistics</h2>");
    }

    // Count monitored hosts
    let monitored_count: usize = links.panels.values()
        .flat_map(|p| p.hosts.values())
        .filter(|h| h.scan)
        .count();

    if monitored_count == 0 {
        content.push_str(r#"<p class="notice">No hosts are being monitored. Open <a href="/panels">Edit panels</a>, click a host's Edit button, and enable <strong>Monitor uptime</strong>.</p>"#);
        return render_page_shell("Uptime Statistics", content, &_config.color, &_config.theme);
    }

    // Per-host stats
    for (panel_name, records) in history {
        if records.is_empty() { continue; }

        // Group by host
        let mut by_host: std::collections::BTreeMap<&str, Vec<&crate::db::UptimeRecord>> = std::collections::BTreeMap::new();
        for rec in records {
            by_host.entry(rec.host.as_str()).or_default().push(rec);
        }

        if filter_host.is_none() {
            content.push_str(&format!("<h3 style='margin-top:1.5rem'>{}</h3>", html_escape(panel_name)));
        }

        for (host_name, recs) in &by_host {
            let total = recs.len();
            let online = recs.iter().filter(|r| r.status).count();
            let uptime_pct = if total > 0 { online * 100 / total } else { 0 };
            let last_status = recs.last().map(|r| r.status).unwrap_or(false);
            let avg_ms = if online > 0 {
                recs.iter().filter(|r| r.status).map(|r| r.duration).sum::<i64>() / online as i64
            } else { 0 };

            let status_color = if last_status { "var(--on)" } else { "var(--off)" };
            let status_label = if last_status { "Online" } else { "Offline" };

            // In host detail view, show panel name as subheading
            if filter_host.is_some() {
                content.push_str(&format!(
                    r#"<p style="color:var(--muted);margin:0 0 0.5rem">Panel: <strong>{}</strong> — last {} days</p>"#,
                    html_escape(panel_name), trim_days
                ));
            }

            content.push_str(&format!(
                r#"<div class="fieldset" style="margin-bottom:1rem">
<div style="display:flex;align-items:center;gap:1rem;flex-wrap:wrap">
  {host_heading}
  <span style="color:{color}">● {status}</span>
  <span style="color:var(--muted)">Overall uptime: <strong>{pct}%</strong></span>
  <span style="color:var(--muted)">Avg response: <strong>{avg} ms</strong></span>
  <span style="color:var(--muted)">Total checks: <strong>{total}</strong></span>
</div>"#,
                host_heading = if filter_host.is_none() {
                    format!(r#"<span style="font-weight:700;font-size:1rem"><a href="/uptime?panel={}&host={}" style="color:inherit;text-decoration:underline;text-underline-offset:2px">{}</a></span>"#,
                        urlencoding::encode(panel_name),
                        urlencoding::encode(host_name),
                        html_escape(host_name))
                } else {
                    format!(r#"<span style="font-weight:700;font-size:1rem">{}</span>"#, html_escape(host_name))
                },
                color = status_color,
                status = status_label,
                pct = uptime_pct,
                avg = avg_ms,
                total = total,
            ));

            if filter_host.is_some() {
                // ── DETAIL VIEW: timeline grouped by day ──────────────────────────
                // recs are ordered oldest→newest (from get_host_history_days)
                // Group into days (UTC date)
                let mut by_day: std::collections::BTreeMap<String, Vec<&crate::db::UptimeRecord>> = std::collections::BTreeMap::new();
                for rec in recs.iter() {
                    let day = format!("{:04}-{:02}-{:02}",
                        rec.timestamp.year(),
                        rec.timestamp.month() as u8,
                        rec.timestamp.day());
                    by_day.entry(day).or_default().push(rec);
                }

                content.push_str(r#"<div style="margin-top:0.9rem;overflow-x:auto"><table class="table" style="min-width:500px"><thead><tr><th>Date</th><th>Uptime</th><th>Checks</th><th style="min-width:200px">Timeline</th></tr></thead><tbody>"#);

                // Show days newest first
                for (day, day_recs) in by_day.iter().rev() {
                    let d_total = day_recs.len();
                    let d_online = day_recs.iter().filter(|r| r.status).count();
                    let d_pct = if d_total > 0 { d_online * 100 / d_total } else { 0 };
                    let pct_color = if d_pct == 100 {
                        "var(--on)"
                    } else if d_pct >= 90 {
                        "#facc15"
                    } else {
                        "var(--off)"
                    };

                    // Bar: each check = one block, max 96 blocks (1 per 15min for a day)
                    let mut bar_html = String::from(r#"<div class="uptime-bar" style="flex-wrap:nowrap;overflow:hidden">"#);
                    for rec in day_recs.iter() {
                        let c = if rec.status { "var(--on)" } else { "var(--off)" };
                        let ts = format!("{:02}:{:02}", rec.timestamp.hour(), rec.timestamp.minute());
                        let lbl = if rec.status { "Online" } else { "Offline" };
                        bar_html.push_str(&format!(
                            r#"<span class="bar-block" style="background:{c};flex-shrink:0" title="{lbl} {ts} ({dur}ms)"></span>"#,
                            c = c, lbl = lbl, ts = ts, dur = rec.duration
                        ));
                    }
                    bar_html.push_str("</div>");

                    content.push_str(&format!(
                        r#"<tr><td style="white-space:nowrap">{day}</td><td style="color:{pct_color};font-weight:600">{pct}%</td><td style="color:var(--muted)">{total}</td><td>{bar}</td></tr>"#,
                        day = day,
                        pct_color = pct_color,
                        pct = d_pct,
                        total = d_total,
                        bar = bar_html,
                    ));
                }

                content.push_str("</tbody></table></div>");

            } else {
                // ── OVERVIEW: last N checks as a flat bar ─────────────────────────
                content.push_str(r#"<div class="uptime-bar" style="margin-top:0.6rem">"#);
                // recs are newest-first from get_uptime_records; reverse for left=old, right=new
                let bar_recs: Vec<_> = recs.iter().rev().collect();
                for rec in &bar_recs {
                    let c = if rec.status { "var(--on)" } else { "var(--off)" };
                    let ts = format!("{:04}-{:02}-{:02} {:02}:{:02}",
                        rec.timestamp.year(), rec.timestamp.month() as u8, rec.timestamp.day(),
                        rec.timestamp.hour(), rec.timestamp.minute());
                    let lbl = if rec.status { "Online" } else { "Offline" };
                    content.push_str(&format!(
                        r#"<span class="bar-block" style="background:{c}" title="{lbl} — {ts} ({dur}ms)"></span>"#,
                        c = c, lbl = lbl, ts = ts, dur = rec.duration
                    ));
                }
                content.push_str("</div>");
            }

            content.push_str("</div>"); // close fieldset
        }
    }

    // CSS for bar blocks
    content.push_str(r#"<style>
.uptime-bar { display:flex; gap:2px; flex-wrap:wrap; }
.bar-block { width:10px; height:20px; border-radius:3px; display:inline-block; cursor:default; }
</style>"#);

    render_page_shell("Uptime Statistics", content, &_config.color, &_config.theme)
}

fn render_config_page(config: &Config) -> String {
    let theme_options = [
        "minty", "cerulean", "cosmo", "cyborg", "darkly", "flatly", "journal", "litera",
        "lumen", "lux", "materia", "morph", "pulse", "quartz", "sandstone", "simplex",
        "sketchy", "slate", "solar", "spacelab", "superhero", "united", "vapor",
    ];
    let mut theme_select_html = String::new();
    for theme in theme_options {
        let selected = if config.theme == theme { "selected" } else { "" };
        theme_select_html.push_str(&format!(
            r#"<option value="{}" {}>{}</option>"#,
            theme, selected, theme
        ));
    }

    let color_options = [("auto", "Auto (system preference)"), ("light", "Light"), ("dark", "Dark")];
    let mut color_select_html = String::new();
    for (value, label) in color_options {
        let selected = if config.color == value { "selected" } else { "" };
        color_select_html.push_str(&format!(
            r#"<option value="{}" {}>{}</option>"#,
            value, selected, label
        ));
    }

    let mut content = String::new();
    content.push_str(r#"<h2>Config</h2><form action="/config" method="post">"#);
    content.push_str(&format!(
        r#"<div class="form-row"><span><label>Host</label><input type="text" name="host" value="{}"></span><span><label>Port</label><input type="text" name="port" value="{}"></span><span><label>Color mode</label><select name="color">{}</select></span></div>"#,
        config.host, config.port, color_select_html
    ));
    content.push_str(&format!(
        r#"<div class="form-row"><span><label>Theme</label><select name="theme">{}</select></span><span><label>Button width</label><input type="text" name="btn_width" value="{}"></span><span><label>Web refresh (seconds)</label><input type="text" name="web_refresh" value="{}"></span><span><label>Scan interval (seconds)</label><input type="text" name="scan_interval" value="{}"></span></div>"#,
        theme_select_html, config.btn_width, config.web_refresh, config.scan_interval
    ));
    content.push_str(&format!(
        r#"<div class="form-row"><span><label>DB trim days</label><input type="text" name="db_trim_days" value="{}"></span><span><label>Panel gap</label><input type="text" name="panel_gap" value="{}" placeholder="12px"></span></div>"#,
        config.db_trim_days, config.panel_gap
    ));
    content.push_str(&format!(
        r#"<div class="form-row"><span><label>Nav font size</label><input type="text" name="nav_font_size" value="{}" placeholder="0.85rem"></span><span><label>Button font size</label><input type="text" name="btn_font_size" value="{}" placeholder="0.8rem"></span><span><label>Button gap</label><input type="text" name="btn_gap" value="{}" placeholder="8px"></span></div>"#,
        config.nav_font_size, config.btn_font_size, config.btn_gap
    ));
    content.push_str(&format!(
        r#"<div class="form-row"><span class="checkbox-row"><input type="checkbox" name="center_columns" id="center_columns" {}><label for="center_columns">Center panel columns layout</label></span></div>"#,
        if config.center_columns { "checked" } else { "" }
    ));
    content.push_str(&format!(
        r#"<div class="form-row"><span class="checkbox-row"><input type="checkbox" name="panel_border" id="panel_border" {}><label for="panel_border">Show panel borders</label></span></div>"#,
        if config.panel_border { "checked" } else { "" }
    ));
    content.push_str(r#"<div class="form-actions"><button type="submit" class="button">Save config</button></div></form>"#);
    render_page_shell("Config", content, &config.color, &config.theme)
}

fn render_board_edit_page(config: &Config, content_text: &str) -> String {
    let mut content = String::new();
    content.push_str(r#"<h2>Edit board file</h2><form action="/board_edit" method="post">"#);
    content.push_str(&format!(
        r#"<label>board.yaml</label><textarea name="content" style="min-height:420px;">{}</textarea>"#,
        html_escape(content_text)
    ));
    content.push_str(r#"<div class="form-actions"><button type="submit" class="button">Save board file</button></div></form>"#);
    render_page_shell("Edit board file", content, &config.color, &config.theme)
}

fn render_about_page(config: &Config) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let content = format!(r#"<h2>About tinyboard</h2>
<p class="notice" style="font-size:1rem">
  <strong>tinyboard v{version}</strong> — a lightweight self-hosted dashboard for your homelab.<br>
  Inspired by <a href="https://github.com/aceberg/miniboard" target="_blank" rel="noreferrer">miniboard</a> by aceberg, but built differently: pure Rust, redb storage, parallel uptime scanning, per-host intervals, and ~2 MB RAM.
</p>
<table class="table" style="max-width:600px">
  <tbody>
    <tr><td style="color:var(--muted)">Version</td><td><strong>{version}</strong></td></tr>
    <tr><td style="color:var(--muted)">Language</td><td>Rust + Axum</td></tr>
    <tr><td style="color:var(--muted)">Storage</td><td>YAML (board.yaml, config.yaml) + redb (uptime)</td></tr>
    <tr><td style="color:var(--muted)">Uptime checks</td><td>HTTP GET or TCP connect (no root required)</td></tr>
    <tr><td style="color:var(--muted)">Authentication</td><td>None built-in — use a reverse proxy (Authelia, tinyauth, etc.)</td></tr>
    <tr><td style="color:var(--muted)">API</td><td><a href="/api/links">/api/links</a> — board data as JSON</td></tr>
  </tbody>
</table>
<div class="form-actions" style="margin-top:1.5rem">
  <a class="button" href="https://github.com/aceberg/miniboard" target="_blank" rel="noreferrer">Inspired by miniboard</a>
  <a class="button" href="/api/links" target="_blank">API: /api/links</a>
</div>"#, version = version);
    render_page_shell("About", content, &config.color, &config.theme)
}

fn theme_accent(theme: &str) -> (&'static str, &'static str) {
    match theme.to_ascii_lowercase().as_str() {
        "minty" => ("#78c2ad", "#63ad99"),
        "cerulean" => ("#2fa4e7", "#2285bf"),
        "cosmo" => ("#2780e3", "#1f69bb"),
        "cyborg" => ("#2a9fd6", "#2280ad"),
        "darkly" => ("#375a7f", "#2d4a69"),
        "flatly" => ("#18bc9c", "#149a80"),
        "journal" => ("#eb6864", "#cf5753"),
        "litera" => ("#4582ec", "#376ad0"),
        "lumen" => ("#158cba", "#11779f"),
        "lux" => ("#1a1a1a", "#111111"),
        "materia" => ("#2196f3", "#1a7fce"),
        "morph" => ("#6ea8fe", "#4f8be8"),
        "pulse" => ("#593196", "#47287a"),
        "quartz" => ("#74a2f4", "#5c86cf"),
        "sandstone" => ("#93c54b", "#7ba83d"),
        "simplex" => ("#d9230f", "#b51d0c"),
        "sketchy" => ("#333333", "#262626"),
        "slate" => ("#3a3f44", "#30343a"),
        "solar" => ("#b58900", "#946f00"),
        "spacelab" => ("#446e9b", "#365a80"),
        "superhero" => ("#4e5d6c", "#3f4c59"),
        "united" => ("#e95420", "#c64619"),
        "vapor" => ("#6f42c1", "#59359a"),
        _ => ("#79cbbf", "#5fb4a8"),
    }
}

fn normalize_css_size(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    if trimmed.ends_with("px") || trimmed.ends_with("rem") || trimmed.ends_with("em") || trimmed.ends_with('%') {
        return trimmed.to_string();
    }
    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return format!("{}px", trimmed);
    }
    fallback.to_string()
}

/// Escape for HTML text content and attribute values.
fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Sanitize a URL for use in href attributes.
/// Only allows http://, https://, and # — everything else (javascript:, data:, etc.) is replaced with #.
fn safe_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed == "#" {
        return "#".to_string();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        html_escape(trimmed)
    } else {
        "#".to_string()
    }
}

/// Sanitize a URL for use in img src attributes.
/// Only allows http:// and https:// — blocks data:, javascript:, and anything else.
fn safe_img_src(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(html_escape(trimmed))
    } else {
        None
    }
}

/// Validate a CSS size value — only allow known-safe units.
/// Returns the value if valid, otherwise returns the fallback.
fn safe_css_size(value: &str, fallback: &str) -> String {
    normalize_css_size(value, fallback)
}

/// Validate a CSS color keyword — only allow "auto", "light", "dark".
fn safe_css_color(value: &str) -> &str {
    match value {
        "auto" | "light" | "dark" => value,
        _ => "auto",
    }
}

/// Validate a theme name — only allow known Bootswatch themes.
fn safe_theme(value: &str) -> &str {
    match value.to_ascii_lowercase().as_str() {
        "minty" | "cerulean" | "cosmo" | "cyborg" | "darkly" | "flatly" | "journal"
        | "litera" | "lumen" | "lux" | "materia" | "morph" | "pulse" | "quartz"
        | "sandstone" | "simplex" | "sketchy" | "slate" | "solar" | "spacelab"
        | "superhero" | "united" | "vapor" => value,
        _ => "minty",
    }
}
