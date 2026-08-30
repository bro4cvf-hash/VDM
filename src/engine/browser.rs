use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct BrowserInfo {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub running: bool,
    pub exe_path: Option<PathBuf>,
    pub extension_page: String,
    pub download_url: String,
}

pub struct BrowserDetector;

const MANIFEST_JSON: &str = include_str!("../../extension/manifest.json");
const BACKGROUND_JS: &str = include_str!("../../extension/background.js");
const CONTENT_JS: &str = include_str!("../../extension/content.js");
const YT_BRIDGE_JS: &str = include_str!("../../extension/yt_bridge.js");
const POPUP_HTML: &str = include_str!("../../extension/popup.html");
const POPUP_JS: &str = include_str!("../../extension/popup.js");
const LOGO_SVG: &str = include_str!("../../ui/icons/vdm.svg");

fn render_icon(px: u32) -> Option<Vec<u8>> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(LOGO_SVG, &opt).ok()?;
    let s = px as f32 / tree.size().width();
    let mut pm = resvg::tiny_skia::Pixmap::new(px, px)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    pm.encode_png().ok()
}

impl BrowserDetector {
    pub fn get_browsers() -> Vec<BrowserInfo> {
        let running_names = Self::get_running_process_names();

        let mut list = Vec::new();

        // 1. Google Chrome
        let chrome_paths = [
            r"%ProgramFiles%\Google\Chrome\Application\chrome.exe",
            r"%ProgramFiles(x86)%\Google\Chrome\Application\chrome.exe",
            r"%LocalAppData%\Google\Chrome\Application\chrome.exe",
        ];
        let chrome_exe = Self::find_browser_exe(&chrome_paths, "chrome.exe");
        let chrome_running = running_names.iter().any(|n| n.eq_ignore_ascii_case("chrome.exe"));
        list.push(BrowserInfo {
            id: "chrome".into(),
            name: "Google Chrome".into(),
            installed: chrome_exe.is_some(),
            running: chrome_running,
            exe_path: chrome_exe,
            extension_page: "chrome://extensions/".into(),
            download_url: "https://www.google.com/chrome/".into(),
        });

        // 2. Microsoft Edge
        let edge_paths = [
            r"%ProgramFiles(x86)%\Microsoft\Edge\Application\msedge.exe",
            r"%ProgramFiles%\Microsoft\Edge\Application\msedge.exe",
            r"%LocalAppData%\Microsoft\Edge\Application\msedge.exe",
            r"%ProgramFiles(x86)%\Microsoft\EdgeCore\msedge.exe",
        ];
        let edge_exe = Self::find_browser_exe(&edge_paths, "msedge.exe");
        let edge_running = running_names.iter().any(|n| n.eq_ignore_ascii_case("msedge.exe"));
        list.push(BrowserInfo {
            id: "edge".into(),
            name: "Microsoft Edge".into(),
            installed: edge_exe.is_some(),
            running: edge_running,
            exe_path: edge_exe,
            extension_page: "edge://extensions/".into(),
            download_url: "https://www.microsoft.com/edge".into(),
        });

        // 3. Brave Browser
        let brave_paths = [
            r"%ProgramFiles%\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"%ProgramFiles(x86)%\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"%LocalAppData%\BraveSoftware\Brave-Browser\Application\brave.exe",
        ];
        let brave_exe = Self::find_browser_exe(&brave_paths, "brave.exe");
        let brave_running = running_names.iter().any(|n| n.eq_ignore_ascii_case("brave.exe"));
        list.push(BrowserInfo {
            id: "brave".into(),
            name: "Brave Browser".into(),
            installed: brave_exe.is_some(),
            running: brave_running,
            exe_path: brave_exe,
            extension_page: "brave://extensions/".into(),
            download_url: "https://brave.com/download/".into(),
        });

        // 4. Mozilla Firefox
        let firefox_paths = [
            r"%ProgramFiles%\Mozilla Firefox\firefox.exe",
            r"%ProgramFiles(x86)%\Mozilla Firefox\firefox.exe",
            r"%LocalAppData%\Mozilla Firefox\firefox.exe",
        ];
        let firefox_exe = Self::find_browser_exe(&firefox_paths, "firefox.exe");
        let firefox_running = running_names.iter().any(|n| n.eq_ignore_ascii_case("firefox.exe"));
        list.push(BrowserInfo {
            id: "firefox".into(),
            name: "Mozilla Firefox".into(),
            installed: firefox_exe.is_some(),
            running: firefox_running,
            exe_path: firefox_exe,
            extension_page: "about:addons".into(),
            download_url: "https://www.mozilla.org/firefox/".into(),
        });

        // 5. Opera
        let opera_paths = [
            r"%LocalAppData%\Programs\Opera\launcher.exe",
            r"%ProgramFiles%\Opera\launcher.exe",
        ];
        let opera_exe = Self::find_browser_exe(&opera_paths, "opera.exe");
        let opera_running = running_names.iter().any(|n| n.eq_ignore_ascii_case("opera.exe"));
        list.push(BrowserInfo {
            id: "opera".into(),
            name: "Opera".into(),
            installed: opera_exe.is_some(),
            running: opera_running,
            exe_path: opera_exe,
            extension_page: "opera://extensions".into(),
            download_url: "https://www.opera.com/download".into(),
        });

        list
    }

