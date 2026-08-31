<div align="center">

<img src="ui/icons/vdm.svg" width="88" height="88" alt="VDM Logo" />

# Visk Download Manager (VDM)

**A high-performance, native multi-threaded download manager for Windows.**
Built entirely in **Rust** and **Slint UI** — lightning fast, minimal memory footprint, zero webview or Electron bloat.

<br/>

[![Release](https://img.shields.io/badge/Release-v0.5.0-0A84FF?style=flat-square)](../../releases/latest)
[![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D4?style=flat-square&logo=windows11&logoColor=white)](../../releases/latest)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-DEA584?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![UI](https://img.shields.io/badge/UI-Slint%201.12-8E5CE6?style=flat-square)](https://slint.dev)
[![License](https://img.shields.io/badge/License-MIT-30D158?style=flat-square)](LICENSE)

<br/>

<img src="docs/img/app-main.png?raw=true&v=0.4.0" width="860" alt="VDM Main Interface" />

</div>

## Highlights

<table>
  <tr>
    <td width="50%" valign="top">
      <h3><img src="docs/icons/bolt.svg" width="20" height="20" align="absmiddle"/> Multi-Threaded Acceleration</h3>
      <p>Up to 32 parallel connections per download via adaptive HTTP Range work-stealing and sparse pre-allocation.</p>
    </td>
    <td width="50%" valign="top">
      <h3><img src="docs/icons/shield.svg" width="20" height="20" align="absmiddle"/> Crash-Resilient &amp; Resumable</h3>
      <p>Byte-level resumption backed by SQLite WAL storage — survives network drops, reboots, and unexpected restarts.</p>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <h3><img src="docs/icons/globe.svg" width="20" height="20" align="absmiddle"/> Browser Integration</h3>
      <p>One-click integration with Chrome, Edge, and Brave. Web downloads are intercepted and routed straight into VDM.</p>
    </td>
    <td width="50%" valign="top">
      <h3><img src="docs/icons/tray.svg" width="20" height="20" align="absmiddle"/> System Tray &amp; Startup</h3>
      <p>Lives quietly in your system tray with two-way sync with Windows Task Manager and Taskbar Startup Apps.</p>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <h3><img src="docs/icons/folder.svg" width="20" height="20" align="absmiddle"/> Smart Category Routing</h3>
      <p>Intelligently routes files into Video, Music, Documents, Programs, and Compressed folders — fully customizable.</p>
    </td>
    <td width="50%" valign="top">
      <h3><img src="docs/icons/play.svg" width="20" height="20" align="absmiddle"/> Media Stream Extraction</h3>
      <p>Captures media streams and video links, delegating to yt-dlp and ffmpeg for audio/video muxing.</p>
    </td>
  </tr>
</table>

## Interface

<div align="center">

<table>
  <tr>
    <td align="center" width="33%"><img src="docs/img/app-downloading.png?raw=true&v=0.4.0" width="290" alt="Active Download" /><br/><sub><b>Active Download</b></sub></td>
    <td align="center" width="33%"><img src="docs/img/app-complete.png?raw=true&v=0.4.0" width="290" alt="Download Complete" /><br/><sub><b>Download Complete</b></sub></td>
    <td align="center" width="33%"><img src="docs/img/app-conflict.png?raw=true&v=0.4.0" width="290" alt="File Conflict Resolver" /><br/><sub><b>Conflict Resolver</b></sub></td>
  </tr>
</table>

</div>

## Installation

<table>
  <tr>
    <td width="50%" valign="top">
      <h3><img src="docs/icons/package.svg" width="20" height="20" align="absmiddle"/> Installer</h3>
      <p>Grab <a href="../../releases/latest"><b>VDM-Setup-0.4.0.exe</b></a> from the Releases page. Includes Start Menu shortcuts, desktop icon, and uninstaller.</p>
    </td>
    <td width="50%" valign="top">
      <h3><img src="docs/icons/layers.svg" width="20" height="20" align="absmiddle"/> Build From Source</h3>
      <p>Requires the <a href="https://rustup.rs"><b>Rust toolchain</b></a> (1.75+).</p>
      <pre>git clone https://github.com/bro4cvf-hash/VDM.git
cd VDM
cargo run --release</pre>
    </td>
  </tr>
</table>

## Browser Extension

<table>
  <tr>
    <td width="60%" valign="top">
      <h3><img src="docs/icons/plug.svg" width="20" height="20" align="absmiddle"/> One-Time Setup</h3>
      <p><b>1.</b> In VDM, open <b>Settings → Browser Integration</b> and click <b>Install Extension</b> to reveal the companion folder.</p>
      <p><b>2.</b> Open <code>chrome://extensions</code> or <code>edge://extensions</code> and enable <b>Developer mode</b>.</p>
      <p><b>3.</b> Click <b>Load unpacked</b> and select the <code>extension</code> directory.</p>
      <p><b>4.</b> Browser downloads now stream directly into VDM.</p>
    </td>
    <td valign="top"><img src="docs/img/app-extension.png?raw=true&v=0.4.0" width="330" alt="Browser Integration" /></td>
  </tr>
</table>

## Architecture

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

## License

This project is licensed under the [MIT License](LICENSE) — © 2026 VDM Contributors.
