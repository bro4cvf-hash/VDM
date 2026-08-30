# VDM (Visk Download Manager) — Complete Project Architecture & Engineering Blueprint

> **Mandatory Pre-Session Protocol**: Inspect and reference this blueprint before modifying any code or making architectural changes. This document maps every module, lifecycle state machine, SQLite table, Slint property bridge, UI alignment invariant, and release build procedure.

---

## 1. High-Level Technology Stack & System Requirements

* **Language**: Rust 1.75+ (Edition 2021)
* **Async Runtime**: Tokio 1.x (multi-threaded runtime with full feature set)
* **UI Engine**: Slint 1.12 (`i-slint-backend-winit` v1.17.1), hardware-accelerated via Winit windowing
* **HTTP Client**: Reqwest 0.12 (TLS via `rustls-tls`, streaming chunks with `bytes::Bytes`)
* **Persistence**: SQLite 3 (`rusqlite` bundled v0.32) in Write-Ahead Logging (WAL) mode
* **System Integration**:
  * `tray-icon` v0.21 (native Windows system tray integration with morphing context menu)
  * `rfd` v0.15 (native Windows async file/folder selection dialogs)
  * `resvg` v0.45 + `tiny-skia` (pure-Rust SVG rendering to RGBA pixel buffers)
  * `windows-sys` v0.59 (Win32 API bindings for taskbar attention, shell icon extraction, and window focusing)
* **Installer**: Inno Setup 6 (compiled via `scripts/build-installer.ps1`)

---

## 2. Exhaustive Directory & File Map

```
g:\AI\VDM/
├── PROJECT.md                             # Exhaustive architectural blueprint & system map (this file)
├── AGENTS.md                              # Pre-session protocol, Rust invariants & UI design rules
├── Cargo.toml                             # Crate metadata, dependencies & aggressive release profile
├── Cargo.lock                             # Pinned dependency lockfile
├── build.rs                               # Slint UI compiler & Windows resource (.ico / manifest) embedder
├── README.md                              # Public documentation & installation guide
│
├── docs/                                  # Modular deep-dive technical documentation
│
├── src/
│   ├── main.rs                            # Host entrypoint: Slint event loop, tray, UI pollers, dialog routing
│   ├── engine/
│   │   ├── mod.rs                         # Re-exports: Manager, TaskSnapshot, TaskStatus
│   │   ├── downloader.rs                  # Master orchestrator: Task lifecycle, chunk partitioning, speed sampling
│   │   ├── worker.rs                      # Per-chunk HTTP Range worker: Work-stealing, chunk streaming, sparse disk IO
│   │   ├── probe.rs                       # Multi-probe engine: HEAD / Range GET probing & yt-dlp metadata extraction
│   │   ├── ytdl.rs                        # yt-dlp wrapper: Format string extraction, audio/video stream muxing via ffmpeg
│   │   ├── rate_limiter.rs                # Token-bucket bandwidth limiter with burst headroom calculation
│   │   ├── server.rs                      # Loopback HTTP server on 127.0.0.1:9191 (extension & single-instance IPC)
│   │   ├── browser.rs                     # Registry scanner for Chrome / Edge / Brave & extension installer
│   │   ├── startup.rs                     # Windows Startup integration (HKCU Run & StartupApproved sync)
│   │   ├── file_allocator.rs              # Disk space allocation (sparse files & SetFileValidData optimizations)
│   │   └── sys_icon.rs                    # Shell icon extractor via SHGetFileInfoW with Slint Image caching
│   └── storage/
│       ├── mod.rs                         # Re-exports: Db
│       └── database.rs                    # SQLite schema: tasks, chunks, and key-value settings store
│
├── ui/
│   ├── app-window.slint                   # Primary Slint export manifest (bridges components to Rust)
│   ├── theme.slint                        # macOS Sonoma dark palette definitions & global tokens
│   ├── types.slint                        # Shared data models: DownloadItem, BrowserItem, ChunkInfo
│   ├── windows/
│   │   └── main-window.slint              # Core desktop window (orchestrates modular views & components)
│   ├── views/                             # Modular UI sub-panels (sidebar, toolbar, table, status bar)
│   ├── components/                        # Reusable granular widgets
│   │   ├── buttons.slint                  # CaptionBtn, PrimaryBtn, IconBtn, PillBtn
│   │   ├── inputs.slint                   # Field (rounded text input), SearchField (pill search input)
│   │   ├── dropdown.slint                 # Custom popup dropdown selector
│   │   ├── icon.slint                     # Resvg-rendered Slint Icon and ToolIcon with tooltip
│   │   ├── table.slint                    # HeaderCol (sort/resize), ColMenuItem, SideItem, SectionHeader
│   │   └── context-menu.slint             # Apple HIG right-click context menu and submenus
│   ├── dialogs/                           # Discrete modal windows and overlays
│   │   ├── download-info-dialog.slint     # Initial download confirmation, category, path selector
│   │   ├── download-progress-dialog.slint # Active download window with per-chunk visual progress bar
│   │   ├── download-mini-pill.slint       # Compact floating progress pill overlay
│   │   ├── complete-dialog.slint          # Download finished prompt (Open, Open Folder, shutdown PC)
│   │   ├── duplicate-dialog.slint         # Existing file / duplicate URL conflict resolver
│   │   ├── renew-dialog.slint             # Expired / broken link recovery dialog
│   │   ├── settings-dialog.slint          # Configuration (speed limits, connections, categories, browsers)
│   │   └── tray-menu.slint                # System tray context menu
│   └── icons/                             # 26+ custom stroke SVG icons
│
├── extension/                             # Manifest V3 browser companion extension
│   ├── manifest.json
│   ├── background.js                      # Stream interceptor & loopback forwarder
│   └── content.js                         # DOM media detector & context menu integration
│
├── scripts/
│   ├── build-installer.ps1                # Inno Setup compilation script
│   └── vdm.iss                            # Inno Setup installer definition
└── assets/                                # App icons, splash screens, and installer artwork
```

