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
<style>
@font-face {{ font-family: 'Montserrat'; font-style: normal; font-weight: 400; font-display: swap; src: url('data:font/woff2;base64,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') format('woff2'); }}
@font-face {{ font-family: 'Montserrat'; font-style: normal; font-weight: 500; font-display: swap; src: url('data:font/woff2;base64,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') format('woff2'); }}
</style>
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
.panel-card h3 {{ margin: 0 0 0.85rem 0; font-size: 1rem; }}
.panel-card h3 a {{ color: inherit; text-decoration: underline; text-underline-offset: 2px; }}
.panel-card.no-border {{ border-color: transparent; background: transparent; box-shadow: none; }}
.panel-card h3 {{ text-align: center; }}
.host-grid {{ display: grid; gap: var(--btn-gap, 8px); grid-template-columns: minmax(var(--btn-width, 180px), var(--btn-width, 180px)); justify-content: center; }}
.host-button {{ display: flex; gap: 0.45rem; align-items: center; justify-content: center; text-align: center; width: var(--btn-width, 180px); min-height: 30px; padding: 0.32rem 0.6rem; border-radius: 6px; background: var(--accent); border: none; color: var(--topbar-text); text-decoration: none; font-family: Montserrat, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; transition: transform 0.1s, filter 0.2s; }}
.host-button:hover {{ transform: translateY(-1px); filter: brightness(1.05); }}
.host-icon {{ width: 26px; height: 26px; border-radius: 8px; background: rgba(255,255,255,0.25); display: inline-flex; align-items: center; justify-content: center; overflow: hidden; flex-shrink: 0; }}
.host-icon img {{ width: 100%; height: 100%; object-fit: contain; display: block; }}
.host-title {{ margin: 0; font-size: var(--btn-font-size); font-weight: 500; line-height: 1.1; color: var(--topbar-text); }}
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
        accent_dark = accent_dark
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
