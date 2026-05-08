# tinyboard

A lightweight self-hosted homelab dashboard with uptime monitoring. Pure Rust — no Node.js, no Python, no C dependencies.

Inspired by [miniboard](https://github.com/aceberg/miniboard) by aceberg, but built differently: redb storage, parallel uptime scanning, per-host scan intervals, and a fraction of the memory footprint.

## Why tinyboard?

| | tinyboard | miniboard (Go) | Homarr / Dashy |
|---|---|---|---|
| RAM | ~2 MB | ~20 MB | 100–300 MB |
| Binary | single static file | single binary | Node.js runtime |
| C deps | **none** | none | none |
| DB | redb (pure Rust) | SQLite | varies |
| Uptime scanning | parallel, per-host interval | sequential | varies |

## Features

- Tabbed dashboard with panels and link buttons
- Per-host uptime monitoring (HTTP GET or TCP connect)
- Per-host configurable scan interval — fast hosts don't wait for slow ones
- Parallel host scanning — a 10s timeout on one host never blocks others
- 30-day uptime history with per-day timeline view
- Full web UI — no config file editing required
- 23 Bootswatch themes with dark/light/auto color mode
- Config via YAML or environment variables
- Docker-ready, single static binary, no runtime dependencies

## Quick start

### Docker Compose (recommended)

```sh
mkdir -p data
cp config.yaml board.yaml data/
# Fix ownership so the container user (UID 10001) can write to the data directory
sudo chown -R 10001:10001 data/
docker compose up -d
```

Dashboard: **http://localhost:8849**

### Build from source

Requires Rust 1.75+. No C toolchain, no perl, no system libraries needed.

```sh
cargo build --release
./target/release/tinyboard -c config.yaml -b board.yaml
```

### Docker (manual)

```sh
docker run -d \
  --name tinyboard \
  -p 8849:8849 \
  -v $(pwd)/data:/data/tinyboard \
  -e TZ=Europe/Warsaw \
  git.kolspace.cc/victor.kolomin/tinyboard:latest
```

## Configuration

### CLI flags

| Flag | Default | Description |
|------|---------|-------------|
| `-c` | `/data/tinyboard/config.yaml` | Config file path |
| `-b` | `/data/tinyboard/board.yaml` | Board file path |

### config.yaml

```yaml
host: 0.0.0.0          # Listen address
port: '8849'           # Web UI port
theme: minty           # Bootswatch theme name (see bootswatch.com)
color: auto            # Color scheme: dark / light / auto
btnwidth: 180px        # Button width (CSS value)
webrefresh: '60'       # Browser page auto-refresh interval (seconds)
scan_interval: '60'    # Uptime scan interval (seconds, server-side default)
dbtrimdays: '30'       # Delete uptime records older than N days
panel_gap: 12px        # Gap between panels (CSS value)
btn_gap: 8px          # Gap between buttons inside a panel
center_columns: true   # Center panel grid
panel_border: true     # Show panel card borders
nav_font_size: 0.85rem
btn_font_size: 0.8rem
```

### Environment variables

| Variable | Description | Default |
|----------|-------------|---------|
| `HOST` | Listen address | `0.0.0.0` |
| `PORT` | Web UI port | `8849` |
| `THEME` | Bootswatch theme | `minty` |
| `COLOR` | Color scheme | `auto` |
| `BTNWIDTH` | Button width | `180px` |
| `WEBREFRESH` | Browser auto-refresh (seconds) | `60` |
| `SCAN_INTERVAL` | Uptime scan interval (seconds) | `60` |
| `DBTRIMDAYS` | Uptime record retention (days) | `30` |
| `PANEL_GAP` | Gap between panels | `12px` |
| `BTN_GAP` | Gap between buttons inside a panel | `8px` |
| `CENTER_COLUMNS` | Center panel grid | `true` |
| `PANEL_BORDER` | Show panel borders | `true` |
| `TZ` | Timezone for uptime timestamps | system |
| `RUST_LOG` | Log level (`info`, `debug`, `warn`) | `info` |

### board.yaml

```yaml
tabs:
  0:
    name: Home
    refresh: ""         # Browser auto-refresh in seconds (empty = off)
    horiz: false        # Horizontal panel layout
    panels:
      0: infra
      1: services

panels:
  infra:
    name: Infrastructure
    hosts:
      0:
        name: Router
        url: http://192.168.1.1
        icon: 🌐
        check_url: 192.168.1.1   # IP/hostname → TCP:80, http(s):// → HTTP GET
        scan: true
        scan_interval: 30        # Override global interval for this host (0 = use global)
      1:
        name: Google DNS
        url: ""
        icon: ""
        check_url: 8.8.8.8:53
        scan: true
        scan_interval: 0         # Use global scan_interval from config.yaml
```

## Uptime monitoring

- Each host with `scan: true` is checked independently on its own timer
- All checks run in parallel — a 10s timeout on one host doesn't delay others
- `check_url` starting with `http://` or `https://` → HTTP GET (any response = online)
- `check_url` without scheme → TCP connect (e.g. `192.168.1.1` → TCP :80, `host:53` → TCP :53)
- History stored in `redb` (pure Rust embedded database, file: `tinyboard.redb`)
- Overview page (`/uptime`) shows last N checks per host as colored blocks
- Detail page (`/uptime?panel=X&host=Y`) shows per-day timeline for the full retention period

## Access control

tinyboard has no built-in authentication. Put it behind a reverse proxy:

- **[Authelia](https://www.authelia.com)** — full SSO/2FA
- **[tinyauth](https://github.com/steveiliop56/tinyauth)** — minimal single-user auth proxy
- **Nginx/Caddy basic auth** — simple HTTP basic authentication

```
# Caddy example
dashboard.example.com {
    basicauth {
        admin $2a$14$...  # bcrypt hash
    }
    reverse_proxy tinyboard:8849
}
```

## API

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Main dashboard |
| `GET` | `/api/links` | Board data as JSON |
| `GET/POST` | `/reload` | Reload config and board from disk |
| `GET/POST` | `/tabs` | Tab list and creation |
| `GET/POST` | `/tab_edit` | Edit a tab |
| `GET/POST` | `/panels` | Panel list and creation |
| `GET/POST` | `/panel_edit` | Edit a panel and its hosts |
| `GET/POST` | `/host_edit` | Edit a host |
| `GET/POST` | `/config` | App settings |
| `GET/POST` | `/board_edit` | Raw board.yaml editor |
| `GET` | `/uptime` | Uptime overview |
| `GET` | `/uptime?panel=X&host=Y` | Per-host uptime timeline |
| `GET` | `/scan` | Trigger immediate scan of all hosts (JSON) |
| `GET` | `/about` | About page |

## Project structure

```
src/
├── main.rs      # Entry point, server setup, CLI args
├── config.rs    # Config loading, env overrides
├── models.rs    # Data types: Host, Panel, Tab, Links
├── routes.rs    # HTTP handlers and HTML rendering
├── state.rs     # Shared app state (Arc<RwLock<AppState>>)
├── db.rs        # redb storage (uptime records)
└── uptime.rs    # Background uptime monitoring, parallel scanning
```

## Logging

```sh
RUST_LOG=info ./tinyboard    # normal
RUST_LOG=debug ./tinyboard   # verbose
RUST_LOG=warn ./tinyboard    # warnings and errors only
```

## Themes

Any [Bootswatch](https://bootswatch.com) theme in lowercase: `flatly`, `darkly`, `cyborg`, `minty`, `lumen`, `solar`, `superhero`, `united`, `vapor`, etc.

## Building the Docker image

```sh
# Local build — no C deps, no perl, fast compile
docker build -t tinyboard .

# Multi-platform
docker buildx build --platform linux/amd64,linux/arm64 -t tinyboard .
```

## CI/CD Pipeline

### Overview

- **Gitea (Woodpecker CI)**: Builds dev images on every `develop` branch push
- **GitHub (Actions)**: Builds and publishes release images on version tags

### Workflow

#### Development builds (Gitea → internal registry)

1. Push to `develop` branch
2. Woodpecker automatically builds and pushes to `git.kolspace.cc/victor.kolomin/tinyboard:dev`
3. Tags: `dev`, `dev-{commit-sha:8}`

```bash
git push origin develop
# → Woodpecker builds and pushes to internal registry
```

#### Release builds (Gitea → GitHub → ghcr.io)

1. Create a version tag on `main` branch
2. Gitea Woodpecker detects the tag and triggers GitHub Actions via API
3. GitHub Actions builds multi-platform image and pushes to `ghcr.io`
4. Tags: `latest`, `{version}`

```bash
git tag v1.0.0
git push origin main --tags
# → Woodpecker triggers GitHub Actions
# → GitHub Actions builds and pushes to ghcr.io/victorkolomin/tinyboard:v1.0.0
```

### Configuration

**Gitea secrets** (`.woodpecker/build.yaml`):
- `gitea_registry_user` — Gitea registry username
- `gitea_registry_pass` — Gitea registry password
- `github_pat` — GitHub Personal Access Token (for triggering Actions)
- `github_repo` — GitHub repository (e.g., `victorkolomin/tinyboard`)

**GitHub secrets** (`.github/workflows/docker.yml`):
- `GITHUB_TOKEN` — automatically provided by GitHub Actions

### Image locations

| Registry | Image | Tags |
|----------|-------|------|
| Gitea (internal) | `git.kolspace.cc/victor.kolomin/tinyboard` | `dev`, `dev-{sha8}` |
| GitHub (public) | `ghcr.io/victorkolomin/tinyboard` | `latest`, `v*` |

## License

MIT