---

## 3. Engine & Concurrency Architecture

### A. Download Task State Machine (`src/engine/downloader.rs`)
Tasks traverse the following explicit lifecycle states:
```
[Queued] ──> [Connecting / Probing] ──> [Downloading] ──┬──> [Completed]
    │                 │                     │           ├──> [Paused]
    └─────────────────┴─────────────────────┴───────────┴──> [Error]
```
1. **Probe Phase (`probe.rs`)**:
   * Probes URL headers via `HEAD` request (with a 5-second timeout).
   * Falls back to `GET` with `Range: bytes=0-0` if `HEAD` fails or returns 405 Method Not Allowed.
   * Extracts `Content-Length`, `Accept-Ranges`, `ETag`, `Last-Modified`, and `Content-Disposition` (with RFC 5987 / RFC 6266 filename decoding).
   * Identifies video sites (YouTube, Vimeo, etc.) and routes them to `ytdl.rs`.
2. **Chunk Partitioning & Worker Dispatch (`worker.rs`)**:
   * File is divided into $N$ equal chunks based on `max_connections` (up to 32 parallel connections, `MIN_CHUNK_TARGET = 512 KB`).
   * Each chunk corresponds to a `Cell` tracking `start`, `end`, `done`, and instantaneous `speed`.
   * Chunks stream data directly to their sparse disk offset using `tokio::fs::File` with `seek(SeekFrom::Start(offset))`.
3. **Dynamic Work-Stealing Algorithm**:
   * If a fast worker completes its byte range while other workers are still downloading large ranges, the idle worker identifies the chunk with the largest remaining byte delta.
   * It splits that chunk's remaining range in half, claims the second half, and spawns a new sub-connection to maximize bandwidth utilization.
