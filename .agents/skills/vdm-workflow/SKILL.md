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
- **Borderless Aesthetic**: Ensure surfaces blend naturally without high-contrast outlines using macOS Sonoma dark tones (`#1E1E1E`, `#28282A`, `#2C2C2E`, `#0A84FF`).
- **Strict Uniform Linear Alignment**:
  - All repeated rows (sidebar items, table rows, settings items) must share identical column widths, padding, and alignment properties:
    ```
    LINE 1: [Icon] [Label] [Count]
    LINE 2: [Icon] [Label] [Count]
    LINE 3: [Icon] [Label] [Count]
    ```
  - Icons must be enclosed in fixed-width containers (`width: 16px` or `20px`) with centered icon placement (`x: (parent.width - self.width) / 2`).
- **Squircle Curvature**: Apply `border-radius: 6px` (buttons/inputs) or `10px - 12px` (cards/dialogs).
- **Smooth Animations**: Include `animate background`, `animate color`, `animate y` with `duration: 120ms - 200ms; easing: ease-out;`.

## 4. Rust Release Build & Verification
- After every Rust source modification, execute:
  ```powershell
  cargo build --release
  ```
- Verify that compilation succeeds with zero errors and zero warnings, producing a valid `target\release\vdm.exe`.

## 5. Autonomous Skill & Modular Documentation Evolution
- Whenever you discover a non-obvious bug fix, an undocumented Slint layout quirk, a new database index optimization, or an engine edge case during pair programming:
  1. Immediately update this skill (`SKILL.md`) or relevant specialized skills with the new solution.
  2. Update [PROJECT.md](file:///g:/AI/VDM/PROJECT.md) to preserve the new knowledge across future sessions.
- **Auditing & Updating Outdated Skills**:
  - If any skill references deprecated crate APIs, obsolete Slint syntax, or outdated CLI arguments, immediately rewrite the outdated sections to maintain 100% freshness and accuracy.
- **Creating Modular `.md` Deep-Dives**:
  - When a topic is too intricate or specialized for `PROJECT.md` (e.g., custom IPC protocol details, yt-dlp format selection matrices, shell icon COM extraction quirks), create a dedicated document in `docs/<topic>.md`.
  - Add an entry and link in `PROJECT.md` under the "Modular Technical Documentation & Reference Index" section.