    fn find_browser_exe(candidates: &[&str], _exe_name: &str) -> Option<PathBuf> {
        for p in candidates {
            let expanded = Self::expand_env(p);
            if expanded.exists() {
                return Some(expanded);
            }
        }
        None
    }

    fn expand_env(path_with_env: &str) -> PathBuf {
        let mut result = path_with_env.to_string();
        if let Ok(pf) = std::env::var("ProgramFiles") {
            result = result.replace("%ProgramFiles%", &pf);
        }
        if let Ok(pfx86) = std::env::var("ProgramFiles(x86)") {
            result = result.replace("%ProgramFiles(x86)%", &pfx86);
        }
        if let Ok(local) = std::env::var("LocalAppData") {
            result = result.replace("%LocalAppData%", &local);
        }
        PathBuf::from(result)
    }

    #[cfg(windows)]
    fn get_running_process_names() -> Vec<String> {
        use std::mem::zeroed;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        };

        let mut names = Vec::new();
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
                return names;
            }

            let mut entry: PROCESSENTRY32W = zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                    let exe_name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                    names.push(exe_name);

                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snapshot);
        }
        names
    }

    #[cfg(not(windows))]
    fn get_running_process_names() -> Vec<String> {
        Vec::new()
    }

    pub fn ensure_extension_files() -> PathBuf {
        let base_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("VDM")
            .join("extension");

        let _ = std::fs::create_dir_all(&base_dir);
        let icons_dir = base_dir.join("icons");
        let _ = std::fs::create_dir_all(&icons_dir);

        let _ = std::fs::write(base_dir.join("manifest.json"), MANIFEST_JSON);
        let _ = std::fs::write(base_dir.join("background.js"), BACKGROUND_JS);
        let _ = std::fs::write(base_dir.join("content.js"), CONTENT_JS);
        let _ = std::fs::write(base_dir.join("yt_bridge.js"), YT_BRIDGE_JS);
        let _ = std::fs::write(base_dir.join("popup.html"), POPUP_HTML);
        let _ = std::fs::write(base_dir.join("popup.js"), POPUP_JS);

        for sz in [16, 48, 128] {
            let icon_path = icons_dir.join(format!("icon{sz}.png"));
            if !icon_path.exists() {
                if let Some(png) = render_icon(sz) {
                    let _ = std::fs::write(&icon_path, png);
                }
            }
        }

        // Also ensure project root ./extension folder is synced if running in workspace
        let local_ext = PathBuf::from("extension");
        if local_ext.exists() {
            let _ = std::fs::write(local_ext.join("manifest.json"), MANIFEST_JSON);
            let _ = std::fs::write(local_ext.join("background.js"), BACKGROUND_JS);
            let _ = std::fs::write(local_ext.join("content.js"), CONTENT_JS);
            let _ = std::fs::write(local_ext.join("yt_bridge.js"), YT_BRIDGE_JS);
            let _ = std::fs::write(local_ext.join("popup.html"), POPUP_HTML);
            let _ = std::fs::write(local_ext.join("popup.js"), POPUP_JS);
            let local_icons = local_ext.join("icons");
            let _ = std::fs::create_dir_all(&local_icons);
            for sz in [16, 48, 128] {
                let icon_path = local_icons.join(format!("icon{sz}.png"));
                if !icon_path.exists() {
                    if let Some(png) = render_icon(sz) {
                        let _ = std::fs::write(&icon_path, png);
                    }
                }
            }
        }

        base_dir
    }

    pub fn get_extension_dir() -> PathBuf {
        Self::ensure_extension_files()
    }

    pub fn install_extension_for(browser_id: &str) {
        let ext_dir = Self::get_extension_dir();
        let ext_str = ext_dir.to_string_lossy().to_string();

        // 1. Copy full valid extension path to Windows clipboard
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(ext_str.clone());
        }

        // 2. Open Windows Explorer highlighting the extension folder
        if ext_dir.exists() {
            let _ = Command::new("explorer").arg(&ext_dir).spawn();
        }

        // 3. Open the target browser's extension page
        let browsers = Self::get_browsers();
        if let Some(b) = browsers.iter().find(|b| b.id == browser_id) {
            if let Some(ref exe) = b.exe_path {
                let _ = Command::new(exe).arg(&b.extension_page).spawn();
                return;
            }
        }

        let _ = Command::new("cmd").args(["/C", "start", "chrome://extensions"]).spawn();
    }

    pub fn open_download_page(browser_id: &str) {
        let browsers = Self::get_browsers();
        if let Some(b) = browsers.iter().find(|b| b.id == browser_id) {
            let _ = Command::new("cmd").args(["/C", "start", &b.download_url]).spawn();
        }
    }
}
