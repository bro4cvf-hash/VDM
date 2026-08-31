# VDM AI Pair-Programming Guidelines & Invariants

## 1. Pre-Session Protocol
- **Read `PROJECT.md` First**: Before making architectural decisions or modifying code, inspect [PROJECT.md](file:///g:/AI/VDM/PROJECT.md) to understand the exact module boundaries, Slint property bindings, and database schemas.

## 2. Rust Engineering & Release Invariants
- **Mandatory Release Build**: Whenever changes are made to Rust code (`src/**/*.rs`, `build.rs`, `Cargo.toml`), always run:
  ```powershell
  cargo build --release
  ```
  Ensure the build completes with zero errors, zero warnings, and produces a functional `target\release\vdm.exe`.
- **No-Polling Invariant for Background Tasks**: Never loop or repeatedly check `manage_task(Action='status')` on long-running commands (e.g. `cargo build --release` or large downloads). After launching the command, immediately end your turn; the system's reactive notification mechanism will automatically wake the agent upon task completion.
- **Production-Level Quality**:
  - No dummy/mock placeholders or half-baked logic.
  - Thread safety: Use `Arc<Mutex<T>>`, `AtomicBool`, and Tokio channels responsibly without blocking the async runtime or the UI main thread.
  - Efficient Polling: Maintain UI pollers with fingerprint diffing to prevent unnecessary Slint model rebuilds.

## 3. UI/UX Design System (Apple HIG, Borderless Blending, Uniform Alignment)
- **Borderless & Blended Surfaces**:
  - Do not use harsh borders or contrasting outlines.
  - Use smooth layered tones from the macOS Sonoma palette:
    - Base canvas: `#1E1E1E`
    - Cards / elevated surfaces: `#28282A`
    - Inputs / search fields: `#2C2C2E`
    - Subtle separators / borders: `#333336` / translucent `#3A3A3C`
    - Accent: `#0A84FF`
- **Strict Uniform Alignment (No Jagged / Staggered Lines)**:
  - All list rows, sidebar items, icons, and text labels must align on identical vertical and horizontal axes.
  - Always use uniform padding, margins, and layout widths across lists:
    ```
    LINE 1: [Icon] [Label] [Count]
    LINE 2: [Icon] [Label] [Count]
    LINE 3: [Icon] [Label] [Count]
    ```
  - Never allow arbitrary element nudging, unequal icon box sizes, or staggered text offsets.
- **Squircle Corners & Fluid Morph Animations**:
  - Use continuous squircle radii (`border-radius: 6px` for small items/buttons, `8px` to `12px` for dialogs/cards).
  - Implement smooth transition animations for states (hover, selection, layout shifts, speed pill transitions) with `duration: 120ms` - `200ms` and `easing: ease-out`.
- **Slint Layout Conventions & Anti-Monolith Modularization**:
  - **No Monolithic Files**: Never dump new views, modals, or complex sub-layouts into `main-window.slint`.
  - **Modular Decomposition**:
    - `ui/components/<name>.slint` for reusable controls (buttons, inputs, dropdowns, icons, table headers).
    - `ui/views/<name>.slint` for major distinct panels (e.g. sidebar, toolbar, downloads table, status bar).
    - `ui/dialogs/<name>.slint` for distinct modal dialogs and overlays.
  - **Slint Layout Alignment Invariants (Prevent Horizontal Bunching & Offset Icons)**:
    - In Slint, setting `alignment: center` on a `HorizontalLayout` centers all children horizontally along the primary axis and **disables horizontal stretching**! Never put `alignment: center` on a `HorizontalLayout` if children should flow left-to-right across the row.
    - To vertically align elements inside a `HorizontalLayout`, explicitly set `vertical-alignment: center;` on `Text` and `y: (parent.height - self.height) / 2;` on icons, checkboxes, and buttons.
    - Keep repeated list row prefixes (e.g. Checkbox at `x = 10px`, Icon at `x = 36px`) strictly anchored to the left with uniform spacing.
  - Slint layouts default to top-alignment; specify explicit centering where necessary.
  - Use custom `Field` and `SearchField` from `ui/components/inputs.slint` instead of standard `LineEdit`.

## 4. Continuous Skill & Knowledge Evolution
- **Proactive Skill Utilization**: Always check and proactively apply relevant skills (e.g. `vdm-workflow`, `tavily`, `firecrawl`, `supermemory`, `morph-fast-apply`, `context7`, etc.) to enhance performance, search accuracy, memory retrieval, and code modification speed.
- **Autonomous Knowledge Updates**: Whenever a non-obvious bug is solved, a new Slint/Rust layout idiom is established, or an architectural component evolves, automatically update `.agents/skills/vdm-workflow/SKILL.md` and `PROJECT.md` with the new findings so all future sessions retain the knowledge.
- **Autonomous Skill Refactoring & Freshness**: Whenever an API, crate version, Slint syntax, tool argument, or upstream workflow changes rendering existing skill instructions outdated or inaccurate, immediately update the relevant `SKILL.md` file to reflect the modern, verified procedure. Never leave obsolete code snippets or outdated models in skills.
- **Modular Deep-Dive Documentation (`docs/*.md`)**:
  - For domain-specific subsystems, intricate protocols, or complex runbooks that are too detailed for `PROJECT.md` (e.g. DASH audio/video muxing, Win32 shell icon extraction, loopback security policies, Slint custom widgets), autonomously create focused markdown files under `docs/<topic>.md`.
  - Always register and cross-link every new document in [PROJECT.md](file:///g:/AI/VDM/PROJECT.md) for instant discovery.