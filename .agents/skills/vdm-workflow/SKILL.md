---
name: vdm-workflow
description: End-to-end development workflow for the VDM project covering Slint UI layout styling, modular component decomposition, Rust engine changes, release compilation, Apple HIG alignment auditing, proactive skill utilization, and autonomous knowledge updates.
---

# VDM Development & Engineering Workflow

Use this skill when developing, debugging, refactoring, or testing features in the VDM download manager.

## 1. Codebase Architecture Navigation
- Before touching any module, inspect [PROJECT.md](file:///g:/AI/VDM/PROJECT.md) and [AGENTS.md](file:///g:/AI/VDM/AGENTS.md).
- **Core Engine Modules**:
  - `src/main.rs`: Window event loop, Slint callbacks, system tray, background polling timer (50ms diffing).
  - `src/engine/downloader.rs`: Multi-connection orchestration, progress snapshot computation, task state machine.
  - `src/engine/worker.rs`: HTTP Range chunk fetching, work stealing, sparse disk IO.
  - `src/engine/probe.rs`: Multi-probe URL analysis (HEAD / Range GET) and yt-dlp metadata extraction.
  - `src/engine/ytdl.rs`: yt-dlp binary wrapper, stream extraction & ffmpeg audio/video muxing.
  - `src/engine/rate_limiter.rs`: Token-bucket bandwidth limiter.
  - `src/engine/server.rs`: Loopback server on `127.0.0.1:9191` for extension and single-instance IPC.
  - `src/engine/startup.rs`: Windows Startup integration (`HKCU\...\Run` & `StartupApproved\Run` sync with Task Manager).
  - `src/storage/database.rs`: SQLite WAL persistence for tasks, chunks, and settings.
  - `ui/windows/main-window.slint`: Main desktop UI window (orchestrates imported modular components).
  - `ui/views/`: Major modular panels (sidebar, toolbar, downloads table, status bar).
  - `ui/dialogs/`: Modal dialogs and notifications.
  - `ui/components/`: Reusable primitive components.

## 2. Proactive Skill Utilization Checklist
Whenever executing tasks in this codebase, evaluate and leverage specialized skills:
- **`vdm-workflow`**: Apply Apple HIG design system, Slint layout rules, and release verification.
- **`tavily`** & **`firecrawl`**: Use for live web research, scraping documentation, and investigating upstream crates/APIs.
- **`supermemory`**: Query and persist project decisions, architecture patterns, and session learnings across workspaces.
- **`morph-fast-apply`**: Utilize for rapid codebase exploration (WarpGrep) and high-speed partial code patching.
- **`context7`**: Fetch accurate, version-specific library docs to prevent API hallucinations.

## 3. Slint UI Design & Modularization Checklist
- **Modular Component Architecture (Anti-Monolith Invariant)**:
  - Do NOT dump new UI logic or large blocks of code into `main-window.slint`.
  - Place reusable widgets in `ui/components/<name>.slint`.
  - Place major layout panels in `ui/views/<name>.slint`.
  - Place distinct modal windows in `ui/dialogs/<name>.slint`.
  - Re-export and import cleanly via `ui/app-window.slint` or `main-window.slint`.
- **Borderless Aesthetic & Reactive Multi-Theme System (`Palette`)**:
  - All components, views, dialogs, and controls must consume colors exclusively from `Palette.<token>` in `ui/theme.slint` (`Palette.base-bg`, `Palette.card-bg`, `Palette.surface-bg`, `Palette.input-bg`, `Palette.border-color`, `Palette.accent`, `Palette.text-primary`, `Palette.text-secondary`, etc.).
  - Never hardcode `#1E1E1E` or `#FFFFFF` in views/dialogs.
  - Slint's `export global Palette` computes tokens reactively in $O(1)$ time based on `active-theme: int` (`0` = Dark, `1` = Light, `2` = Dark Purple, `3` = Ocean Slate, `4` = OLED Black).
  - Theme switching updates all open windows instantly without recreating widgets or causing layout lag.
- **Strict Uniform Linear Alignment**:
  - All repeated rows (sidebar items, table rows, settings items, torrent file rows) must share identical column widths, padding, and alignment properties:
    ```
    LINE 1: [Checkbox] [Icon] [Label ...........................] [Size]
    LINE 2: [Checkbox] [Icon] [Label ...........................] [Size]
    LINE 3: [Checkbox] [Icon] [Label ...........................] [Size]
    ```
  - Icons and checkboxes must be enclosed in fixed-width containers (`width: 16px` or `20px`) with centered icon placement (`x: (parent.width - self.width) / 2`).
  - **HorizontalLayout Alignment Invariant**:
    - Never set `alignment: center` on a `HorizontalLayout` for rows where child elements should spread left-to-right across the parent width. In Slint, `alignment: center` clusters all children into the horizontal middle and disables `horizontal-stretch: 1`.
    - For vertical centering across a row, explicitly set `vertical-alignment: center;` on all `Text` elements and `y: (parent.height - self.height) / 2;` on all icons, buttons, and custom controls.
- **Slint Keyboard Focus & FocusScope Invariant**:
  - `FocusScope` only captures key events when it has active focus.
  - To ensure global and table shortcuts (`Delete`, `Ctrl+A`, `Up/Down`, `Space`, `Ctrl+C`, `Escape`) always function, call `self.focus()` in `init` on `main-focus := FocusScope`, and explicitly call `main-focus.focus()` on table row clicks, background clicks, sidebar item changes, and modal dismissals.
  - Avoid deeply nested `else if` chains (30+ branches) in Slint `key-pressed` blocks; use individual `if ... { return accept; }` statements or helper functions to keep Slint AST compilation linear and lightweight.
- **Windows Build Script Stack Size Invariant (`build.rs`)**:
  - Large Slint component trees can exceed the default 1MB thread stack on Windows during procedural macro / build-script AST optimization, triggering `0xc00000fd (STATUS_STACK_OVERFLOW)`.
  - Always invoke `slint_build::compile` inside a dedicated thread configured with `std::thread::Builder::new().stack_size(8 * 1024 * 1024)`.

## 4. BitTorrent Engine & Selective File Downloading
- **DHT & Peer Discovery**: `SessionOptions::default()` has `dht: None` by default. When initializing or updating settings, always configure `session_opts.dht = Some(librqbit::DhtSessionConfig::default())` and enable tier-1 public tracker enhancement (`DEFAULT_PUBLIC_TRACKERS`).
- **Selective File Downloads (`only_files`)**:
  - `librqbit::AddTorrentOptions` accepts `only_files: Option<Vec<usize>>`.
  - Pass the user-selected file indices to allocate and stream only the chunks corresponding to the selected files.
  - Store selected file indices in `TaskRow.headers_json` under `"selected_files": [...]`.
- **Pre-flight Metadata Inspection**:
  - Use `session.add_torrent(..., Some(AddTorrentOptions { list_only: true, .. }))` to probe swarm metadata and enumerate files via `meta.info.iter_file_details()`.
  - Provide fallback to magnet `dn` / `xl` attributes if DHT resolution takes longer than pre-flight timeout.
- **HTTP Work-Stealing Bound**: `worker.rs` caps adaptive range cells at 128. Keep this ceiling when changing stealing logic because each new cell is included in periodic snapshots and full chunk-table persistence.

## 5. Safe OS Power Management & Shutdown Controls
- **Never Silently Execute OS Shutdown**: Never invoke `shutdown /s` directly without presenting an on-screen interactive modal to the user.
- **Interactive Countdown Dialog (`ShutdownDialog`)**:
  - Display a 30-second ticking progress bar and prominent "Cancel Shutdown" button.
  - Clicking "Cancel", pressing `Escape`, or quitting the application must immediately execute `shutdown /a` and disarm `SHUTDOWN_ARMED`.
- **Session-Scoped Execution**: Only trigger post-download shutdown if an active download in the current session transitions from running to complete, never because completed tasks exist in historical SQLite rows.
- **Dialog Dismissal Cleanup**: When progress dialogs are closed or cancelled, verify if any other active dialog has shutdown enabled; if not, automatically disarm `SHUTDOWN_ARMED`.

## 6. Zero Idle Disk I/O & In-Memory State Caching
- **Zero Idle Disk I/O Invariant**:
  - The UI runs periodic diffing timers (250ms UI poller) to refresh download speeds, ETA, and progress.
  - Never execute SQLite read queries (e.g. `SELECT * FROM tasks` or `SELECT * FROM chunks`) inside recurring UI pollers.
  - Maintain an in-memory task state cache (`tasks: Mutex<Vec<CachedTask>>`) initialized once upon startup.
  - `Manager::list_downloads`, `snapshot_of`, and `get_task_path` must read exclusively from in-memory task state and live runtimes, guaranteeing 0 B/s disk I/O when idle.
  - SQLite writes only occur during state transitions (e.g., download created, paused, resumed, completed, errored) or 256 KB worker chunk checkpoints.
  - Set `librqbit::SessionOptions.persistence = None` to prevent background BitTorrent session json files from being continuously touched on disk.

## 7. Responsive Window Close, Anti-Hang Teardown & Loopback Security
- **Configurable Close Action**:
  - General Settings provides a choice: "Exit VDM completely" (`"exit"`, default) vs "Minimize to System Tray" (`"tray"`).
  - Stored in SQLite `kv` table under key `"close_action"`.
  - Both Slint caption buttons (`on_close_window`) and native OS close events (`Alt+F4` / taskbar close via `window().on_close_requested`) must route through the configured `close_action`.
- **Graceful Shutdown & Watchdog Teardown Invariants**:
  - Maintain an atomic `is_shutting_down: Arc<AtomicBool>` test-and-set fence across all background loops (`download_rx`, `tray_icon` pump, 250ms poller).
  - On exit (via window close, tray "Quit", or Ctrl+C):
    1. Arm a 900ms hard watchdog thread FIRST (`std::thread::spawn(|| { sleep(900ms); exit(0); })`) to guarantee process termination within 1 second even if OS file locks or drivers stall.
    2. Immediately hide all UI windows and dialogs on the UI thread via `slint::invoke_from_event_loop` to prevent visual lag or ghost HWNDs.
    3. Abort any active shutdown countdown timers.
    4. Call `manager.pause_all()` to synchronously persist chunk progress.
    5. Call `slint::quit_event_loop()`.
    6. Execute `std::process::exit(0)` to cleanly release all file locks, handles, sockets, and resources.
- **Strict Loopback Origin Security**:
  - Loopback server on `127.0.0.1:9191` strictly permits `chrome-extension://`, `moz-extension://`, `safari-extension://`, and exact `localhost` / `127.0.0.1` origins.
  - Subdomains (such as `http://localhost.evil.com`) are rejected with 403 Forbidden to prevent CSRF/DNS-rebinding attacks.

## 8. Rust Release Build & Verification
- After every Rust source modification, execute:
  ```powershell
  cargo build --release
  ```
- **No-Polling Rule**: Do NOT poll or check `manage_task(Action='status')` in a loop while waiting for compilation. Release builds take several minutes. Launch the background command, update the user that the build has been dispatched, and immediately end the turn. The messaging system will wake the agent reactively upon completion.
- Verify that compilation succeeds with zero errors and zero warnings, producing a valid `target\release\vdm.exe`.

## 9. Autonomous Skill & Modular Documentation Evolution
- Whenever you discover a non-obvious bug fix, an undocumented Slint layout quirk, a new database index optimization, or an engine edge case during pair programming:
  1. Immediately update this skill (`SKILL.md`) or relevant specialized skills with the new solution.
  2. Update [PROJECT.md](file:///g:/AI/VDM/PROJECT.md) to preserve the new knowledge across future sessions.
- **Auditing & Updating Outdated Skills**:
  - If any skill references deprecated crate APIs, obsolete Slint syntax, or outdated CLI arguments, immediately rewrite the outdated sections to maintain 100% freshness and accuracy.
- **Creating Modular `.md` Deep-Dives**:
  - When a topic is too intricate or specialized for `PROJECT.md` (e.g., custom IPC protocol details, yt-dlp format selection matrices, shell icon COM extraction quirks), create a dedicated document in `docs/<topic>.md`.
  - Add an entry and link in `PROJECT.md` under the "Modular Technical Documentation & Reference Index" section.