4. **State Persistence**:
   * Chunk progress is periodically committed to SQLite (`storage/database.rs`).
   * On application restart or crash recovery, VDM inspects `chunks` rows to resume downloading from the exact byte position.

### B. yt-dlp & Audio/Video Muxing Engine (`src/engine/ytdl.rs`)
* Detects YouTube / video platform URLs.
* Executes `yt-dlp` to extract DASH streams.
* When separate DASH video and audio streams are returned (e.g. 1080p video + opus audio), VDM downloads both streams concurrently to temporary files and invokes ffmpeg (`mux_audio_video`) to produce a lossless merged container (`.mp4` / `.mkv`).

### C. Rate Limiting Engine (`src/engine/rate_limiter.rs`)
* Implements a precision token-bucket algorithm:
  $$\text{Capacity} = \max(\text{Rate} \times 1.5, 64\text{ KB})$$
* Workers asynchronously call `limiter.acquire(bytes)` before reading stream chunks, providing smooth, jitter-free bandwidth throttling from 2 MB/s up to unlimited.

### D. Loopback Server (`src/engine/server.rs`)
* Runs an async TCP listener on `127.0.0.1:9191`.
* **Security**: Enforces strict origin filtering (only `chrome-extension://`, `moz-extension://`, `safari-extension://`, `http://localhost`, and `http://127.0.0.1` are permitted; arbitrary web origins are blocked to prevent CSRF/DNS-rebinding).
* **Endpoints**:
  * `POST /download`: Receives `DownloadPayload` (`url`, `filename`, `referrer`, `cookies`, `user_agent`, `file_size`) from browser extensions.
  * `GET /ping` or single-instance call: Brings the existing VDM window to the foreground and focuses it.

---

## 4. SQLite Storage Schema (`src/storage/database.rs`)

Database is located at `%APPDATA%\VDM\vdm.db` (or local directory in portable mode).
* **Pragmas**: `journal_mode = WAL`, `synchronous = NORMAL`, `busy_timeout = 5000ms`.

```sql
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    filename TEXT NOT NULL,
    dir TEXT NOT NULL,
    total INTEGER DEFAULT 0,
    etag TEXT DEFAULT '',
    last_modified TEXT DEFAULT '',
    accept_ranges INTEGER DEFAULT 0,
    multi_conn INTEGER DEFAULT 0,
    status TEXT DEFAULT 'queued',
    created_at INTEGER NOT NULL,
    headers_json TEXT DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS chunks (
    task_id TEXT NOT NULL,
    idx INTEGER NOT NULL,
    start INTEGER NOT NULL,
    end INTEGER NOT NULL,
    done INTEGER DEFAULT 0,
    PRIMARY KEY(task_id, idx)
);

CREATE TABLE IF NOT EXISTS kv (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);
```

---

## 5. Slint UI Architecture & Apple HIG Design System

### A. Surface Palette & Layering (macOS Sonoma Dark Glass)
* **Base Canvas**: `#1E1E1E` (the flat, non-distracting window foundation)
* **Elevated Surface / Table / Cards**: `#28282A`
* **Input Controls / Search Bar**: `#2C2C2E`
* **Hover State**: `#232325` / `#333336`
* **Primary Accent**: `#0A84FF` (macOS Blue)
* **Secondary Accent**: `#64D2FF` (macOS Cyan)
* **Success Indicator**: `#30D158` (macOS Green)
* **Destructive / Close**: `#FF453A` (macOS Red)
* **Warning**: `#FF9F0A` (macOS Orange)
* **Separators / Borders**: Translucent `#333336` or `#3A3A3C` (never stark or contrasting)

