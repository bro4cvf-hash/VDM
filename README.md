<div align="center">

<img src="ui/icons/vdm.svg" width="88" height="88" alt="VDM Logo" />

# Visk Download Manager (VDM)

**A high-performance, native multi-threaded download manager for Windows.**  
Built entirely in **Rust** and **Slint UI** — lightning fast, minimal memory footprint, zero webview or Electron bloat.

<br/>

[![Release](https://img.shields.io/badge/Release-v0.3.0-0A84FF?style=flat-square)](../../releases/latest)
[![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D4?style=flat-square&logo=windows11&logoColor=white)](../../releases/latest)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-DEA584?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![UI](https://img.shields.io/badge/UI-Slint%201.12-8E5CE6?style=flat-square)](https://slint.dev)
[![License](https://img.shields.io/badge/License-MIT-30D158?style=flat-square)](LICENSE)

<br/>
<br/>

<img src="docs/img/app-main.png?raw=true&v=0.3.0" width="860" alt="VDM Main Interface" />

<br/>
<br/>

</div>

---

## ⚡ Highlights

<table>
  <tr>
    <td width="50%" valign="top">
      <h4>🚀 Multi-Threaded Acceleration</h4>
      <p>Up to 32 parallel connections per download using adaptive HTTP Range work-stealing and sparse pre-allocation to maximize network throughput.</p>
    </td>
    <td width="50%" valign="top">
      <h4>🛡️ Crash-Resilient & Resumable</h4>
      <p>Automatic byte-level resumption. Chunk progress is continually persisted to SQLite WAL storage, surviving network drops, system reboots, or unexpected restarts.</p>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <h4>🌐 Browser Integration</h4>
      <p>Seamless one-click integration with Google Chrome, Microsoft Edge, and Brave. Intercepts web downloads automatically and routes them straight into VDM.</p>
    </td>
    <td width="50%" valign="top">
      <h4>🪟 Windows Startup & System Tray</h4>
      <p>Lives unobtrusively in your system tray. Features full two-way synchronization with Windows Task Manager and Taskbar Startup Apps for silent background startup.</p>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <h4>📂 Smart Category Routing</h4>
      <p>Intelligently categorizes files into Video, Music, Documents, Programs, and Compressed archives with customizable destination folders.</p>
    </td>
    <td width="50%" valign="top">
      <h4>🎬 Media Stream Extraction</h4>
      <p>Built-in support for capturing media streams and video links with seamless delegation to yt-dlp and ffmpeg for audio/video muxing.</p>
    </td>
  </tr>
</table>

<br/>

---

## 🖼️ Interface Showcase

<div align="center">

| Download Info | File Conflict Resolver | Settings & System Startup |
|:---:|:---:|:---:|
| <img src="docs/img/app-info-dialog.png?raw=true&v=0.3.0" width="280" alt="Download Info Dialog" /> | <img src="docs/img/app-conflict.png?raw=true&v=0.3.0" width="280" alt="Conflict Dialog" /> | <img src="docs/img/app-settings.png?raw=true&v=0.3.0" width="280" alt="Settings Dialog" /> |

</div>

<br/>

---

## 📦 Installation

### Option 1: Official Installer (Recommended)

Download the latest setup package from the [**Releases**](../../releases/latest) page:

- **`VDM-Setup-0.3.0.exe`** — Installs VDM with Start Menu shortcuts, desktop icon, and uninstaller.

### Option 2: Build From Source

```powershell
# Clone the repository
git clone https://github.com/bro4cvf-hash/VDM.git
cd VDM

# Compile and launch in release mode
cargo run --release
```

> **Requirements**: [Rust toolchain](https://rustup.rs) (1.75+).

<br/>

---

## 🔌 Browser Extension Setup

1. In VDM, open **Settings → Browser Integration** and click **Install Extension** to reveal the companion extension folder.
2. Open your browser's extension manager (`chrome://extensions` or `edge://extensions`) and turn on **Developer mode**.
3. Click **Load unpacked** and select the unpacked `extension` directory.
4. Browser downloads will now automatically stream directly into VDM.

<br/>

---

## 🏗️ Technical Architecture

```
VDM/
├── src/
│   ├── main.rs             # Application lifecycle, system tray, Slint event loop & poller
│   ├── engine/
│   │   ├── downloader.rs   # Task lifecycle, speed measurement & snapshot computation
│   │   ├── worker.rs       # HTTP Range chunk workers & work-stealing engine
│   │   ├── probe.rs        # URL probe headers & format detection
│   │   ├── rate_limiter.rs # Token-bucket bandwidth throttling
│   │   ├── startup.rs      # Windows HKCU & StartupApproved registry synchronization
│   │   ├── server.rs       # Loopback IPC server (127.0.0.1:9191) for single-instance & extension
│   │   └── browser.rs      # Browser detection & native manifest registration
│   └── storage/
│       └── database.rs     # SQLite WAL persistence for tasks, chunks & preferences
└── ui/                     # Modular Slint UI definitions (Apple HIG dark theme)
```

**Core Stack:** `Rust 2021` · `Slint UI` · `Tokio` · `Reqwest (rustls)` · `SQLite 3` · `Inno Setup`

<br/>

---

## 📄 License

This project is licensed under the [MIT License](LICENSE) — © 2026 VDM Contributors.