### B. Modular Slint Component Architecture (Anti-Monolith Standard)
* **Never Dump Code into Monolithic Files**: Do not continuously append UI panels, dialogs, or complex controls into `main-window.slint`.
* **Structured Directory Decomposition**:
  * `ui/components/`: Granular controls (`buttons.slint`, `inputs.slint`, `dropdown.slint`, `icon.slint`, `table.slint`).
  * `ui/views/`: Main distinct application sections (`sidebar.slint`, `toolbar.slint`, `table-view.slint`, `statusbar.slint`).
  * `ui/dialogs/`: Individual modal windows (`download-info-dialog.slint`, `settings-dialog.slint`, etc.).

### C. Strict Uniform Linear Alignment
Every list, sidebar row, table column, dialog row, and toolbar button must align on exact coordinate axes:
* **Sidebar Rows (`SideItem`)**:
  * Fixed height: `32px`
  * Padding: `padding-left: 9px; padding-right: 9px; spacing: 9px;`
  * Icon box: Fixed `width: 16px;`, icons centered (`x: (parent.width - self.width) / 2`), ensuring all text labels align with 0px variance.
* **Table Rows**:
  * Fixed column widths mapped to `HeaderCol` state (`col-filename-width`, `col-size-width`, `col-progress-width`, `col-status-width`, `col-eta-width`, `col-date-width`).
  * Text vertical centering: `vertical-alignment: center;`.
* **Prohibited**: Never introduce arbitrary margin offsets, asymmetrical padding, or staggered text offsets:
  ```
  CORRECT:
  LINE 1: [Icon] [Label] [Count]
  LINE 2: [Icon] [Label] [Count]
  LINE 3: [Icon] [Label] [Count]

  PROHIBITED:
  LINE 1: [Icon] [Label] [Count]
  LINE 2:   [Icon] [Label] [Count]
  LINE 3:  [Icon] [Label] [Count]
  ```

### D. Squircles & Fluid Morph Animations
* **Squircle Radii**:
  * Small buttons, badges, inputs: `border-radius: 6px;`
  * Dialog cards, modal surfaces: `border-radius: 10px;` to `12px;`
* **Transition Timings**:
  * Background / color hover: `duration: 120ms - 150ms; easing: ease-out;`
  * Sidebar sliding pill indicator: `animate y { duration: 150ms; easing: ease-out; }`
  * Speed meter pill: Smooth morph between inactive and active download states.

### E. Slint Layout Conventions & Gotchas
1. **Top-Alignment Default**: Slint `VerticalLayout` and `HorizontalLayout` default to `start` alignment; explicit `alignment: center` must be provided when vertical centering is required.
2. **Input Fields**: Standard `LineEdit` has rigid sizing; use custom `Field` and `SearchField` from `ui/components/inputs.slint`.
3. **Z-Ordering**: Modals, column menus (`ColMenuItem`), and dropdowns must be declared at the bottom of the Slint file to overlay table components.

---

## 6. Build, Optimization & Release Pipeline

### A. Release Profile Configuration (`Cargo.toml`)
```toml
[profile.release]
opt-level = 3
lto = "fat"
strip = true
codegen-units = 1
panic = "abort"
```

### B. Compilation Workflow
1. **Release Build**:
   ```powershell
   cargo build --release
   ```
   * Compiles Slint templates to optimized native C++/Rust code via `slint-build`.
   * Embeds Windows application icon and version metadata via `winresource`.
   * Generates `target\release\vdm.exe` (~30 MB standalone executable).

2. **Installer Creation**:
   ```powershell
   powershell -ExecutionPolicy Bypass -File .\scripts\build-installer.ps1
   ```
   * Compiles `scripts\vdm.iss` into `dist\VDM-Setup-<version>.exe`.

---

## 7. Modular Technical Documentation & Reference Index (`docs/*.md`)

For domain-specific subsystems, deep protocols, or specialized edge cases, dedicated reference documents are maintained under `docs/`:

* *(Additional subsystem deep-dives are created autonomously in `docs/<topic>.md` as new specialized logic is developed and cross-referenced here.)*