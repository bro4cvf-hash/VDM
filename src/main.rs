#![windows_subsystem = "windows"]

use anyhow::Context;
use i_slint_backend_winit::WinitWindowAccessor;
use rfd::FileDialog;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod engine;
mod storage;

use engine::{Manager, TaskSnapshot};
use storage::database::Db;

slint::include_modules!();

// armed from the CompleteDialog checkbox: shut down the PC when the queue drains
static SHUTDOWN_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const LOGO_SVG: &str = include_str!("../ui/icons/vdm.svg");

// rasterize the brand SVG to straight-alpha RGBA (window + tray icons)
fn logo_rgba(px: u32) -> Option<Vec<u8>> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(LOGO_SVG, &opt).ok()?;
    let s = px as f32 / tree.size().width();
    let mut pm = resvg::tiny_skia::Pixmap::new(px, px)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    Some(
        pm.pixels()
            .iter()
            .flat_map(|c| {
                let d = c.demultiply();
                [d.red(), d.green(), d.blue(), d.alpha()]
            })
            .collect(),
    )
}

/// Trims physical working set memory, returning unused RAM pages to Windows memory manager
#[cfg(target_os = "windows")]
pub fn trim_working_set() {
    unsafe {
        use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}

#[cfg(not(target_os = "windows"))]
pub fn trim_working_set() {}

#[derive(Clone, Debug)]
struct CompletedTaskInfo {
    id: String,
    filename: String,
    total: u64,
    location: String,
    url: String,
}

fn show_next_completed_dialog(
    queue: &Arc<Mutex<VecDeque<CompletedTaskInfo>>>,
    dialog: &CompleteDialog,
    main_window_opt: Option<&slint::Window>,
    is_showing: &Arc<AtomicBool>,
) {
    let next = {
        let mut q = queue.lock().unwrap();
        q.pop_front()
    };
    if let Some(item) = next {
        is_showing.store(true, Ordering::Relaxed);
        dialog.set_task_id(item.id.into());
        dialog.set_filename(item.filename.clone().into());
        dialog.set_size(if item.total > 0 { fmt_size(item.total) } else { "Unknown size".into() }.into());
        dialog.set_total_bytes_text(format!("{} Bytes", item.total).into());
        dialog.set_location(item.location.into());
        dialog.set_url(item.url.into());
        if let Some(img) = engine::sys_icon::get_file_icon_image(&item.filename) {
            dialog.set_file_icon(img);
            dialog.set_has_sys_icon(true);
        } else {
            dialog.set_has_sys_icon(false);
        }
        let _ = dialog.show();
        center_dialog(main_window_opt, dialog.window(), 490.0, 315.0);
        dialog.window().with_winit_window(|win| {
            win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
            win.set_visible(true);
            win.focus_window();
            win.request_user_attention(Some(i_slint_backend_winit::winit::window::UserAttentionType::Critical));
        });
    } else {
        is_showing.store(false, Ordering::Relaxed);
    }
}

// center a dialog over the main window (IDM-style) or monitor center if main is hidden
fn center_over_main(parent: &slint::Window, win: &slint::Window, w: f64, h: f64) {
    parent.with_winit_window(|pw| {
        let scale = pw.scale_factor();
        let pos = pw.outer_position().unwrap_or_default();
        let size = pw.inner_size();
        let x = pos.x as f64 + (size.width as f64 - w * scale) / 2.0;
        let y = pos.y as f64 + (size.height as f64 - h * scale) / 2.0;
        let _ = win.set_position(slint::WindowPosition::Physical(slint::PhysicalPosition::new(
            x as i32,
            y as i32,
        )));
    });
}

fn center_dialog(parent_opt: Option<&slint::Window>, win: &slint::Window, w: f64, h: f64) {
    let mut parent_usable = false;
    if let Some(parent) = parent_opt {
        parent.with_winit_window(|pw| {
            if pw.is_visible().unwrap_or(false) && !pw.is_minimized().unwrap_or(false) {
                let pos = pw.outer_position().unwrap_or_default();
                if pos.x > -10000 && pos.y > -10000 {
                    parent_usable = true;
                }
            }
        });
    }

    if parent_usable {
        if let Some(parent) = parent_opt {
            center_over_main(parent, win, w, h);
            return;
        }
    }

    win.with_winit_window(|winit_win| {
        if let Some(monitor) = winit_win.current_monitor().or_else(|| winit_win.primary_monitor()) {
            let scale = monitor.scale_factor();
            let mon_size = monitor.size();
            let mon_pos = monitor.position();
            let x = mon_pos.x as f64 + (mon_size.width as f64 - w * scale) / 2.0;
            let y = mon_pos.y as f64 + (mon_size.height as f64 - h * scale) / 2.0;
            let _ = winit_win.set_outer_position(i_slint_backend_winit::winit::dpi::PhysicalPosition::new(
                x as i32,
                y as i32,
            ));
        }
    });
}

fn open_renew_dialog(
    renew_dialog: &RenewLinkDialog,
    parent_window: Option<&slint::Window>,
    active_renewing_task: &Arc<Mutex<Option<String>>>,
    task_id: &str,
    filename: &str,
    url: &str,
    referrer: &str,
    error_msg: &str,
    downloaded_text: &str,
    progress: f32,
) {
    renew_dialog.set_task_id(task_id.into());
    renew_dialog.set_filename(filename.into());
    renew_dialog.set_url(url.into());
    renew_dialog.set_new_url(url.into());
    renew_dialog.set_referrer(referrer.into());
    renew_dialog.set_error_msg(error_msg.into());
    renew_dialog.set_downloaded_text(downloaded_text.into());
    renew_dialog.set_progress(progress.clamp(0.0, 1.0));
    renew_dialog.set_is_listening(true);

    *active_renewing_task.lock().unwrap() = Some(task_id.to_string());

    let _ = renew_dialog.show();
    let h = if error_msg.is_empty() { 310.0 } else { 352.0 };
    center_dialog(parent_window, renew_dialog.window(), 590.0, h);

    renew_dialog.window().with_winit_window(|win| {
        win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
        win.set_visible(true);
        win.focus_window();
        win.request_user_attention(Some(i_slint_backend_winit::winit::window::UserAttentionType::Critical));
    });
}

fn start_shutdown_countdown(
    shutdown_dialog: &ShutdownDialog,
    main_window_opt: Option<&slint::Window>,
    countdown_active: &Arc<AtomicBool>,
    countdown_seconds: &Arc<AtomicI32>,
    weak_dialog: slint::Weak<ShutdownDialog>,
) {
    if countdown_active.swap(true, Ordering::SeqCst) {
        return; // already counting down
    }
    countdown_seconds.store(30, Ordering::Relaxed);
    shutdown_dialog.set_seconds_remaining(30);
    shutdown_dialog.set_progress(1.0);
    shutdown_dialog.set_message("All downloads have finished successfully.".into());
    let _ = shutdown_dialog.show();
    center_dialog(main_window_opt, shutdown_dialog.window(), 480.0, 235.0);
    shutdown_dialog.window().with_winit_window(|win| {
        win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
        win.set_visible(true);
        win.focus_window();
        win.request_user_attention(Some(i_slint_backend_winit::winit::window::UserAttentionType::Critical));
    });

    // 35s OS grace window so Windows knows shutdown is scheduled but our in-app dialog can abort it
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("shutdown").args(["/s", "/t", "35"]).spawn();
    }

    let active_timer = countdown_active.clone();
    let sec_timer = countdown_seconds.clone();
    let weak_d = weak_dialog;

    std::thread::spawn(move || {
        for _ in 0..30 {
            std::thread::sleep(Duration::from_secs(1));
            if !active_timer.load(Ordering::Relaxed) {
                return;
            }
            let remaining = sec_timer.fetch_sub(1, Ordering::Relaxed) - 1;
            let wd = weak_d.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(d) = wd.upgrade() {
                    d.set_seconds_remaining(remaining.max(0));
                    d.set_progress((remaining.max(0) as f32 / 30.0).clamp(0.0, 1.0));
                }
            });
            if remaining <= 0 {
                break;
            }
        }

        if active_timer.swap(false, Ordering::SeqCst) {
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("shutdown")
                    .args(["/s", "/t", "0"])
                    .spawn();
            }
        }
    });
}

fn abort_shutdown_countdown(
    shutdown_dialog_weak: &slint::Weak<ShutdownDialog>,
    countdown_active: &Arc<AtomicBool>,
) {
    countdown_active.store(false, Ordering::SeqCst);
    SHUTDOWN_ARMED.store(false, Ordering::Relaxed);
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("shutdown")
            .args(["/a"])
            .spawn();
    }
    if let Some(d) = shutdown_dialog_weak.upgrade() {
        let _ = d.hide();
    }
}

fn get_category_folder(cat: &str) -> String {
    let user_profile = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Default".into());
    let downloads = format!("{user_profile}\\Downloads");
    match cat {
        "Compressed" | "compressed" => format!("{downloads}\\Compressed"),
        "Programs" | "installer" => format!("{downloads}\\Programs"),
        "Video" | "video" => format!("{downloads}\\Video"),
        "Music" | "audio" => format!("{downloads}\\Music"),
        "Documents" | "document" => format!("{downloads}\\Documents"),
        "Pictures" | "image" => format!("{downloads}\\Pictures"),
        _ => format!("{downloads}\\Documents"),
    }
}

fn detect_category_name(filename: &str) -> String {
    match detect_category(filename).as_str() {
        "compressed" => "Compressed".into(),
        "installer" => "Programs".into(),
        "video" => "Video".into(),
        "audio" => "Music".into(),
        "document" => "Documents".into(),
        _ => "Documents".into(),
    }
}

// ---------- formatting & classification helpers ----------
fn fmt_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn fmt_speed(bps: u64) -> String {
    if bps == 0 {
        return "0 KB/s".into();
    }
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bps as f64;
    if b >= MB {
        format!("{:.1} MB/s", b / MB)
    } else {
        format!("{:.0} KB/s", b / KB)
    }
}

fn fmt_eta(secs: Option<u64>) -> String {
    match secs {
        None => "—".into(),
        Some(s) if s > 3600 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        Some(s) if s > 60 => format!("{}m {}s", s / 60, s % 60),
        Some(s) => format!("{s}s"),
    }
}

fn get_clipboard_text() -> String {
    for _ in 0..4 {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(text) = cb.get_text() {
                return text;
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    String::new()
}

fn extract_url_from_clipboard() -> Option<String> {
    let raw = get_clipboard_text();
    if raw.is_empty() {
        return None;
    }
    for line in raw.lines() {
        let trimmed = line.trim().trim_matches(|c: char| {
            c == '"' || c == '\'' || c == '<' || c == '>' || c == '(' || c == ')' || c == '[' || c == ']'
        }).trim();

        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("ftp://")
            || trimmed.starts_with("sftp://")
            || trimmed.starts_with("magnet:?")
        {
            return Some(trimmed.to_string());
        }
        if trimmed.starts_with("www.") && trimmed.contains('.') {
            return Some(format!("https://{}", trimmed));
        }
    }
    None
}

fn detect_file_type(filename: &str) -> String {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext.is_empty() {
        return "document".into();
    }
    match ext.as_str() {
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "dmg" | "pkg" | "tgz" => "archive".into(),
        "exe" | "msi" | "apk" | "deb" | "rpm" | "appimage" | "bat" | "cmd" | "ps1" => "installer".into(),
        "mp4" | "mkv" | "mov" | "avi" | "wmv" | "flv" | "webm" | "m4v" | "m2ts" => "video".into(),
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" => "audio".into(),
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "epub" | "md" => "document".into(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tiff" => "image".into(),
        "rs" | "js" | "ts" | "py" | "c" | "cpp" | "h" | "html" | "css" | "json" | "xml" | "toml" | "yaml" => "code".into(),
        _ => "document".into(),
    }
}

fn detect_category(filename: &str) -> String {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext.is_empty() {
        return "document".into();
    }
    match ext.as_str() {
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "dmg" | "pkg" | "tgz" => "compressed".into(),
        "exe" | "msi" | "apk" | "deb" | "rpm" | "appimage" | "bat" | "cmd" | "ps1" => "installer".into(),
        "mp4" | "mkv" | "mov" | "avi" | "wmv" | "flv" | "webm" | "m4v" | "m2ts" => "video".into(),
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" => "audio".into(),
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "epub" | "md" => "document".into(),
        _ => "document".into(),
    }
}

fn format_timestamp(ts_ms: i64) -> String {
    if ts_ms <= 0 {
        return "—".to_string();
    }
    let ts = ts_ms / 1000; // created_at is milliseconds; calendar math below is seconds-based
    let mins_total = ts / 60;
    let mins = mins_total % 60;
    let hours = (mins_total / 60) % 24;
    let days = ts / 86400; // days since 1970-01-01

    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };

    let month_str = match m {
        1 => "Jan", 2 => "Feb", 3 => "Mar", 4 => "Apr", 5 => "May", 6 => "Jun",
        7 => "Jul", 8 => "Aug", 9 => "Sep", 10 => "Oct", 11 => "Nov", 12 => "Dec",
        _ => "Jan",
    };
    format!("{} {:02}, {:02}:{:02}", month_str, d, hours, mins)
}

// field-by-field compare so we can skip set_row_data for unchanged rows
fn item_eq(a: &DownloadItem, b: &DownloadItem) -> bool {
    a.id == b.id
        && a.filename == b.filename
        && a.url == b.url
        && a.referrer == b.referrer
        && a.status == b.status
        && a.progress == b.progress
        && a.progress_text == b.progress_text
        && a.size == b.size
        && a.speed == b.speed
        && a.eta == b.eta
        && a.date_text == b.date_text
        && a.file_type == b.file_type
        && a.category == b.category
        && a.selected == b.selected
        && a.error_msg == b.error_msg
}

fn chunk_eq(a: &ChunkInfo, b: &ChunkInfo) -> bool {
    a.id == b.id && a.downloaded == b.downloaded && a.status == b.status
}

// swap in a fresh model only when row count/order changed; otherwise update
// just the changed rows (full model swap makes Slint rebuild every delegate)
fn apply_items(app: &AppWindow, items: Vec<DownloadItem>) {
    let model = app.get_downloads();
    let vm = model.as_any().downcast_ref::<VecModel<DownloadItem>>();
    let same_shape = vm
        .map(|m| {
            m.row_count() == items.len()
                && (0..items.len()).all(|i| m.row_data(i).map(|r| r.id == items[i].id).unwrap_or(false))
        })
        .unwrap_or(false);
    if let (true, Some(m)) = (same_shape, vm) {
        for (i, new) in items.into_iter().enumerate() {
            if m.row_data(i).map(|old| !item_eq(&old, &new)).unwrap_or(true) {
                m.set_row_data(i, new);
            }
        }
    } else {
        app.set_downloads(ModelRc::new(VecModel::from(items)));
    }
}

fn snapshot_to_item(s: &TaskSnapshot, is_selected: bool) -> DownloadItem {
    let has_total = s.total.map(|t| t > 0).unwrap_or(false);
    let progress = if s.status == "completed" {
        100.0
    } else if let Some(total) = s.total {
        if total == 0 { 0.0 } else { (s.downloaded as f32 / total as f32 * 100.0).clamp(0.0, 100.0) }
    } else {
        0.0
    };
    let progress_text = if s.status == "completed" {
        "100%".into()
    } else if has_total {
        format!("{}%", (progress as u32).min(100))
    } else if s.status == "downloading" || s.status == "connecting" {
        "—".into()
    } else {
        "—".into()
    };
    let size = if s.status == "completed" {
        if let Some(total) = s.total {
            fmt_size(total)
        } else {
            fmt_size(s.downloaded)
        }
    } else if let Some(total) = s.total {
        format!("{} / {}", fmt_size(s.downloaded), fmt_size(total))
    } else {
        fmt_size(s.downloaded)
    };
    let file_type = detect_file_type(&s.filename);
    let category = detect_category(&s.filename);
    let date_text = format_timestamp(s.created_at);
    let error_msg = s.error_msg.clone().unwrap_or_default();

    DownloadItem {
        id: s.id.clone().into(),
        filename: s.filename.clone().into(),
        url: s.url.clone().into(),
        referrer: s.referrer.clone().into(),
        status: s.status.clone().into(),
        progress,
        progress_text: progress_text.into(),
        size: size.into(),
        speed: fmt_speed(s.speed_bps).into(),
        eta: fmt_eta(s.eta_secs).into(),
        date_text: date_text.into(),
        file_type: file_type.into(),
        category: category.into(),
        selected: is_selected,
        error_msg: error_msg.into(),
    }
}

fn compute_next_duplicate_name(folder: &str, filename: &str, existing_tasks: &[TaskSnapshot]) -> String {
    let path = std::path::Path::new(filename);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(filename);

    for n in 1..10000u32 {
        let candidate = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };

        let path_on_disk = std::path::PathBuf::from(folder).join(&candidate);
        let in_db = existing_tasks.iter().any(|t| t.dir.eq_ignore_ascii_case(folder) && t.filename.eq_ignore_ascii_case(&candidate));
        if !path_on_disk.exists() && !in_db {
            return candidate;
        }
    }
    format!("{stem}_{}", engine::downloader::new_id())
}

struct ProgressRegistry {
    dialogs: Arc<Mutex<HashMap<String, slint::Weak<DownloadProgressDialog>>>>,
    manager: Arc<Manager>,
    main_app: slint::Weak<AppWindow>,
    renew_dialog: slint::Weak<RenewLinkDialog>,
    active_renewing_task: Arc<Mutex<Option<String>>>,
}

impl ProgressRegistry {
    fn new(
        manager: Arc<Manager>,
        main_app: slint::Weak<AppWindow>,
        renew_dialog: slint::Weak<RenewLinkDialog>,
        active_renewing_task: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            dialogs: Arc::new(Mutex::new(HashMap::new())),
            manager,
            main_app,
            renew_dialog,
            active_renewing_task,
        }
    }

    fn open_dialog(&self, snap: &TaskSnapshot) {
        let mut map = self.dialogs.lock().unwrap();
        map.retain(|_, weak| weak.upgrade().is_some());
        if let Some(existing_weak) = map.get(&snap.id) {
            if let Some(existing) = existing_weak.upgrade() {
                let _ = existing.show();
                existing.window().with_winit_window(|win| {
                    win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
                    win.set_visible(true);
                    win.focus_window();
                });
                return;
            }
        }

        let p = match DownloadProgressDialog::new() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[VDM] Failed to create DownloadProgressDialog: {:?}", e);
                return;
            }
        };
        let theme_idx: i32 = self.manager.db.get_kv("theme").and_then(|s| s.parse().ok()).unwrap_or(0);
        p.global::<Palette>().set_active_theme(theme_idx);

        let task_id = snap.id.clone();
        p.set_task_id(task_id.clone().into());
        p.set_filename(snap.filename.clone().into());
        p.set_url(snap.url.clone().into());
        p.set_save_path(format!("{}\\{}", snap.dir, snap.filename).into());
        p.set_total_size(snap.total.map(fmt_size).unwrap_or_else(|| "—".into()).into());
        p.set_downloaded_size(fmt_size(snap.downloaded).into());
        p.set_transfer_rate("0 KB/s".into());
        p.set_time_left("Calculating...".into());
        p.set_resume_capable("Yes".into());
        let pct = snap.total.map(|t| if t > 0 { snap.downloaded as f32 / t as f32 } else { 0.0 }).unwrap_or(0.0);
        p.set_progress(pct);
        p.set_percent_text(format!("{:.1}%", pct * 100.0).into());
        p.set_is_paused(snap.status == "paused");
        p.set_is_torrent(snap.url.starts_with("magnet:?"));
        p.set_status_text(if snap.status == "downloading" { "Receiving data...".into() } else { snap.status.clone().into() });

        let p_weak = p.as_weak();
        let p_weak_min = p.as_weak();
        p.on_minimize_window(move || {
            if let Some(d) = p_weak_min.upgrade() {
                let _ = d.hide();
            }
        });

        let p_weak_drag = p.as_weak();
        p.on_drag_window(move || {
            if let Some(d) = p_weak_drag.upgrade() {
                d.window().with_winit_window(|win| {
                    let _ = win.drag_window();
                });
            }
        });

        let p_weak_close = p.as_weak();
        let map_close = self.dialogs.clone();
        let tid_close = task_id.clone();
        p.on_closed(move || {
            if let Some(d) = p_weak_close.upgrade() {
                let _ = d.hide();
            }
            let mut map = map_close.lock().unwrap();
            map.remove(&tid_close);
            let any_armed = map.values().any(|w| {
                w.upgrade().map(|d| d.get_shutdown_when_done()).unwrap_or(false)
            });
            if !any_armed {
                SHUTDOWN_ARMED.store(false, Ordering::Relaxed);
            }
        });

        let m_pause = self.manager.clone();
        let p_weak_pause = p.as_weak();
        let tid_pause = task_id.clone();
        p.on_toggle_pause(move || {
            if let Ok(snap) = m_pause.snapshot_of(&tid_pause) {
                if snap.status == "downloading" || snap.status == "connecting" {
                    let _ = m_pause.pause(&tid_pause);
                    if let Some(d) = p_weak_pause.upgrade() {
                        d.set_is_paused(true);
                        d.set_status_text("Paused".into());
                    }
                } else {
                    let _ = m_pause.resume(&tid_pause);
                    if let Some(d) = p_weak_pause.upgrade() {
                        d.set_is_paused(false);
                        d.set_status_text("Receiving data...".into());
                    }
                }
            }
        });

        let m_canc = self.manager.clone();
        let p_weak_canc = p.as_weak();
        let map_canc = self.dialogs.clone();
        let tid_canc = task_id.clone();
        p.on_cancel_download(move || {
            let _ = m_canc.pause(&tid_canc);
            if let Some(d) = p_weak_canc.upgrade() {
                let _ = d.hide();
            }
            let mut map = map_canc.lock().unwrap();
            map.remove(&tid_canc);
            let any_armed = map.values().any(|w| {
                w.upgrade().map(|d| d.get_shutdown_when_done()).unwrap_or(false)
            });
            if !any_armed {
                SHUTDOWN_ARMED.store(false, Ordering::Relaxed);
            }
        });

        let m_lim = self.manager.clone();
        p.on_limit_changed(move |mbps| {
            let bps = if mbps <= 0.01 { 0 } else { (mbps as f64 * 1024.0 * 1024.0) as u64 };
            m_lim.set_speed_limit(bps);
        });

        p.on_shutdown_changed(move |on| {
            SHUTDOWN_ARMED.store(on, std::sync::atomic::Ordering::Relaxed);
        });

        let ren_weak = self.renew_dialog.clone();
        let act_ren_tid = self.active_renewing_task.clone();
        let m_ren = self.manager.clone();
        let tid_ren = task_id.clone();
        let main_app_weak = self.main_app.clone();
        p.on_renew_link(move || {
            if let Ok(snap) = m_ren.snapshot_of(&tid_ren) {
                if let Some(ren) = ren_weak.upgrade() {
                    let dl_text = format!("{} of {}", fmt_size(snap.downloaded), snap.total.map(fmt_size).unwrap_or_else(|| "—".into()));
                    let err = snap.error_msg.unwrap_or_else(|| "Download failed".into());
                    let pct = snap.total.map(|t| if t > 0 { snap.downloaded as f32 / t as f32 } else { 0.0 }).unwrap_or(0.0);
                    let ref_url = if snap.referrer.is_empty() { snap.url.clone() } else { snap.referrer.clone() };
                    let main_app = main_app_weak.upgrade();
                    open_renew_dialog(
                        &ren,
                        main_app.as_ref().map(|a| a.window()),
                        &act_ren_tid,
                        &snap.id,
                        &snap.filename,
                        &snap.url,
                        &ref_url,
                        &err,
                        &dl_text,
                        pct,
                    );
                }
            }
        });

        let count = map.len() as i32;
        let offset_x = (count * 35) % 280;
        let offset_y = (count * 35) % 280;
        let app_opt = self.main_app.upgrade();
        center_dialog(app_opt.as_ref().map(|a| a.window()), p.window(), 520.0, 300.0);

        p.window().with_winit_window(move |win| {
            let pos = win.outer_position().unwrap_or_default();
            win.set_outer_position(i_slint_backend_winit::winit::dpi::PhysicalPosition::new(
                pos.x + offset_x,
                pos.y + offset_y,
            ));
            win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
            win.set_visible(true);
            win.focus_window();
        });

        let _ = p.show();
        map.insert(task_id, p_weak);
    }
}

fn main() -> anyhow::Result<()> {
    let is_autostart = std::env::args().any(|arg| {
        arg == "--autostart"
            || arg == "--minimized"
            || arg == "--startup"
            || arg == "--tray"
            || arg == "--background"
    });

    // ── Single-Instance Check ──
    // If VDM is already running, notify it to bring its window to front and exit.
    if let Ok(mut stream) = std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", engine::server::DEFAULT_SERVER_PORT).parse().unwrap(),
        Duration::from_millis(250),
    ) {
        if !is_autostart {
            use std::io::Write;
            let _ = stream.write_all(b"GET /show HTTP/1.1\r\nHost: 127.0.0.1:9191\r\nConnection: close\r\n\r\n");
            println!("[VDM] Active instance running on :{} — brought window to front.", engine::server::DEFAULT_SERVER_PORT);
        }
        return Ok(());
    }

    // tokio runtime that lives for the whole process; slint runs on the main thread
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    let _guard = rt.enter();

    // Ensure extension files are unpacked & synchronized
    engine::browser::BrowserDetector::ensure_extension_files();

    // ---- persistent storage ----
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("VDM");
    std::fs::create_dir_all(&data_dir).ok();
    let db_path = data_dir.join("vdm.db");
    let db = Db::open(&db_path).context("open db")?;
    let manager = Manager::new(db);
    let manager_cloned_for_poll = manager.clone();

    // ---- slint window ----
    let app = AppWindow::new().context("create window")?;
    let menu = TrayMenu::new().context("tray menu")?;
    let info = DownloadInfoDialog::new().context("info dialog")?;
    let torrent_dialog = TorrentFileSelectDialog::new().context("torrent select dialog")?;
    let renew_dialog = RenewLinkDialog::new().context("renew dialog")?;
    let duplicate_dialog = DuplicateDownloadDialog::new().context("duplicate dialog")?;
    let mini_pill = DownloadMiniPill::new().context("mini pill")?;
    let done = CompleteDialog::new().context("complete dialog")?;
    let shutdown_dialog = ShutdownDialog::new().context("shutdown dialog")?;
    let countdown_active = Arc::new(AtomicBool::new(false));
    let countdown_seconds = Arc::new(AtomicI32::new(30));
    let had_active_downloads_in_session = Arc::new(AtomicBool::new(false));
    let active_renewing_task: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let progress_registry = Arc::new(ProgressRegistry::new(
        manager.clone(),
        app.as_weak(),
        renew_dialog.as_weak(),
        active_renewing_task.clone(),
    ));

    // Load and apply initial active theme across all windows
    let theme_idx: i32 = manager.db.get_kv("theme").and_then(|s| s.parse().ok()).unwrap_or(0);
    app.global::<Palette>().set_active_theme(theme_idx);
    info.global::<Palette>().set_active_theme(theme_idx);
    torrent_dialog.global::<Palette>().set_active_theme(theme_idx);
    renew_dialog.global::<Palette>().set_active_theme(theme_idx);
    duplicate_dialog.global::<Palette>().set_active_theme(theme_idx);
    mini_pill.global::<Palette>().set_active_theme(theme_idx);
    done.global::<Palette>().set_active_theme(theme_idx);
    shutdown_dialog.global::<Palette>().set_active_theme(theme_idx);

    // window / taskbar icon from the brand SVG
    if let Some(rgba) = logo_rgba(64) {
        let icon = i_slint_backend_winit::winit::window::Icon::from_rgba(rgba, 64, 64).ok();
        app.window().with_winit_window(move |win| {
            win.set_window_icon(icon);
        });
    }

    // initial slider values from persisted settings
    let init_speed_mbps = manager.limiter.rate() as f32 / (1024.0 * 1024.0);
    let init_max_conns = manager.max_conns.load(std::sync::atomic::Ordering::Relaxed) as i32;
    let init_max_active = manager.max_active.load(std::sync::atomic::Ordering::Relaxed) as i32;

    app.set_speed_limit_mbps(init_speed_mbps);
    app.set_max_connections(init_max_conns);
    app.set_max_active(init_max_active);

    let dbl_action = manager.db.get_kv("double_click_action").unwrap_or_else(|| "open".into());
    app.set_double_click_action(dbl_action.into());

    // shared filter / search / sort state (polled by background thread)
    let filter = Arc::new(Mutex::new(String::from("All")));
    let search = Arc::new(Mutex::new(String::new()));
    let sort_col = Arc::new(Mutex::new(String::from("date")));
    let sort_asc = Arc::new(Mutex::new(false));
    let last_sig = Arc::new(Mutex::new(0u64));
    let filter_for_poll = filter.clone();
    let search_for_poll = search.clone();
    let sort_col_for_poll = sort_col.clone();
    let sort_asc_for_poll = sort_asc.clone();
    let last_sig_for_poll = last_sig.clone();

    // ── Window Controls (Frameless Drag & Traffic Lights) ──
    let weak_drag = app.as_weak();
    app.on_drag_window(move || {
        if let Some(app) = weak_drag.upgrade() {
            app.window().with_winit_window(|win| {
                // dragging a maximized window must restore it first, or the
                // move loop misbehaves and the state goes stale
                if win.is_maximized() {
                    win.set_maximized(false);
                    app.set_is_maximized(false);
                }
                let _ = win.drag_window();
            });
        }
    });


    let weak_min = app.as_weak();
    app.on_minimize_window(move || {
        if let Some(app) = weak_min.upgrade() {
            app.window().set_minimized(true);
            trim_working_set();
        }
    });

    let weak_max = app.as_weak();
    app.on_maximize_window(move || {
        if let Some(app) = weak_max.upgrade() {
            // trust the intended state — reading back is_maximized() right
            // after set_maximized() returns a stale value on Windows
            let mut now_max = false;
            app.window().with_winit_window(|win| {
                let is_max = win.is_maximized();
                win.set_maximized(!is_max);
                now_max = !is_max;
            });
            app.set_is_maximized(now_max);
        }
    });

    let weak_resize = app.as_weak();
    app.on_resize_window(move |dir| {
        if let Some(app) = weak_resize.upgrade() {
            app.window().with_winit_window(|win| {
                let d = match dir.as_str() {
                    "e" => i_slint_backend_winit::winit::window::ResizeDirection::East,
                    "w" => i_slint_backend_winit::winit::window::ResizeDirection::West,
                    "s" => i_slint_backend_winit::winit::window::ResizeDirection::South,
                    "n" => i_slint_backend_winit::winit::window::ResizeDirection::North,
                    "se" => i_slint_backend_winit::winit::window::ResizeDirection::SouthEast,
                    "sw" => i_slint_backend_winit::winit::window::ResizeDirection::SouthWest,
                    "ne" => i_slint_backend_winit::winit::window::ResizeDirection::NorthEast,
                    "nw" => i_slint_backend_winit::winit::window::ResizeDirection::NorthWest,
                    _ => return,
                };
                let _ = win.drag_resize_window(d);
            });
        }
    });

    // ── Multi-Selection State ──
    let selected_ids = Arc::new(Mutex::new(HashSet::<String>::new()));
    let anchor_index = Arc::new(AtomicI32::new(-1));

    // ── Download Operations ──
    let m = manager.clone();
    app.on_pause_download(move |id| {
        let _ = m.pause(&String::from(id));
    });

    let m = manager.clone();
    let progress_reg_res = progress_registry.clone();
    app.on_resume_download(move |id| {
        let s_id = String::from(id);
        let _ = m.resume(&s_id);
        if let Ok(snap) = m.snapshot_of(&s_id) {
            progress_reg_res.open_dialog(&snap);
        }
    });

    let m = manager.clone();
    let selected_ids_single_rem = selected_ids.clone();
    let weak_app_single_rem = app.as_weak();
    app.on_remove_download(move |id, delete_file| {
        let s_id = String::from(id);
        let _ = m.remove(&s_id, delete_file);
        let mut sel = selected_ids_single_rem.lock().unwrap();
        sel.remove(&s_id);
        let count = sel.len() as i32;
        if let Some(a) = weak_app_single_rem.upgrade() {
            a.set_selected_count(count);
            if count == 0 {
                a.set_first_selected_id("".into());
                a.set_first_selected_filename("".into());
                a.set_selected_index(-1);
            }
        }
    });

    let selected_ids_click = selected_ids.clone();
    let anchor_click = anchor_index.clone();
    let weak_app_click = app.as_weak();
    app.on_row_clicked(move |idx, shift, ctrl| {
        let Some(a) = weak_app_click.upgrade() else { return };
        let model = a.get_downloads();
        let count = model.row_count();
        if idx < 0 || idx as usize >= count { return };

        let mut sel = selected_ids_click.lock().unwrap();
        let target_id = model.row_data(idx as usize).map(|it| it.id.to_string()).unwrap_or_default();

        if shift {
            let anchor = anchor_click.load(Ordering::SeqCst);
            let start = if anchor >= 0 { anchor.min(idx) } else { 0 };
            let end = if anchor >= 0 { anchor.max(idx) } else { idx };

            if !ctrl {
                sel.clear();
            }
            for i in start..=end {
                if let Some(it) = model.row_data(i as usize) {
                    sel.insert(it.id.to_string());
                }
            }
        } else if ctrl {
            anchor_click.store(idx, Ordering::SeqCst);
            if sel.contains(&target_id) {
                sel.remove(&target_id);
            } else {
                sel.insert(target_id);
            }
        } else {
            sel.clear();
            sel.insert(target_id);
            anchor_click.store(idx, Ordering::SeqCst);
        }

        a.set_selected_index(idx);

        let mut first_id = String::new();
        let mut first_name = String::new();
        if let Some(vm) = model.as_any().downcast_ref::<VecModel<DownloadItem>>() {
            for i in 0..count {
                if let Some(mut it) = vm.row_data(i) {
                    let want = sel.contains(it.id.as_str());
                    if it.selected != want {
                        it.selected = want;
                        vm.set_row_data(i, it.clone());
                    }
                    if want && first_id.is_empty() {
                        first_id = it.id.to_string();
                        first_name = it.filename.to_string();
                    }
                }
            }
        }
        a.set_selected_count(sel.len() as i32);
        a.set_first_selected_id(first_id.into());
        a.set_first_selected_filename(first_name.into());
    });

    let selected_ids_all = selected_ids.clone();
    let weak_app_all = app.as_weak();
    app.on_select_all_items(move || {
        let Some(a) = weak_app_all.upgrade() else { return };
        let model = a.get_downloads();
        let count = model.row_count();
        let mut sel = selected_ids_all.lock().unwrap();
        sel.clear();
        let mut first_id = String::new();
        let mut first_name = String::new();
        if let Some(vm) = model.as_any().downcast_ref::<VecModel<DownloadItem>>() {
            for i in 0..count {
                if let Some(mut it) = vm.row_data(i) {
                    sel.insert(it.id.to_string());
                    if first_id.is_empty() {
                        first_id = it.id.to_string();
                        first_name = it.filename.to_string();
                    }
                    if !it.selected {
                        it.selected = true;
                        vm.set_row_data(i, it);
                    }
                }
            }
        }
        if count > 0 && a.get_selected_index() < 0 {
            a.set_selected_index(0);
        }
        a.set_selected_count(sel.len() as i32);
        a.set_first_selected_id(first_id.into());
        a.set_first_selected_filename(first_name.into());
    });

    let selected_ids_desel = selected_ids.clone();
    let weak_app_desel = app.as_weak();
    app.on_deselect_all_items(move || {
        let Some(a) = weak_app_desel.upgrade() else { return };
        let model = a.get_downloads();
        let count = model.row_count();
        selected_ids_desel.lock().unwrap().clear();
        if let Some(vm) = model.as_any().downcast_ref::<VecModel<DownloadItem>>() {
            for i in 0..count {
                if let Some(mut it) = vm.row_data(i) {
                    if it.selected {
                        it.selected = false;
                        vm.set_row_data(i, it);
                    }
                }
            }
        }
        a.set_selected_index(-1);
        a.set_selected_count(0);
        a.set_first_selected_id("".into());
        a.set_first_selected_filename("".into());
    });

    let m_res_sel = manager.clone();
    let selected_ids_res = selected_ids.clone();
    let progress_reg_res_sel = progress_registry.clone();
    app.on_resume_selected(move || {
        let sel = selected_ids_res.lock().unwrap().clone();
        for id in sel {
            let _ = m_res_sel.resume(&id);
            if let Ok(snap) = m_res_sel.snapshot_of(&id) {
                progress_reg_res_sel.open_dialog(&snap);
            }
        }
    });

    let m_pause_sel = manager.clone();
    let selected_ids_pause = selected_ids.clone();
    app.on_pause_selected(move || {
        let sel = selected_ids_pause.lock().unwrap().clone();
        for id in sel {
            let _ = m_pause_sel.pause(&id);
        }
    });

    let m_rem_sel = manager.clone();
    let selected_ids_rem = selected_ids.clone();
    let weak_app_rem = app.as_weak();
    app.on_remove_selected(move |delete_file| {
        let sel = selected_ids_rem.lock().unwrap().clone();
        for id in sel {
            let _ = m_rem_sel.remove(&id, delete_file);
        }
        selected_ids_rem.lock().unwrap().clear();
        if let Some(a) = weak_app_rem.upgrade() {
            a.set_selected_index(-1);
            a.set_selected_count(0);
            a.set_first_selected_id("".into());
            a.set_first_selected_filename("".into());
        }
    });

    let m = manager.clone();
    app.on_pause_all(move || {
        let _ = m.pause_all();
    });

    let m = manager.clone();
    let progress_reg_res_all = progress_registry.clone();
    app.on_resume_all(move || {
        let _ = m.resume_all();
        if let Ok(snaps) = m.list_downloads() {
            for snap in snaps {
                if snap.status == "downloading" || snap.status == "connecting" || snap.status == "queued" {
                    progress_reg_res_all.open_dialog(&snap);
                }
            }
        }
    });

    let m = manager.clone();
    app.on_clear_completed(move || {
        let _ = m.clear_completed();
    });

    // Folder picker + clipboard now live on the DownloadInfoDialog

    let selected_ids_copy = selected_ids.clone();
    let m_copy = manager.clone();
    app.on_copy_url(move |single_url| {
        let url_str = String::from(single_url);
        let sel = selected_ids_copy.lock().unwrap().clone();
        if sel.len() > 1 {
            let snaps = m_copy.list_downloads().unwrap_or_default();
            let urls: Vec<String> = snaps
                .into_iter()
                .filter(|s| sel.contains(&s.id))
                .map(|s| s.url)
                .collect();
            if !urls.is_empty() {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(urls.join("\r\n"));
                }
                return;
            }
        }
        if !url_str.is_empty() {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(url_str);
            }
        }
    });

    let settings = SettingsDialog::new().context("settings dialog")?;
    settings.global::<Palette>().set_active_theme(theme_idx);
    settings.set_speed_limit_mbps(init_speed_mbps);
    settings.set_max_connections(init_max_conns);
    settings.set_max_active(init_max_active);
    settings.set_start_at_startup(engine::startup::is_startup_enabled());

    let close_action = manager
        .db
        .get_kv("close_action")
        .unwrap_or_else(|| "tray".into());
    let close_action_val = Arc::new(Mutex::new(close_action.clone()));
    settings.set_close_action(close_action.into());

    let torrent_down_mbps = manager.db
        .get_kv("torrent_down_limit")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0);
    let torrent_up_mbps = manager.db
        .get_kv("torrent_up_limit")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0);
    let torrent_max_peers = manager.db
        .get_kv("torrent_max_peers")
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(100);
    let torrent_dht = manager.db
        .get_kv("torrent_dht")
        .map(|v| v != "0")
        .unwrap_or(true);
    let torrent_pex = manager.db
        .get_kv("torrent_pex")
        .map(|v| v != "0")
        .unwrap_or(true);
    let torrent_auto_trackers = manager.db
        .get_kv("torrent_auto_trackers")
        .map(|v| v != "0")
        .unwrap_or(true);

    settings.set_torrent_down_mbps(torrent_down_mbps);
    settings.set_torrent_up_mbps(torrent_up_mbps);
    settings.set_torrent_max_peers(torrent_max_peers);
    settings.set_torrent_dht_enabled(torrent_dht);
    settings.set_torrent_pex_enabled(torrent_pex);
    settings.set_torrent_auto_trackers(torrent_auto_trackers);

    // Settings
    let m = manager.clone();
    let weak_settings_sync = settings.as_weak();
    app.on_set_speed_limit(move |mbps| {
        let bps = if mbps <= 0.01 { 0 } else { (mbps as f64 * 1024.0 * 1024.0) as u64 };
        m.set_speed_limit(bps);
        if let Some(s) = weak_settings_sync.upgrade() {
            s.set_speed_limit_mbps(mbps);
        }
    });

    let m = manager.clone();
    let weak_settings_sync = settings.as_weak();
    app.on_set_max_connections(move |n| {
        m.set_max_connections(n as u64);
        if let Some(s) = weak_settings_sync.upgrade() {
            s.set_max_connections(n);
        }
    });

    let m = manager.clone();
    let weak_settings_sync = settings.as_weak();
    app.on_set_max_active(move |n| {
        m.set_max_active(n as u64);
        if let Some(s) = weak_settings_sync.upgrade() {
            s.set_max_active(n);
        }
    });

    let f = filter.clone();
    let l_f = last_sig.clone();
    app.on_filter_changed(move |v| {
        *f.lock().unwrap() = String::from(v);
        *l_f.lock().unwrap() = 0;
    });

    let s = search.clone();
    let l_s = last_sig.clone();
    app.on_search_changed(move |v| {
        *s.lock().unwrap() = String::from(v);
        *l_s.lock().unwrap() = 0;
    });

    let sc = sort_col.clone();
    let sa = sort_asc.clone();
    let l_sc = last_sig.clone();
    app.on_sort_changed(move |col, asc| {
        *sc.lock().unwrap() = String::from(col);
        *sa.lock().unwrap() = asc;
        *l_sc.lock().unwrap() = 0;
    });

    let m_up = manager.clone();
    let progress_reg_up = progress_registry.clone();
    app.on_update_download_url(move |id, new_url| {
        let id_str = String::from(id);
        let url_str = String::from(new_url).trim().to_string();
        if !id_str.is_empty() && !url_str.is_empty() {
            if let Err(e) = m_up.update_task_url(&id_str, &url_str) {
                eprintln!("[VDM] Failed to update download URL: {:?}", e);
            } else {
                let _ = m_up.resume(&id_str);
                if let Ok(snap) = m_up.snapshot_of(&id_str) {
                    progress_reg_up.open_dialog(&snap);
                }
            }
        }
    });

    app.on_open_browser_url(move |url| {
        let url_str = String::from(url).trim().to_string();
        if !url_str.is_empty() && url_str != "—" {
            let _ = std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", &url_str])
                .spawn();
        }
    });

    let weak_ren_app = renew_dialog.as_weak();
    let weak_main_app = app.as_weak();
    let act_ren_main = active_renewing_task.clone();
    let m_ren_app = manager.clone();
    app.on_renew_download(move |id| {
        let id_str = String::from(id);
        if let Ok(snap) = m_ren_app.snapshot_of(&id_str) {
            if let Some(ren) = weak_ren_app.upgrade() {
                let dl_text = format!("{} of {}", fmt_size(snap.downloaded), snap.total.map(fmt_size).unwrap_or_else(|| "—".into()));
                // only a real error state gets the red banner; paused/queued is not an error
                let err = if snap.status == "error" {
                    snap.error_msg.unwrap_or_else(|| "Download failed".into())
                } else {
                    String::new()
                };
                let pct = snap.total.map(|t| if t > 0 { snap.downloaded as f32 / t as f32 } else { 0.0 }).unwrap_or(0.0);
                let ref_url = if snap.referrer.is_empty() { snap.url.clone() } else { snap.referrer.clone() };
                let main_app = weak_main_app.upgrade();
                open_renew_dialog(
                    &ren,
                    main_app.as_ref().map(|a| a.window()),
                    &act_ren_main,
                    &snap.id,
                    &snap.filename,
                    &snap.url,
                    &ref_url,
                    &err,
                    &dl_text,
                    pct,
                );
            }
        }
    });

    // Windows Explorer integration (Open file / Reveal in folder)
    let m = manager.clone();
    app.on_open_file(move |id| {
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("explorer").arg(&path).spawn();
            }
        }
    });

    let m = manager.clone();
    app.on_open_folder(move |id| {
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("explorer")
                    .arg(format!("/select,{}", path.to_string_lossy()))
                    .spawn();
            } else if let Some(parent) = path.parent() {
                let _ = std::process::Command::new("explorer").arg(parent).spawn();
            }
        }
    });

    let m = manager.clone();
    app.on_open_file_with(move |id| {
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let path_str = path.to_string_lossy().to_string();
                let _ = std::process::Command::new("rundll32.exe")
                    .args(["shell32.dll,OpenAs_RunDLL", &path_str])
                    .spawn();
            }
        }
    });

    let m = manager.clone();
    app.on_open_shell_properties(move |id| {
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                #[cfg(target_os = "windows")]
                unsafe {
                    use std::os::windows::ffi::OsStrExt;
                    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_INVOKEIDLIST};
                    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOW;

                    let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
                    wide_path.push(0);
                    let verb: Vec<u16> = "properties\0".encode_utf16().collect();

                    let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
                    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
                    info.fMask = SEE_MASK_INVOKEIDLIST;
                    info.lpVerb = verb.as_ptr();
                    info.lpFile = wide_path.as_ptr();
                    info.nShow = SW_SHOW;

                    ShellExecuteExW(&mut info);
                }
            }
        }
    });

    let m = manager.clone();
    app.on_redownload(move |id| {
        let _ = m.redownload(&String::from(id));
    });

    let m = manager.clone();
    app.on_rename_task(move |id, new_filename, new_dir| {
        let id_str = String::from(id);
        let name_str = String::from(new_filename);
        let dir_str = String::from(new_dir);
        let dir_opt = if dir_str.trim().is_empty() { None } else { Some(dir_str.as_str()) };
        let _ = m.rename_task(&id_str, &name_str, dir_opt);
    });

    app.on_pick_rename_folder(move || {
        let dialog = rfd::FileDialog::new();
        if let Some(path) = dialog.pick_folder() {
            path.to_string_lossy().to_string().into()
        } else {
            "".into()
        }
    });

    let m = manager.clone();
    app.on_queue_action(move |id, action| {
        let id_str = String::from(id);
        let action_str = String::from(action);
        match action_str.as_str() {
            "start" | "top" => {
                let _ = m.resume(&id_str);
            }
            "stop" | "remove" => {
                let _ = m.pause(&id_str);
            }
            _ => {}
        }
    });

    let m = manager.clone();
    app.on_set_double_click_action(move |act| {
        let act_str = String::from(act);
        let _ = m.db.set_kv("double_click_action", &act_str);
    });

    // ── System tray (squircle logo, custom morphing menu) ──
    let tray_rgba = logo_rgba(32).context("rasterize tray icon")?;
    let _tray = tray_icon::TrayIconBuilder::new()
        .with_tooltip("VDM — Download Manager")
        .with_icon(tray_icon::Icon::from_rgba(tray_rgba, 32, 32).context("tray icon data")?)
        .build()
        .context("create tray icon")?;

    // ── Mini-Pill callbacks ──
    let weak_pill_drag = mini_pill.as_weak();
    mini_pill.on_drag_window(move || {
        if let Some(pill) = weak_pill_drag.upgrade() {
            pill.window().with_winit_window(|win| {
                let _ = win.drag_window();
            });
        }
    });

    // ── Renew Link Dialog Callbacks ──
    let weak_ren_drag = renew_dialog.as_weak();
    renew_dialog.on_drag_window(move || {
        if let Some(d) = weak_ren_drag.upgrade() {
            d.window().with_winit_window(|win| {
                let _ = win.drag_window();
            });
        }
    });

    let weak_ren_close = renew_dialog.as_weak();
    let act_ren_close = active_renewing_task.clone();
    renew_dialog.on_closed(move || {
        if let Some(d) = weak_ren_close.upgrade() {
            let _ = d.hide();
        }
        *act_ren_close.lock().unwrap() = None;
    });

    renew_dialog.on_open_page(move |url| {
        let url_str = String::from(url).trim().to_string();
        if !url_str.is_empty() && url_str != "—" {
            let _ = std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", &url_str])
                .spawn();
        }
    });

    let weak_ren_paste = renew_dialog.as_weak();
    renew_dialog.on_paste_clipboard(move || {
        if let Some(url) = extract_url_from_clipboard() {
            if let Some(d) = weak_ren_paste.upgrade() {
                d.set_new_url(url.into());
            }
        }
    });

    let weak_ren_apply = renew_dialog.as_weak();
    let act_ren_apply = active_renewing_task.clone();
    let m_ren_apply = manager.clone();
    let progress_reg_ren_apply = progress_registry.clone();
    renew_dialog.on_apply_url(move |task_id, new_url| {
        let id_str = String::from(task_id).trim().to_string();
        let url_str = String::from(new_url).trim().to_string();
        if !id_str.is_empty() && !url_str.is_empty() {
            println!("[VDM] Applying renewed download link for task {}: {}", id_str, url_str);
            if let Err(e) = m_ren_apply.update_task_url(&id_str, &url_str) {
                eprintln!("[VDM] Failed to update download URL: {:?}", e);
            } else {
                let _ = m_ren_apply.resume(&id_str);
                *act_ren_apply.lock().unwrap() = None;
                if let Some(d) = weak_ren_apply.upgrade() {
                    let _ = d.hide();
                }
                if let Ok(snap) = m_ren_apply.snapshot_of(&id_str) {
                    progress_reg_ren_apply.open_dialog(&snap);
                }
            }
        }
    });

    let weak_ren_retry = renew_dialog.as_weak();
    let act_ren_retry = active_renewing_task.clone();
    let m_ren_retry = manager.clone();
    let progress_reg_ren_retry = progress_registry.clone();
    renew_dialog.on_retry_current(move |task_id| {
        let id_str = String::from(task_id).trim().to_string();
        if !id_str.is_empty() {
            let _ = m_ren_retry.resume(&id_str);
            *act_ren_retry.lock().unwrap() = None;
            if let Some(d) = weak_ren_retry.upgrade() {
                let _ = d.hide();
            }
            if let Ok(snap) = m_ren_retry.snapshot_of(&id_str) {
                progress_reg_ren_retry.open_dialog(&snap);
            }
        }
    });

    // ── Payloads state for browser extension and dialogs ──
    let pending_payloads: Arc<Mutex<HashMap<String, (HashMap<String, String>, Option<u64>)>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let pending_payloads_server = pending_payloads.clone();
    let pending_payloads_confirm = pending_payloads.clone();

    // ── Duplicate / File Conflict Dialog Callbacks ──
    let weak_dup_drag = duplicate_dialog.as_weak();
    duplicate_dialog.on_drag_window(move || {
        if let Some(d) = weak_dup_drag.upgrade() {
            d.window().with_winit_window(|win| {
                let _ = win.drag_window();
            });
        }
    });

    let weak_dup_close = duplicate_dialog.as_weak();
    let pending_dup_close = pending_payloads.clone();
    duplicate_dialog.on_closed(move || {
        if let Some(d) = weak_dup_close.upgrade() {
            // drop the staged intercept payload so dismissed dialogs don't leak it
            pending_dup_close.lock().unwrap().remove(&String::from(d.get_new_url()));
            let _ = d.hide();
        }
    });

    let weak_dup_ren = duplicate_dialog.as_weak();
    let m_dup_ren = manager.clone();
    let p_reg_dup_ren = progress_registry.clone();
    let pending_payloads_dup_ren = pending_payloads.clone();
    duplicate_dialog.on_rename_and_download(move |new_fname| {
        if let Some(d) = weak_dup_ren.upgrade() {
            let _ = d.hide();
            let url: String = d.get_new_url().into();
            let folder: String = d.get_save_folder().into();
            let fname_str: String = new_fname.into();
            let (headers, file_size) = pending_payloads_dup_ren.lock().unwrap().remove(&url).unwrap_or_default();
            if let Ok(snap) = m_dup_ren.add_download_with_total(url, Some(folder), Some(fname_str), headers, file_size) {
                p_reg_dup_ren.open_dialog(&snap);
            }
        }
    });

    let weak_dup_upd = duplicate_dialog.as_weak();
    let m_dup_upd = manager.clone();
    let p_reg_dup_upd = progress_registry.clone();
    let pending_payloads_dup_upd = pending_payloads.clone();
    duplicate_dialog.on_resume_or_update_link(move |task_id, new_url| {
        if let Some(d) = weak_dup_upd.upgrade() {
            let _ = d.hide();
            let id_str: String = task_id.into();
            let url_str: String = new_url.into();
            let (headers, _) = pending_payloads_dup_upd.lock().unwrap().remove(&url_str).unwrap_or_default();
            let _ = m_dup_upd.update_task_url_and_headers(&id_str, &url_str, Some(&headers));
            let _ = m_dup_upd.resume(&id_str);
            if let Ok(snap) = m_dup_upd.snapshot_of(&id_str) {
                p_reg_dup_upd.open_dialog(&snap);
            }
        }
    });

    let weak_dup_ovr = duplicate_dialog.as_weak();
    let m_dup_ovr = manager.clone();
    let p_reg_dup_ovr = progress_registry.clone();
    let pending_payloads_dup_ovr = pending_payloads.clone();
    duplicate_dialog.on_overwrite_existing(move |task_id, new_url| {
        if let Some(d) = weak_dup_ovr.upgrade() {
            let _ = d.hide();
            let id_str: String = task_id.into();
            let url_str: String = new_url.into();
            let folder: String = d.get_save_folder().into();
            let fname: String = d.get_filename().into();
            let (headers, file_size) = pending_payloads_dup_ovr.lock().unwrap().remove(&url_str).unwrap_or_default();
            let _ = m_dup_ovr.remove(&id_str, true);
            if let Ok(snap) = m_dup_ovr.add_download_with_total(url_str, Some(folder), Some(fname), headers, file_size) {
                p_reg_dup_ovr.open_dialog(&snap);
            }
        }
    });

    let weak_dup_show = duplicate_dialog.as_weak();
    let m_dup_show = manager.clone();
    let p_reg_dup_show = progress_registry.clone();
    duplicate_dialog.on_show_existing_progress(move |task_id| {
        if let Some(d) = weak_dup_show.upgrade() {
            let _ = d.hide();
            let id_str: String = task_id.into();
            if let Ok(snap) = m_dup_show.snapshot_of(&id_str) {
                p_reg_dup_show.open_dialog(&snap);
            }
        }
    });

    // ── Shutdown Countdown Dialog Callbacks ──
    let weak_sd_drag = shutdown_dialog.as_weak();
    shutdown_dialog.on_drag_window(move || {
        if let Some(d) = weak_sd_drag.upgrade() {
            d.window().with_winit_window(|win| {
                let _ = win.drag_window();
            });
        }
    });

    let weak_sd_cancel = shutdown_dialog.as_weak();
    let act_sd_cancel = countdown_active.clone();
    shutdown_dialog.on_cancel_shutdown(move || {
        abort_shutdown_countdown(&weak_sd_cancel, &act_sd_cancel);
    });

    let weak_sd_close = shutdown_dialog.as_weak();
    let act_sd_close = countdown_active.clone();
    shutdown_dialog.on_closed(move || {
        abort_shutdown_countdown(&weak_sd_close, &act_sd_close);
    });

    let weak_sd_now = shutdown_dialog.as_weak();
    let act_sd_now = countdown_active.clone();
    shutdown_dialog.on_shutdown_now(move || {
        act_sd_now.store(false, Ordering::SeqCst);
        SHUTDOWN_ARMED.store(false, Ordering::Relaxed);
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("shutdown")
                .args(["/s", "/t", "0"])
                .spawn();
        }
        if let Some(d) = weak_sd_now.upgrade() {
            let _ = d.hide();
        }
    });

    // ── Browser Integration & Settings Dialog ──
    fn refresh_browser_list(settings: &SettingsDialog) {
        let browsers = engine::browser::BrowserDetector::get_browsers();
        let items: Vec<BrowserItem> = browsers
            .into_iter()
            .map(|b| BrowserItem {
                id: b.id.into(),
                name: b.name.into(),
                installed: b.installed,
                running: b.running,
            })
            .collect();
        settings.set_browsers(ModelRc::new(VecModel::from(items)));
    }

    // Pre-populate so it's instantly ready
    refresh_browser_list(&settings);

    let weak_settings = settings.as_weak();
    let weak_app_settings = app.as_weak();
    app.on_open_settings(move || {
        if let Some(s) = weak_settings.upgrade() {
            s.set_start_at_startup(engine::startup::is_startup_enabled());
            let _ = s.show();
            if let Some(a) = weak_app_settings.upgrade() {
                center_over_main(a.window(), s.window(), 580.0, 500.0);
            }
            refresh_browser_list(&s);
        }
    });

    let weak_settings_drag = settings.as_weak();
    settings.on_drag_window(move || {
        if let Some(s) = weak_settings_drag.upgrade() {
            s.window().with_winit_window(|win| {
                let _ = win.drag_window();
            });
        }
    });

    let weak_settings_inst = settings.as_weak();
    settings.on_install_extension(move |id| {
        engine::browser::BrowserDetector::install_extension_for(&id);
        if let Some(s) = weak_settings_inst.upgrade() {
            refresh_browser_list(&s);
        }
    });

    settings.on_download_browser(move |id| {
        engine::browser::BrowserDetector::open_download_page(&id);
    });

    settings.on_open_extension_folder(move || {
        let ext_dir = engine::browser::BrowserDetector::get_extension_dir();
        let _ = std::process::Command::new("explorer").arg(&ext_dir).spawn();
    });

    let weak_settings_close = settings.as_weak();
    settings.on_closed(move || {
        if let Some(s) = weak_settings_close.upgrade() {
            let _ = s.hide();
        }
    });

    let weak_settings_startup = settings.as_weak();
    let m_startup = manager.clone();
    settings.on_set_start_at_startup(move |val| {
        if let Err(e) = engine::startup::set_startup_enabled(val) {
            eprintln!("[VDM Startup] Failed to update Windows startup setting: {}", e);
        }
        let _ = m_startup.db.set_kv("start_at_startup", if val { "1" } else { "0" });
        if let Some(s) = weak_settings_startup.upgrade() {
            s.set_start_at_startup(engine::startup::is_startup_enabled());
        }
    });

    settings.on_open_startup_apps(move || {
        engine::startup::open_windows_startup_settings();
    });

    let m_close_act = manager.clone();
    let close_act_store = close_action_val.clone();
    let weak_settings_close_act = settings.as_weak();
    settings.on_set_close_action(move |action| {
        let act_str: String = action.into();
        *close_act_store.lock().unwrap() = act_str.clone();
        let _ = m_close_act.db.set_kv("close_action", &act_str);
        if let Some(s) = weak_settings_close_act.upgrade() {
            s.set_close_action(act_str.into());
        }
    });

    let m = manager.clone();
    let weak_app_sync = app.as_weak();
    settings.on_set_max_active(move |n| {
        m.set_max_active(n as u64);
        if let Some(a) = weak_app_sync.upgrade() {
            a.set_max_active(n);
        }
    });

    let m = manager.clone();
    let weak_app_sync = app.as_weak();
    settings.on_set_max_connections(move |n| {
        m.set_max_connections(n as u64);
        if let Some(a) = weak_app_sync.upgrade() {
            a.set_max_connections(n);
        }
    });

    let m = manager.clone();
    let weak_app_sync = app.as_weak();
    settings.on_set_speed_limit(move |mbps| {
        let bps = if mbps <= 0.01 { 0 } else { (mbps as f64 * 1024.0 * 1024.0) as u64 };
        m.set_speed_limit(bps);
        if let Some(a) = weak_app_sync.upgrade() {
            a.set_speed_limit_mbps(mbps);
        }
    });

    let m_t_down = manager.clone();
    settings.on_set_torrent_down_limit(move |mbps| {
        m_t_down.update_torrent_settings(|cur| {
            cur.max_download_bps = if mbps <= 0.01 { 0 } else { (mbps as f64 * 1024.0 * 1024.0) as u64 };
        });
        let _ = m_t_down.db.set_kv("torrent_down_limit", &mbps.to_string());
    });

    let m_t_up = manager.clone();
    settings.on_set_torrent_up_limit(move |mbps| {
        m_t_up.update_torrent_settings(|cur| {
            cur.max_upload_bps = if mbps <= 0.01 { 0 } else { (mbps as f64 * 1024.0 * 1024.0) as u64 };
        });
        let _ = m_t_up.db.set_kv("torrent_up_limit", &mbps.to_string());
    });

    let m_t_peers = manager.clone();
    settings.on_set_torrent_max_peers(move |n| {
        m_t_peers.update_torrent_settings(|cur| {
            cur.max_peers = (n as usize).clamp(10, 500);
        });
        let _ = m_t_peers.db.set_kv("torrent_max_peers", &n.to_string());
    });

    let m_t_dht = manager.clone();
    settings.on_set_torrent_dht_enabled(move |val| {
        m_t_dht.update_torrent_settings(|cur| {
            cur.enable_dht = val;
        });
        let _ = m_t_dht.db.set_kv("torrent_dht", if val { "1" } else { "0" });
    });

    let m_t_pex = manager.clone();
    settings.on_set_torrent_pex_enabled(move |val| {
        m_t_pex.update_torrent_settings(|cur| {
            cur.enable_pex = val;
        });
        let _ = m_t_pex.db.set_kv("torrent_pex", if val { "1" } else { "0" });
    });

    let m_t_trackers = manager.clone();
    settings.on_set_torrent_auto_trackers(move |val| {
        m_t_trackers.update_torrent_settings(|cur| {
            cur.enable_auto_trackers = val;
        });
        let _ = m_t_trackers.db.set_kv("torrent_auto_trackers", if val { "1" } else { "0" });
    });

    let weak_app_th = app.as_weak();
    let weak_settings_th = settings.as_weak();
    let weak_info_th = info.as_weak();
    let weak_torrent_th = torrent_dialog.as_weak();
    let weak_renew_th = renew_dialog.as_weak();
    let weak_dup_th = duplicate_dialog.as_weak();
    let weak_pill_th = mini_pill.as_weak();
    let weak_done_th = done.as_weak();
    let weak_shutdown_th = shutdown_dialog.as_weak();
    let m_th = manager.clone();
    let prog_reg_th = progress_registry.clone();

    settings.on_set_theme(move |idx| {
        let _ = m_th.db.set_kv("theme", &idx.to_string());
        if let Some(w) = weak_app_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }
        if let Some(w) = weak_settings_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }
        if let Some(w) = weak_info_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }
        if let Some(w) = weak_torrent_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }
        if let Some(w) = weak_renew_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }
        if let Some(w) = weak_dup_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }
        if let Some(w) = weak_pill_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }
        if let Some(w) = weak_done_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }
        if let Some(w) = weak_shutdown_th.upgrade() { w.global::<Palette>().set_active_theme(idx); }

        let active_dialogs = prog_reg_th.dialogs.lock().unwrap();
        for (_, weak_p) in active_dialogs.iter() {
            if let Some(p) = weak_p.upgrade() {
                p.global::<Palette>().set_active_theme(idx);
            }
        }
    });

    // ── Shutdown state fence ──
    let is_shutting_down = Arc::new(AtomicBool::new(false));

    // ── Local Loopback Server (:9191) for Browser Extension ──
    let (download_tx, mut download_rx) = tokio::sync::mpsc::unbounded_channel::<engine::server::ServerEvent>();
    let loopback_server = Arc::new(engine::server::LoopbackServer::new(
        engine::server::DEFAULT_SERVER_PORT,
        download_tx,
    ));
    tokio::spawn(async move {
        loopback_server.run().await;
    });

    let weak_info_server = info.as_weak();
    let weak_torrent_server = torrent_dialog.as_weak();
    let weak_app_server = app.as_weak();
    let weak_ren_server = renew_dialog.as_weak();
    let weak_dup_server = duplicate_dialog.as_weak();
    let active_ren_server = active_renewing_task.clone();
    let m_server = manager.clone();
    let weak_settings_state = settings.as_weak();
    let progress_reg_server = progress_registry.clone();
    let is_shutting_down_events = is_shutting_down.clone();

    tokio::spawn(async move {
        while let Some(event) = download_rx.recv().await {
            if is_shutting_down_events.load(Ordering::Relaxed) {
                break;
            }
            let wi = weak_info_server.clone();
            let wt = weak_torrent_server.clone();
            let wa = weak_app_server.clone();
            let wren = weak_ren_server.clone();
            let wdup = weak_dup_server.clone();
            let act_ren = active_ren_server.clone();
            let ws = weak_settings_state.clone();
            let m = m_server.clone();
            let ps = pending_payloads_server.clone();
            let p_reg = progress_reg_server.clone();
            let is_shutting_down_ev = is_shutting_down_events.clone();

            let _ = slint::invoke_from_event_loop(move || {
                if is_shutting_down_ev.load(Ordering::Relaxed) {
                    return;
                }
                match event {
                    engine::server::ServerEvent::ShowWindow => {
                        if let Some(a) = wa.upgrade() {
                            a.window().with_winit_window(|win| {
                                win.set_visible(true);
                                win.set_minimized(false);
                                win.focus_window();
                            });
                            let _ = a.show();
                            a.window().set_minimized(false);
                        }
                    }
                    engine::server::ServerEvent::AddDownload(payload) => {
                        if payload.url.starts_with("magnet:?") {
                            if let Some(td) = wt.upgrade() {
                                open_torrent_select_dialog(
                                    &td,
                                    wa.upgrade().as_ref().map(|a| a.window()),
                                    m.clone(),
                                    payload.url,
                                    None,
                                );
                            }
                            return;
                        }

                        let show_modal = ws.upgrade().map(|s| s.get_show_info_modal()).unwrap_or(true);

                        let inferred = engine::probe::url_basename(&payload.url);
                        let filename = if payload.filename.is_empty()
                            || payload.filename == "main"
                            || payload.filename == "master"
                            || payload.filename == "download"
                            || !payload.filename.contains('.')
                        {
                            inferred
                                .filter(|s| s.contains('.') && !s.is_empty())
                                .or_else(|| if !payload.filename.is_empty() { Some(payload.filename.clone()) } else { None })
                                .unwrap_or_else(|| "download".into())
                        } else {
                            payload.filename.clone()
                        };

                        let cat = detect_category_name(&filename);
                        let folder = get_category_folder(&cat);
                        let file_size_text = payload.file_size.map(fmt_size).unwrap_or_else(|| "—".into());

                        let mut headers = HashMap::new();
                        if !payload.cookies.is_empty() {
                            headers.insert("Cookie".to_string(), payload.cookies.clone());
                        }
                        if !payload.referrer.is_empty() {
                            headers.insert("Referer".to_string(), payload.referrer.clone());
                        }
                        if !payload.user_agent.is_empty() {
                            headers.insert("User-Agent".to_string(), payload.user_agent.clone());
                        }

                        // ── Explicit Link Renewal Capture (Only when the user actively opened the Renew Dialog for a task) ──
                        let active_ren_id = act_ren.lock().unwrap().clone();
                        if let Some(ref ren_id) = active_ren_id {
                            if let Ok(existing) = m.snapshot_of(ren_id) {
                                println!("[VDM] Re-capturing fresh download link for explicitly renewed task {}: {}", existing.id, existing.filename);
                                let _ = m.update_task_url_and_headers(&existing.id, &payload.url, Some(&headers));
                                let _ = m.resume(&existing.id);
                                *act_ren.lock().unwrap() = None;
                                if let Some(ren) = wren.upgrade() {
                                    let _ = ren.hide();
                                }
                                if let Ok(snap) = m.snapshot_of(&existing.id) {
                                    p_reg.open_dialog(&snap);
                                }
                                return;
                            }
                        }

                        // ── Check for Duplicate / Conflict Download ──
                        if let Ok(snaps) = m.list_downloads() {
                            let conflict_task = snaps.iter().find(|t| {
                                (t.filename.eq_ignore_ascii_case(&filename) && t.dir.eq_ignore_ascii_case(&folder))
                                    || (t.url == payload.url && !payload.url.is_empty())
                            });

                            if let Some(existing) = conflict_task {
                                let is_active = existing.status == "downloading" || existing.status == "connecting";
                                let existing_status_desc = match existing.status.as_str() {
                                    "downloading" => {
                                        let pct = existing.total.map(|tot| if tot > 0 { (existing.downloaded as f32 / tot as f32).min(1.0) * 100.0 } else { 0.0 }).unwrap_or(0.0);
                                        format!("Downloading ({:.0}% at {})", pct, fmt_speed(existing.speed_bps))
                                    },
                                    "connecting" => "Connecting...".into(),
                                    "paused" => {
                                        let pct = existing.total.map(|tot| if tot > 0 { (existing.downloaded as f32 / tot as f32).min(1.0) * 100.0 } else { 0.0 }).unwrap_or(0.0);
                                        if pct > 0.0 {
                                            format!("Paused ({:.0}% done, {})", pct, fmt_size(existing.downloaded))
                                        } else {
                                            format!("Paused ({})", fmt_size(existing.downloaded))
                                        }
                                    },
                                    "completed" => format!("Completed on disk ({})", fmt_size(existing.downloaded)),
                                    "error" => "Paused / Expired Link".into(),
                                    other => other.to_string(),
                                };
                                let existing_size = existing.total.map(fmt_size).unwrap_or_else(|| fmt_size(existing.downloaded));
                                let suggested_rename = compute_next_duplicate_name(&folder, &filename, &snaps);

                                ps.lock().unwrap().insert(payload.url.clone(), (headers.clone(), payload.file_size));

                                if let Some(dup) = wdup.upgrade() {
                                    dup.set_task_id(existing.id.clone().into());
                                    dup.set_filename(filename.clone().into());
                                    dup.set_new_suggested_name(suggested_rename.into());
                                    dup.set_existing_status(existing_status_desc.into());
                                    dup.set_existing_size_text(existing_size.into());
                                    dup.set_save_folder(folder.clone().into());
                                    dup.set_new_url(payload.url.clone().into());
                                    dup.set_is_active_downloading(is_active);
                                    let _ = dup.show();

                                    let main_app = wa.upgrade();
                                    center_dialog(main_app.as_ref().map(|a| a.window()), dup.window(), 540.0, if is_active { 310.0 } else { 380.0 });

                                    dup.window().with_winit_window(|win| {
                                        win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
                                        win.set_visible(true);
                                        win.focus_window();
                                        win.request_user_attention(Some(i_slint_backend_winit::winit::window::UserAttentionType::Critical));
                                    });
                                    return;
                                }
                            }
                        }

                        if show_modal {
                            ps.lock()
                                .unwrap()
                                .insert(payload.url.clone(), (headers.clone(), payload.file_size));

                            if let Some(d) = wi.upgrade() {
                                d.set_url(payload.url.clone().into());
                                update_dialog_for_file(&d, &filename);
                                d.set_file_size_text(file_size_text.into());
                                d.set_description("".into());
                                d.set_remember_path(true);
                                d.set_start_now(true);
                                let _ = d.show();

                                let main_app = wa.upgrade();
                                center_dialog(main_app.as_ref().map(|a| a.window()), d.window(), 540.0, 285.0);

                                d.window().with_winit_window(|win| {
                                    win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
                                    win.set_visible(true);
                                    win.focus_window();
                                    win.request_user_attention(Some(i_slint_backend_winit::winit::window::UserAttentionType::Critical));
                                });

                                spawn_probe_for_dialog(wi.clone(), payload.url, headers);
                            }
                        } else {
                            let fname = if filename.is_empty() { None } else { Some(filename) };
                            if let Ok(snap) = m.add_download_with_total(payload.url, Some(folder), fname, headers, payload.file_size) {
                                p_reg.open_dialog(&snap);
                            }
                        }
                    }
                }
            });
        }
    });

    // tray event pump: left click shows VDM, right click pops the morph menu
    let ev_app = app.as_weak();
    let ev_menu = menu.as_weak();
    let menu_gen = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let menu_gen_tray = menu_gen.clone();

    // close the menu with its morph-out animation
    fn close_menu_later(weak: slint::Weak<TrayMenu>, gen: &Arc<std::sync::atomic::AtomicU64>) {
        gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(mm) = weak.upgrade() {
            mm.set_open(false);
            let w2 = weak.clone();
            slint::Timer::single_shot(Duration::from_millis(220), move || {
                if let Some(mm) = w2.upgrade() {
                    let _ = mm.hide();
                }
            });
        }
    }

    let is_shutting_down_tray = is_shutting_down.clone();
    std::thread::spawn(move || {
        let rx = tray_icon::TrayIconEvent::receiver();
        while let Ok(ev) = rx.recv() {
            if is_shutting_down_tray.load(Ordering::Relaxed) {
                break;
            }
            match ev {
                tray_icon::TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    ..
                } | tray_icon::TrayIconEvent::DoubleClick {
                    button: tray_icon::MouseButton::Left,
                    ..
                } => {
                    let wa = ev_app.clone();
                    let wm = ev_menu.clone();
                    let mg = menu_gen_tray.clone();
                    let is_sd_click = is_shutting_down_tray.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if is_sd_click.load(Ordering::Relaxed) {
                            return;
                        }
                        close_menu_later(wm, &mg);
                        if let Some(a) = wa.upgrade() {
                            a.window().with_winit_window(|win| {
                                win.set_visible(true);
                                win.set_minimized(false);
                                win.focus_window();
                            });
                            let _ = a.show();
                            a.window().set_minimized(false);
                        }
                    });
                }
                tray_icon::TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Right,
                    position,
                    ..
                } => {
                    let wa = ev_app.clone();
                    let wm = ev_menu.clone();
                    let mg = menu_gen_tray.clone();
                    let is_sd_rclick = is_shutting_down_tray.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if is_sd_rclick.load(Ordering::Relaxed) {
                            return;
                        }
                        if let Some(mm) = wm.upgrade() {
                            // If already open, toggle close
                            if mm.get_open() {
                                close_menu_later(wm, &mg);
                                return;
                            }

                            let gen = mg.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            let scale = wa.upgrade().map(|a| a.window().scale_factor()).unwrap_or(1.0);
                            mm.set_open(false);
                            let mw = (180.0 * scale) as i32;
                            let mh = (185.0 * scale) as i32;
                            let x = (position.x as i32 - mw / 2).max(10);
                            let y = (position.y as i32 - mh - (8.0 * scale) as i32).max(10);
                            let _ = mm.show();
                            mm.window()
                                .set_position(slint::WindowPosition::Physical(slint::PhysicalPosition::new(x, y)));
                            mm.window().with_winit_window(|win| {
                                win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
                                win.focus_window();
                            });
                            let w2 = wm.clone();
                            slint::Timer::single_shot(Duration::from_millis(20), move || {
                                if let Some(mm) = w2.upgrade() {
                                    mm.set_open(true);
                                }
                            });

                            // Watch for clicks outside the tray menu or Escape key to dismiss it automatically
                            let wm_poll = wm.clone();
                            let mg_poll = mg.clone();
                            let is_sd_watch = is_sd_rclick.clone();
                            std::thread::spawn(move || {
                                let start = std::time::Instant::now();
                                loop {
                                    std::thread::sleep(Duration::from_millis(25));

                                    if is_sd_watch.load(Ordering::Relaxed) {
                                        break;
                                    }

                                    // Terminate if the menu has been closed or re-opened
                                    if mg_poll.load(std::sync::atomic::Ordering::SeqCst) != gen {
                                        break;
                                    }

                                    // Startup grace period to prevent initial click from dismissing
                                    if start.elapsed() < Duration::from_millis(180) {
                                        continue;
                                    }

                                    #[cfg(target_os = "windows")]
                                    {
                                        use windows_sys::Win32::Foundation::POINT;
                                        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
                                            GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
                                        };
                                        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

                                        let esc = (unsafe { GetAsyncKeyState(VK_ESCAPE as i32) } as u16 & 0x8000) != 0;
                                        if esc {
                                            let wm = wm_poll.clone();
                                            let mg = mg_poll.clone();
                                            let _ = slint::invoke_from_event_loop(move || {
                                                close_menu_later(wm, &mg);
                                            });
                                            break;
                                        }

                                        let lb = (unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000) != 0;
                                        let rb = (unsafe { GetAsyncKeyState(VK_RBUTTON as i32) } as u16 & 0x8000) != 0;
                                        let mb = (unsafe { GetAsyncKeyState(VK_MBUTTON as i32) } as u16 & 0x8000) != 0;

                                        if lb || rb || mb {
                                            let mut pt = POINT { x: 0, y: 0 };
                                            if unsafe { GetCursorPos(&mut pt) } != 0 {
                                                let inside = pt.x >= x && pt.x <= x + mw && pt.y >= y && pt.y <= y + mh;
                                                if !inside {
                                                    let wm = wm_poll.clone();
                                                    let mg = mg_poll.clone();
                                                    let _ = slint::invoke_from_event_loop(move || {
                                                        close_menu_later(wm, &mg);
                                                    });
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                        }
                    });
                }
                _ => {}
            }
        }
    });

    let weak_menu_quit = menu.as_weak();
    let weak_app_quit = app.as_weak();
    let weak_info_quit = info.as_weak();
    let weak_torrent_quit = torrent_dialog.as_weak();
    let weak_renew_quit = renew_dialog.as_weak();
    let weak_dup_quit = duplicate_dialog.as_weak();
    let weak_pill_quit = mini_pill.as_weak();
    let weak_done_quit = done.as_weak();
    let weak_settings_quit = settings.as_weak();
    let weak_shutdown_quit = shutdown_dialog.as_weak();
    let act_shutdown_quit = countdown_active.clone();
    let progress_reg_menu = progress_registry.clone();
    let m = manager.clone();

    let is_shutting_down_action = is_shutting_down.clone();
    let perform_graceful_shutdown = Arc::new(move || {
        if is_shutting_down_action.swap(true, Ordering::SeqCst) {
            return;
        }

        // 1. Anti-hang watchdog FIRST: guarantees process termination within 900ms-1000ms
        // no matter what happens in OS drivers, networking, or file I/O subsystems
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(900));
            std::process::exit(0);
        });

        // 2. Hide all open windows immediately on the UI thread
        let mm_w = weak_menu_quit.clone();
        let a_w = weak_app_quit.clone();
        let inf_w = weak_info_quit.clone();
        let td_w = weak_torrent_quit.clone();
        let rd_w = weak_renew_quit.clone();
        let dd_w = weak_dup_quit.clone();
        let pill_w = weak_pill_quit.clone();
        let dn_w = weak_done_quit.clone();
        let st_w = weak_settings_quit.clone();
        let sd_w = weak_shutdown_quit.clone();
        let prog_reg_w = progress_reg_menu.clone();

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(mm) = mm_w.upgrade() { let _ = mm.hide(); }
            if let Some(a) = a_w.upgrade() { let _ = a.hide(); }
            if let Some(inf) = inf_w.upgrade() { let _ = inf.hide(); }
            if let Some(td) = td_w.upgrade() { let _ = td.hide(); }
            if let Some(rd) = rd_w.upgrade() { let _ = rd.hide(); }
            if let Some(dd) = dd_w.upgrade() { let _ = dd.hide(); }
            if let Some(pill) = pill_w.upgrade() { let _ = pill.hide(); }
            if let Some(dn) = dn_w.upgrade() { let _ = dn.hide(); }
            if let Some(st) = st_w.upgrade() { let _ = st.hide(); }
            if let Some(sd) = sd_w.upgrade() { let _ = sd.hide(); }
            for (_, weak_p) in prog_reg_w.dialogs.lock().unwrap().drain() {
                if let Some(p) = weak_p.upgrade() {
                    let _ = p.hide();
                }
            }
        });

        abort_shutdown_countdown(&weak_shutdown_quit, &act_shutdown_quit);

        // 3. Pause all downloads cleanly (synchronously persists all chunk progress to DB and in-memory cache)
        m.pause_all();

        // 4. Grace period (100ms) to allow OS file buffers to flush and network sockets to release
        std::thread::sleep(Duration::from_millis(100));

        // 5. Quit Slint event loop
        let _ = slint::quit_event_loop();

        // 6. Clean exit (immediately releases all file locks, handles, sockets, and resources)
        std::process::exit(0);
    });

    let weak_app_close = app.as_weak();
    let close_act_for_win = close_action_val.clone();
    let shutdown_for_win = perform_graceful_shutdown.clone();
    app.on_close_window(move || {
        let act = close_act_for_win.lock().unwrap().clone();
        if act == "tray" {
            if let Some(a) = weak_app_close.upgrade() {
                a.window().with_winit_window(|win| {
                    win.set_visible(false);
                });
                let _ = a.hide();
                trim_working_set();
            }
        } else {
            shutdown_for_win();
        }
    });

    let weak_app_req = app.as_weak();
    let close_act_for_req = close_action_val.clone();
    let shutdown_for_req = perform_graceful_shutdown.clone();
    app.window().on_close_requested(move || {
        let act = close_act_for_req.lock().unwrap().clone();
        if act == "tray" {
            if let Some(a) = weak_app_req.upgrade() {
                a.window().with_winit_window(|win| {
                    win.set_visible(false);
                });
                let _ = a.hide();
                trim_working_set();
            }
            slint::CloseRequestResponse::HideWindow
        } else {
            shutdown_for_req();
            slint::CloseRequestResponse::HideWindow
        }
    });

    let shutdown_for_ctrlc = perform_graceful_shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            shutdown_for_ctrlc();
        }
    });

    let weak_menu = menu.as_weak();
    let weak_app = app.as_weak();
    let shutdown_for_tray = perform_graceful_shutdown.clone();
    let menu_gen_item = menu_gen.clone();
    let m = manager.clone();
    menu.on_item(move |what| {
        let what: String = what.into();
        match what.as_str() {
            "open" => {
                close_menu_later(weak_menu.clone(), &menu_gen_item);
                if let Some(a) = weak_app.upgrade() {
                    a.window().with_winit_window(|win| {
                        win.set_visible(true);
                        win.set_minimized(false);
                        win.focus_window();
                    });
                    let _ = a.show();
                    a.window().set_minimized(false);
                }
            }
            "pause-all" => {
                close_menu_later(weak_menu.clone(), &menu_gen_item);
                m.pause_all();
            }
            "resume-all" => {
                close_menu_later(weak_menu.clone(), &menu_gen_item);
                m.resume_all();
            }
            "quit" => {
                shutdown_for_tray();
            }
            _ => {
                close_menu_later(weak_menu.clone(), &menu_gen_item);
            }
        }
    });

    let weak_menu = menu.as_weak();
    let menu_gen_dismiss = menu_gen.clone();
    menu.on_dismissed(move || close_menu_later(weak_menu.clone(), &menu_gen_dismiss));

    // ── VDM Download File Info Dialog Helpers & Callbacks ──
    fn update_dialog_for_file(d: &DownloadInfoDialog, filename: &str) {
        let cat = detect_category_name(filename);
        let folder = get_category_folder(&cat);
        let save_as = if !filename.is_empty() { format!("{}\\{}", folder, filename) } else { "".into() };
        let file_type = detect_file_type(filename);

        d.set_filename(filename.into());
        d.set_category(cat.into());
        d.set_folder(folder.into());
        d.set_save_as(save_as.into());
        d.set_file_type(file_type.into());

        if let Some(img) = engine::sys_icon::get_file_icon_image(filename) {
            d.set_file_icon(img);
            d.set_has_sys_icon(true);
        } else {
            d.set_has_sys_icon(false);
        }
    }

    fn spawn_probe_for_dialog(
        weak_dialog: slint::Weak<DownloadInfoDialog>,
        url: String,
        headers: HashMap<String, String>,
    ) {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return;
        }
        if let Some(d) = weak_dialog.upgrade() {
            d.set_is_probing(true);
        }
        tokio::spawn(async move {
            // one shared client instead of a fresh pool per keystroke
            static PROBE_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
            let client = PROBE_CLIENT.get_or_init(|| {
                reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(8))
                    .redirect(reqwest::redirect::Policy::limited(10))
                    .build()
                    .unwrap_or_default()
            });

            if let Ok(p) = engine::probe::probe(client, &url, &headers).await {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(d) = weak_dialog.upgrade() {
                        d.set_is_probing(false);
                        if let Some(ref better) = p.filename_hint {
                            let cur_name: String = d.get_filename().into();
                            if cur_name.is_empty()
                                || !cur_name.contains('.')
                                || cur_name == "main"
                                || cur_name == "master"
                                || cur_name.starts_with("download")
                                || cur_name.starts_with("main.")
                                || cur_name.starts_with("master.")
                            {
                                update_dialog_for_file(&d, better);
                            }
                        }
                        if let Some(total) = p.total {
                            d.set_file_size_text(fmt_size(total).into());
                        }
                    }
                });
            } else {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(d) = weak_dialog.upgrade() {
                        d.set_is_probing(false);
                    }
                });
            }
        });
    }

    fn open_torrent_select_dialog(
        torrent_dialog: &TorrentFileSelectDialog,
        main_window: Option<&slint::Window>,
        manager: Arc<Manager>,
        magnet_url: String,
        initial_folder: Option<String>,
    ) {
        let folder = initial_folder.unwrap_or_else(|| {
            dirs::download_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("Torrents")
                .to_string_lossy()
                .to_string()
        });

        let magnet_info = engine::probe::parse_magnet(&magnet_url);
        let torrent_name = magnet_info
            .as_ref()
            .and_then(|m| m.display_name.clone())
            .unwrap_or_else(|| "Torrent Package".to_string());
        let total_size_str = magnet_info
            .as_ref()
            .and_then(|m| m.total_size)
            .map(fmt_size)
            .unwrap_or_else(|| "—".to_string());

        torrent_dialog.set_url(magnet_url.clone().into());
        torrent_dialog.set_torrent_name(torrent_name.into());
        torrent_dialog.set_folder(folder.into());
        torrent_dialog.set_total_size_text(total_size_str.clone().into());
        torrent_dialog.set_selected_size_text(total_size_str.into());
        torrent_dialog.set_selected_count(0);
        torrent_dialog.set_total_files_count(0);
        torrent_dialog.set_is_fetching_meta(true);
        torrent_dialog.set_meta_status_text("Connecting to DHT swarm & reading torrent files...".into());
        torrent_dialog.set_files(ModelRc::new(VecModel::default()));

        let _ = torrent_dialog.show();
        center_dialog(main_window, torrent_dialog.window(), 620.0, 480.0);
        torrent_dialog.window().with_winit_window(|win| {
            win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
            win.set_visible(true);
            win.focus_window();
            win.request_user_attention(Some(i_slint_backend_winit::winit::window::UserAttentionType::Critical));
        });

        let weak_dialog = torrent_dialog.as_weak();
        let m_torrent = manager.clone();
        tokio::spawn(async move {
            let res = match m_torrent.ensure_torrent_engine_async().await {
                Ok(t_engine) => t_engine.fetch_torrent_files(&magnet_url).await,
                Err(e) => Err(e),
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(d) = weak_dialog.upgrade() {
                    match res {
                        Ok((name, total_bytes, files)) => {
                            d.set_torrent_name(name.into());
                            let size_str = if total_bytes > 0 { fmt_size(total_bytes) } else { "—".to_string() };
                            d.set_total_size_text(size_str.clone().into());
                            d.set_selected_size_text(size_str.into());
                            d.set_total_files_count(files.len() as i32);
                            d.set_selected_count(files.len() as i32);
                            d.set_is_fetching_meta(false);

                            let items: Vec<TorrentFileItem> = files
                                .into_iter()
                                .map(|f| TorrentFileItem {
                                    id: f.id as i32,
                                    name: f.name.into(),
                                    size_text: if f.size_bytes > 0 { fmt_size(f.size_bytes).into() } else { "—".into() },
                                    size_bytes: f.size_bytes as f32,
                                    selected: true,
                                    file_type: f.file_type.into(),
                                })
                                .collect();
                            d.set_files(ModelRc::new(VecModel::from(items)));
                        }
                        Err(e) => {
                            d.set_is_fetching_meta(false);
                            d.set_meta_status_text(format!("Could not fetch file list: {}", e).into());
                        }
                    }
                }
            });
        });
    }

    // ── Torrent File Select Dialog Callbacks ──
    let weak_t_dialog = torrent_dialog.as_weak();
    torrent_dialog.on_toggle_file(move |file_id| {
        if let Some(d) = weak_t_dialog.upgrade() {
            let model = d.get_files();
            let count = model.row_count();
            let mut total_selected_bytes = 0.0f64;
            let mut selected_count = 0i32;

            if let Some(vm) = model.as_any().downcast_ref::<VecModel<TorrentFileItem>>() {
                for i in 0..count {
                    if let Some(mut item) = vm.row_data(i) {
                        if item.id == file_id {
                            item.selected = !item.selected;
                            vm.set_row_data(i, item.clone());
                        }
                        if item.selected {
                            selected_count += 1;
                            total_selected_bytes += item.size_bytes as f64;
                        }
                    }
                }
            }
            d.set_selected_count(selected_count);
            d.set_selected_size_text(fmt_size(total_selected_bytes as u64).into());
        }
    });

    let weak_t_dialog = torrent_dialog.as_weak();
    torrent_dialog.on_select_all(move |val| {
        if let Some(d) = weak_t_dialog.upgrade() {
            let model = d.get_files();
            let count = model.row_count();
            let mut total_selected_bytes = 0.0f64;
            let mut selected_count = 0i32;

            if let Some(vm) = model.as_any().downcast_ref::<VecModel<TorrentFileItem>>() {
                for i in 0..count {
                    if let Some(mut item) = vm.row_data(i) {
                        item.selected = val;
                        vm.set_row_data(i, item.clone());
                        if val {
                            selected_count += 1;
                            total_selected_bytes += item.size_bytes as f64;
                        }
                    }
                }
            }
            d.set_selected_count(selected_count);
            d.set_selected_size_text(fmt_size(total_selected_bytes as u64).into());
        }
    });

    let weak_t_dialog = torrent_dialog.as_weak();
    let m_t_confirm = manager.clone();
    let p_reg_t_confirm = progress_registry.clone();
    torrent_dialog.on_confirm(move |folder, _extra| {
        if let Some(d) = weak_t_dialog.upgrade() {
            let url: String = d.get_url().into();
            let folder_str: String = folder.into();
            let name_str: String = d.get_torrent_name().into();

            let model = d.get_files();
            let count = model.row_count();
            let mut selected_indices = Vec::new();
            let mut total_bytes = 0u64;

            if let Some(vm) = model.as_any().downcast_ref::<VecModel<TorrentFileItem>>() {
                for i in 0..count {
                    if let Some(item) = vm.row_data(i) {
                        if item.selected {
                            selected_indices.push(item.id as usize);
                            total_bytes += item.size_bytes as u64;
                        }
                    }
                }
            }

            let mut headers = HashMap::new();
            headers.insert(
                "selected_files".to_string(),
                serde_json::to_string(&selected_indices).unwrap_or_default(),
            );

            let folder_opt = if folder_str.trim().is_empty() {
                None
            } else {
                Some(folder_str)
            };

            match m_t_confirm.add_download_with_total(
                url,
                folder_opt,
                Some(name_str),
                headers,
                Some(total_bytes),
            ) {
                Ok(snap) => {
                    p_reg_t_confirm.open_dialog(&snap);
                }
                Err(e) => eprintln!("[VDM] Failed to add torrent download: {e}"),
            }

            let _ = d.hide();
        }
    });

    let weak_t_dialog = torrent_dialog.as_weak();
    torrent_dialog.on_drag_window(move || {
        if let Some(d) = weak_t_dialog.upgrade() {
            d.window().with_winit_window(|win| {
                let _ = win.drag_window();
            });
        }
    });

    let weak_t_dialog = torrent_dialog.as_weak();
    torrent_dialog.on_minimize_window(move || {
        if let Some(d) = weak_t_dialog.upgrade() {
            d.window().set_minimized(true);
        }
    });

    let weak_t_dialog = torrent_dialog.as_weak();
    torrent_dialog.on_closed(move || {
        if let Some(d) = weak_t_dialog.upgrade() {
            let _ = d.hide();
        }
    });

    torrent_dialog.on_pick_folder(move || {
        FileDialog::new()
            .pick_folder()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
            .into()
    });

    let weak_info = info.as_weak();
    let weak_t_add = torrent_dialog.as_weak();
    let weak_add_parent = app.as_weak();
    let m_add = manager.clone();
    app.on_open_add(move || {
        let url = extract_url_from_clipboard().unwrap_or_default();
        if url.starts_with("magnet:?") {
            if let Some(td) = weak_t_add.upgrade() {
                open_torrent_select_dialog(
                    &td,
                    weak_add_parent.upgrade().as_ref().map(|a| a.window()),
                    m_add.clone(),
                    url,
                    None,
                );
            }
            return;
        }

        if let Some(d) = weak_info.upgrade() {
            let filename = if !url.is_empty() {
                engine::probe::url_basename(&url).unwrap_or_default()
            } else {
                "".into()
            };

            d.set_url(url.clone().into());
            update_dialog_for_file(&d, &filename);
            d.set_file_size_text("—".into());
            d.set_description("".into());
            d.set_remember_path(true);
            d.set_start_now(true);
            let _ = d.show();

            let main_app = weak_add_parent.upgrade();
            center_dialog(main_app.as_ref().map(|a| a.window()), d.window(), 540.0, 285.0);

            d.window().with_winit_window(|win| {
                win.set_window_level(i_slint_backend_winit::winit::window::WindowLevel::AlwaysOnTop);
                win.set_visible(true);
                win.focus_window();
                win.request_user_attention(Some(i_slint_backend_winit::winit::window::UserAttentionType::Critical));
            });

            if !url.is_empty() {
                spawn_probe_for_dialog(weak_info.clone(), url, HashMap::new());
            }
        }
    });

    let weak_info = info.as_weak();
    let m = manager.clone();
    let pending_for_confirm = pending_payloads_confirm.clone();
    let progress_reg_confirm = progress_registry.clone();
    info.on_confirm(move |url, filename, folder, _desc, start_now| {
        let (headers, file_size) = pending_for_confirm
            .lock()
            .unwrap()
            .remove(&String::from(url.clone()))
            .unwrap_or_default();
        let folder_opt = {
            let f: String = folder.into();
            if f.trim().is_empty() { None } else { Some(f) }
        };
        let filename_opt = {
            let f: String = filename.into();
            if f.trim().is_empty() { None } else { Some(f) }
        };
        match m.add_download_with_total(String::from(url.clone()), folder_opt.clone(), filename_opt.clone(), headers, file_size) {
            Ok(snap) => {
                if !start_now {
                    let _ = m.pause(&snap.id);
                } else {
                    progress_reg_confirm.open_dialog(&snap);
                }
            }
            Err(e) => eprintln!("[VDM] add_download failed: {e}"),
        }
        if let Some(d) = weak_info.upgrade() {
            d.set_url("".into());
            d.set_filename("".into());
            d.set_save_as("".into());
            d.set_description("".into());
            let _ = d.hide();
        }
    });

    let weak_info_cat = info.as_weak();
    info.on_category_changed(move |cat| {
        if let Some(d) = weak_info_cat.upgrade() {
            let cat_str: String = cat.into();
            let folder = get_category_folder(&cat_str);
            let fname: String = d.get_filename().into();
            d.set_folder(folder.clone().into());
            if !fname.is_empty() {
                d.set_save_as(format!("{}\\{}", folder, fname).into());
            }
        }
    });

    let weak_info_url = info.as_weak();
    let weak_t_url = torrent_dialog.as_weak();
    let weak_app_url = app.as_weak();
    let m_url = manager.clone();
    info.on_url_edited(move |url| {
        let u: String = url.into();
        if u.starts_with("magnet:?") {
            if let Some(d) = weak_info_url.upgrade() {
                let _ = d.hide();
            }
            if let Some(td) = weak_t_url.upgrade() {
                open_torrent_select_dialog(
                    &td,
                    weak_app_url.upgrade().as_ref().map(|a| a.window()),
                    m_url.clone(),
                    u,
                    None,
                );
            }
            return;
        }

        if let Some(d) = weak_info_url.upgrade() {
            let base = engine::probe::url_basename(&u).unwrap_or_default();
            if !base.is_empty() {
                update_dialog_for_file(&d, &base);
            }
            spawn_probe_for_dialog(weak_info_url.clone(), u, HashMap::new());
        }
    });

    let weak_info_fname = info.as_weak();
    info.on_filename_edited(move |fname| {
        if let Some(d) = weak_info_fname.upgrade() {
            let f: String = fname.into();
            let cat = detect_category_name(&f);
            let file_type = detect_file_type(&f);
            d.set_category(cat.into());
            d.set_file_type(file_type.into());
            if let Some(img) = engine::sys_icon::get_file_icon_image(&f) {
                d.set_file_icon(img);
                d.set_has_sys_icon(true);
            } else {
                d.set_has_sys_icon(false);
            }
        }
    });

    let weak_info_drag = info.as_weak();
    info.on_drag_window(move || {
        if let Some(d) = weak_info_drag.upgrade() {
            d.window().with_winit_window(|win| {
                let _ = win.drag_window();
            });
        }
    });

    let weak_info_min = info.as_weak();
    info.on_minimize_window(move || {
        if let Some(d) = weak_info_min.upgrade() {
            d.window().set_minimized(true);
        }
    });

    info.on_pick_folder(move || {
        FileDialog::new()
            .pick_folder()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
            .into()
    });

    let weak_info = info.as_weak();
    let pending_for_closed = pending_payloads.clone();
    info.on_closed(move || {
        if let Some(d) = weak_info.upgrade() {
            // drop the staged intercept payload so dismissed dialogs don't leak it
            pending_for_closed.lock().unwrap().remove(&String::from(d.get_url()));
            let _ = d.hide();
        }
    });

    // ── IDM-style Download complete dialog ──
    let completed_queue: Arc<Mutex<VecDeque<CompletedTaskInfo>>> = Arc::new(Mutex::new(VecDeque::new()));
    let is_complete_dialog_showing: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    let m = manager.clone();
    let weak_done_open = done.as_weak();
    let q_open = completed_queue.clone();
    let is_showing_open = is_complete_dialog_showing.clone();
    let weak_app_open = app.as_weak();
    done.on_open_file(move |id| {
        if let Some(d) = weak_done_open.upgrade() {
            let _ = d.hide();
            let app_opt = weak_app_open.upgrade();
            show_next_completed_dialog(&q_open, &d, app_opt.as_ref().map(|a| a.window()), &is_showing_open);
        }
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("explorer").arg(&path).spawn();
            }
        }
    });

    let m = manager.clone();
    let weak_done_with = done.as_weak();
    let q_with = completed_queue.clone();
    let is_showing_with = is_complete_dialog_showing.clone();
    let weak_app_with = app.as_weak();
    done.on_open_with(move |id| {
        if let Some(d) = weak_done_with.upgrade() {
            let _ = d.hide();
            let app_opt = weak_app_with.upgrade();
            show_next_completed_dialog(&q_with, &d, app_opt.as_ref().map(|a| a.window()), &is_showing_with);
        }
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("rundll32.exe")
                    .args(["shell32.dll,OpenAs_RunDLL", &path.to_string_lossy().to_string()])
                    .spawn();
            }
        }
    });

    let m = manager.clone();
    let weak_done_fld = done.as_weak();
    let q_fld = completed_queue.clone();
    let is_showing_fld = is_complete_dialog_showing.clone();
    let weak_app_fld = app.as_weak();
    done.on_open_folder(move |id| {
        if let Some(d) = weak_done_fld.upgrade() {
            let _ = d.hide();
            let app_opt = weak_app_fld.upgrade();
            show_next_completed_dialog(&q_fld, &d, app_opt.as_ref().map(|a| a.window()), &is_showing_fld);
        }
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("explorer")
                    .arg(format!("/select,{}", path.to_string_lossy()))
                    .spawn();
            } else if let Some(parent) = path.parent() {
                let _ = std::process::Command::new("explorer").arg(parent).spawn();
            }
        }
    });

    let m = manager.clone();
    let weak_done_drag_file = done.as_weak();
    let q_drag_file = completed_queue.clone();
    let is_showing_drag_file = is_complete_dialog_showing.clone();
    let weak_app_drag_file = app.as_weak();
    done.on_start_drag(move |id| {
        if let Some(d) = weak_done_drag_file.upgrade() {
            let _ = d.hide();
            let app_opt = weak_app_drag_file.upgrade();
            show_next_completed_dialog(&q_drag_file, &d, app_opt.as_ref().map(|a| a.window()), &is_showing_drag_file);
        }
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("explorer")
                    .arg(format!("/select,{}", path.to_string_lossy()))
                    .spawn();
            } else if let Some(parent) = path.parent() {
                let _ = std::process::Command::new("explorer").arg(parent).spawn();
            }
        }
    });

    let weak_done_drag = done.as_weak();
    done.on_drag_window(move || {
        if let Some(d) = weak_done_drag.upgrade() {
            d.window().with_winit_window(|win| {
                let _ = win.drag_window();
            });
        }
    });

    let weak_done = done.as_weak();
    let q_closed = completed_queue.clone();
    let is_showing_closed = is_complete_dialog_showing.clone();
    let weak_app_closed = app.as_weak();
    done.on_closed(move || {
        if let Some(d) = weak_done.upgrade() {
            let _ = d.hide();
            let app_opt = weak_app_closed.upgrade();
            show_next_completed_dialog(&q_closed, &d, app_opt.as_ref().map(|a| a.window()), &is_showing_closed);
        }
    });

    // ---- background poller: refresh download list every 250ms ----
    let weak = app.as_weak();
    let selected_ids_for_poll = selected_ids.clone();
    let progress_registry_for_poll = progress_registry.clone();
    let _weak_pill_poll = mini_pill.as_weak();

    // completed tasks known at boot (no popup for old history)
    let completed_seen: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(
        manager_cloned_for_poll
            .list_downloads()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.status == "completed")
            .map(|s| s.id)
            .collect(),
    ));
    let completed_seen = completed_seen.clone();
    let completed_queue_for_poll = completed_queue.clone();
    let is_complete_dialog_showing_poll = is_complete_dialog_showing.clone();
    let weak_done_poll = done.as_weak();
    let weak_shutdown_poll = shutdown_dialog.as_weak();
    let countdown_active_poll = countdown_active.clone();
    let countdown_seconds_poll = countdown_seconds.clone();
    let had_active_downloads_in_session_poll = had_active_downloads_in_session.clone();
    let is_shutting_down_poller = is_shutting_down.clone();

    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        if is_shutting_down_poller.load(Ordering::Relaxed) {
            break;
        }
        let snaps = manager_cloned_for_poll.list_downloads().unwrap_or_default();

        let busy = snaps.iter().any(|s| matches!(s.status.as_str(), "downloading" | "connecting" | "queued" | "processing"));
        if busy {
            had_active_downloads_in_session_poll.store(true, Ordering::Relaxed);
        }

        // Trigger safe shutdown countdown ONLY if active downloads were running in this session and just finished
        if SHUTDOWN_ARMED.load(Ordering::Relaxed) && !busy && had_active_downloads_in_session_poll.load(Ordering::Relaxed) {
            if SHUTDOWN_ARMED.swap(false, Ordering::SeqCst) {
                had_active_downloads_in_session_poll.store(false, Ordering::Relaxed);
                let weak_sd = weak_shutdown_poll.clone();
                let weak_app_sd = weak.clone();
                let act_sd = countdown_active_poll.clone();
                let sec_sd = countdown_seconds_poll.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(sd) = weak_sd.upgrade() {
                        let app_opt = weak_app_sd.upgrade();
                        start_shutdown_countdown(
                            &sd,
                            app_opt.as_ref().map(|a| a.window()),
                            &act_sd,
                            &sec_sd,
                            weak_sd.clone(),
                        );
                    }
                });
            }
        }

        // IDM-style: fresh completions push into completion popup queue
        let mut new_completions: Vec<CompletedTaskInfo> = Vec::new();
        {
            let mut seen = completed_seen.lock().unwrap();
            for s in &snaps {
                if s.status == "completed" && seen.insert(s.id.clone()) {
                    let loc = manager_cloned_for_poll
                        .get_task_path(&s.id)
                        .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
                        .unwrap_or_default();
                    new_completions.push(CompletedTaskInfo {
                        id: s.id.clone(),
                        filename: s.filename.clone(),
                        total: s.total.unwrap_or(0),
                        location: loc,
                        url: s.url.clone(),
                    });
                }
            }
        }

        if !new_completions.is_empty() {
            let mut q = completed_queue_for_poll.lock().unwrap();
            for item in new_completions {
                q.push_back(item);
            }
        }

        let should_show_done = !completed_queue_for_poll.lock().unwrap().is_empty()
            && !is_complete_dialog_showing_poll.load(Ordering::Relaxed);
        if should_show_done {
            let q_poll = completed_queue_for_poll.clone();
            let is_showing_poll = is_complete_dialog_showing_poll.clone();
            let weak_done = weak_done_poll.clone();
            let weak_app = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(d) = weak_done.upgrade() {
                    let app_opt = weak_app.upgrade();
                    show_next_completed_dialog(&q_poll, &d, app_opt.as_ref().map(|a| a.window()), &is_showing_poll);
                }
            });
        }

        // IDM-style: Update all active download progress boxes simultaneously
        let active_dialogs: Vec<(String, slint::Weak<DownloadProgressDialog>)> = {
            progress_registry_for_poll
                .dialogs
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };

        for (tid, weak_p) in active_dialogs {
            if let Some(s) = snaps.iter().find(|x| x.id == tid) {
                let is_torrent = s.url.starts_with("magnet:?");
                let t_stats = if is_torrent {
                    manager_cloned_for_poll.get_torrent_engine().and_then(|t| t.get_stats(&s.id))
                } else {
                    None
                };
                let peers_text: String = if let Some(ref ts) = t_stats {
                    format!("{} peers ({} seeds)", ts.live_peers, ts.live_seeds)
                } else {
                    "—".into()
                };

                let is_checking_files = is_torrent && t_stats.as_ref().map(|ts| ts.state_kind == "initializing").unwrap_or(false);
                let is_processing = s.status == "processing" || is_checking_files;
                let pct = if let Some(tot) = s.total {
                    if tot > 0 { (s.downloaded as f32 / tot as f32).min(1.0) } else { 0.0 }
                } else { 0.0 };
                let pct_str = format!("{:.0}%", pct * 100.0);
                let speed_str = if is_checking_files {
                    "Checking disk...".into()
                } else if is_processing {
                    "Processing...".into()
                } else {
                    fmt_speed(s.speed_bps)
                };
                let eta_str = if is_checking_files || (is_processing && !is_torrent) {
                    "—".into()
                } else if s.speed_bps == 0 {
                    "—".into()
                } else {
                    fmt_eta(s.eta_secs)
                };
                let dl_str = fmt_size(s.downloaded);
                let tot_str = s.total.map(fmt_size).unwrap_or_else(|| "—".into());
                let is_p = s.status == "paused";
                let is_err = s.status == "error";
                let is_done = s.status == "completed";
                let st_text: String = match s.status.as_str() {
                    "processing" => {
                        if is_checking_files {
                            "Verifying existing files on disk...".into()
                        } else {
                            "Merging audio and video...".into()
                        }
                    },
                    "downloading" => {
                        if is_torrent {
                            if let Some(ref ts) = t_stats {
                                if ts.state_kind == "initializing" {
                                    "Verifying existing files on disk...".into()
                                } else if ts.live_peers == 0 {
                                    "Connecting to swarm / DHT...".into()
                                } else {
                                    format!("Downloading from swarm ({} peers)...", ts.live_peers)
                                }
                            } else {
                                "Connecting to swarm...".into()
                            }
                        } else if s.downloaded >= s.total.unwrap_or(u64::MAX) && s.total.unwrap_or(0) > 0 {
                            "Finalizing download...".into()
                        } else {
                            "Receiving data...".into()
                        }
                    },
                    "paused" => "Paused".into(),
                    "connecting" => {
                        if is_torrent {
                            if is_checking_files {
                                "Verifying existing files on disk...".into()
                            } else {
                                "Connecting to swarm / DHT...".into()
                            }
                        } else {
                            "Connecting...".into()
                        }
                    },
                    "completed" => "Complete".into(),
                    "error" => {
                        if let Some(err) = &s.error_msg {
                            format!("Error: {err}")
                        } else {
                            "Error".into()
                        }
                    }
                    _ => s.status.clone(),
                };
                let raw_st = s.status.clone();
                let resume_cap = if is_err {
                    "No"
                } else {
                    "Yes"
                };

                let mut chunk_items = Vec::new();
                if !s.segments.is_empty() {
                    for seg in &s.segments {
                        let is_chunk_done = seg.end > seg.start && seg.done >= (seg.end - seg.start);
                        let status_text = if is_chunk_done || s.status == "completed" || is_processing {
                            "Finished"
                        } else if s.status == "downloading" {
                            "Receiving data..."
                        } else if s.status == "paused" {
                            "Paused"
                        } else {
                            "Connecting..."
                        };
                        chunk_items.push(ChunkInfo {
                            id: (seg.idx + 1) as i32,
                            downloaded: fmt_size(seg.done).into(),
                            status: status_text.into(),
                        });
                    }
                } else {
                    let count = manager_cloned_for_poll.max_conns.load(std::sync::atomic::Ordering::Relaxed).max(1);
                    let part_size = if count > 0 { s.downloaded / count } else { 0 };
                    for i in 1..=count {
                        let is_active = (s.status == "downloading" || s.status == "connecting" || s.status == "processing") && i <= 8;
                        let status_text = if s.status == "completed" || is_processing {
                            "Finished"
                        } else if is_active {
                            "Receiving data..."
                        } else if s.status == "paused" {
                            "Paused"
                        } else {
                            "Standby"
                        };
                        chunk_items.push(ChunkInfo {
                            id: i as i32,
                            downloaded: fmt_size(part_size).into(),
                            status: status_text.into(),
                        });
                    }
                }

                let fname = s.filename.clone();
                let dl_url = s.url.clone();
                let dl_id = s.id.clone();
                let dl_dir = s.dir.clone();
                let reg_map = progress_registry_for_poll.dialogs.clone();
                let tid_copy = tid.clone();
                let wp = weak_p.clone();

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(p) = wp.upgrade() {
                        p.set_task_id(dl_id.into());
                        p.set_filename(fname.into());
                        p.set_url(dl_url.into());
                        p.set_save_path(dl_dir.into());
                        p.set_progress(pct);
                        p.set_percent_text(pct_str.into());
                        p.set_downloaded_size(dl_str.into());
                        p.set_total_size(tot_str.into());
                        p.set_transfer_rate(speed_str.into());
                        p.set_time_left(eta_str.into());
                        p.set_is_paused(is_p);
                        p.set_is_error(is_err);
                        p.set_is_torrent(is_torrent);
                        p.set_peers_info(peers_text.into());
                        p.set_raw_status(raw_st.into());
                        p.set_resume_capable(resume_cap.into());
                        p.set_status_text(st_text.into());
                        let old_chunks = p.get_chunks();
                        if let Some(cm) = old_chunks.as_any().downcast_ref::<VecModel<ChunkInfo>>() {
                            if cm.row_count() == chunk_items.len() {
                                for (i, c) in chunk_items.into_iter().enumerate() {
                                    if cm.row_data(i).map(|old| !chunk_eq(&old, &c)).unwrap_or(true) {
                                        cm.set_row_data(i, c);
                                    }
                                }
                            } else {
                                p.set_chunks(ModelRc::new(VecModel::from(chunk_items)));
                            }
                        } else {
                            p.set_chunks(ModelRc::new(VecModel::from(chunk_items)));
                        }

                        if is_done {
                            let _ = p.hide();
                            reg_map.lock().unwrap().remove(&tid_copy);
                        }
                    } else {
                        reg_map.lock().unwrap().remove(&tid_copy);
                    }
                });
            } else {
                let wp = weak_p.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(p) = wp.upgrade() {
                        let _ = p.hide();
                    }
                });
                progress_registry_for_poll.dialogs.lock().unwrap().remove(&tid);
            }
        }

        let sel_guard = selected_ids_for_poll.lock().unwrap();

        // Zero-allocation fingerprint: skip UI update entirely when nothing changed
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        for s in &snaps {
            s.id.hash(&mut hasher);
            s.status.hash(&mut hasher);
            s.downloaded.hash(&mut hasher);
            s.total.unwrap_or(0).hash(&mut hasher);
            s.speed_bps.hash(&mut hasher);
            s.filename.hash(&mut hasher);
            sel_guard.contains(&s.id).hash(&mut hasher);
        }
        filter_for_poll.lock().unwrap().hash(&mut hasher);
        search_for_poll.lock().unwrap().hash(&mut hasher);
        sort_col_for_poll.lock().unwrap().hash(&mut hasher);
        sort_asc_for_poll.lock().unwrap().hash(&mut hasher);
        let state_sig = hasher.finish();
        {
            let mut last = last_sig_for_poll.lock().unwrap();
            if *last == state_sig {
                continue;
            }
            *last = state_sig;
        }

        // Calculate aggregate metrics across all tasks
        let total_count = snaps.len() as i32;
        let downloading_count = snaps.iter().filter(|s| s.status == "downloading" || s.status == "connecting" || s.status == "processing").count() as i32;
        let completed_count = snaps.iter().filter(|s| s.status == "completed").count() as i32;
        let paused_count = snaps.iter().filter(|s| s.status == "paused").count() as i32;
        let queued_count = snaps.iter().filter(|s| s.status == "queued").count() as i32;
        let error_count = snaps.iter().filter(|s| s.status == "error").count() as i32;

        // IDM Category Counts
        let count_compressed = snaps.iter().filter(|s| detect_category(&s.filename) == "compressed").count() as i32;
        let count_programs = snaps.iter().filter(|s| detect_category(&s.filename) == "installer").count() as i32;
        let count_video = snaps.iter().filter(|s| detect_category(&s.filename) == "video").count() as i32;
        let count_music = snaps.iter().filter(|s| detect_category(&s.filename) == "audio").count() as i32;
        let count_documents = snaps.iter().filter(|s| detect_category(&s.filename) == "document").count() as i32;

        let total_speed: u64 = snaps
            .iter()
            .filter(|s| s.status == "downloading")
            .map(|s| s.speed_bps)
            .sum();
        let total_speed_text = fmt_speed(total_speed);

        // Apply sidebar active filter & search query
        let filt = filter_for_poll.lock().unwrap().clone();
        let qry = search_for_poll.lock().unwrap().to_lowercase();
        let cur_sort_col = sort_col_for_poll.lock().unwrap().clone();
        let cur_sort_asc = *sort_asc_for_poll.lock().unwrap();

        let mut filtered: Vec<TaskSnapshot> = snaps
            .into_iter()
            .filter(|s| {
                if filt.starts_with("cat:") {
                    let target_cat = &filt[4..];
                    if detect_category(&s.filename) != target_cat {
                        return false;
                    }
                } else if filt != "All" {
                    if filt == "Downloading" && !(s.status == "downloading" || s.status == "connecting" || s.status == "processing") {
                        return false;
                    }
                    if filt != "Downloading" && !s.status.eq_ignore_ascii_case(&filt) {
                        return false;
                    }
                }
                if !qry.is_empty() {
                    let hay = format!("{} {}", s.filename, s.url).to_lowercase();
                    if !hay.contains(&qry) {
                        return false;
                    }
                }
                true
            })
            .collect();

        filtered.sort_by(|a, b| {
            let ordering = match cur_sort_col.as_str() {
                "filename" => a.filename.to_lowercase().cmp(&b.filename.to_lowercase()),
                "size" => a.total.unwrap_or(a.downloaded).cmp(&b.total.unwrap_or(b.downloaded)),
                "progress" => {
                    let pa = if a.status == "completed" { 100.0 } else { a.total.map(|t| if t == 0 { 0.0 } else { a.downloaded as f32 / t as f32 * 100.0 }).unwrap_or(0.0) };
                    let pb = if b.status == "completed" { 100.0 } else { b.total.map(|t| if t == 0 { 0.0 } else { b.downloaded as f32 / t as f32 * 100.0 }).unwrap_or(0.0) };
                    pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                }
                "status" => a.status.cmp(&b.status),
                "speed" => a.speed_bps.cmp(&b.speed_bps),
                "eta" => a.eta_secs.unwrap_or(u64::MAX).cmp(&b.eta_secs.unwrap_or(u64::MAX)),
                "date" | _ => a.created_at.cmp(&b.created_at),
            };
            if cur_sort_asc {
                ordering
            } else {
                ordering.reverse()
            }
        });

        let mut first_id = String::new();
        let mut first_name = String::new();
        let items: Vec<DownloadItem> = filtered
            .iter()
            .map(|s| {
                let is_sel = sel_guard.contains(&s.id);
                if is_sel && first_id.is_empty() {
                    first_id = s.id.clone();
                    first_name = s.filename.clone();
                }
                snapshot_to_item(s, is_sel)
            })
            .collect();
        let sel_count = sel_guard.len() as i32;

        let weak2 = weak.clone();
        let _ = weak2.upgrade_in_event_loop(move |app| {
            // self-heal maximized state: Win+Up / drag-restore / snap bypass
            // the maximize button, leaving the property (corners, resize
            // handles) stale — re-sync it every tick
            let max_now = app.window().is_maximized();
            if max_now != app.get_is_maximized() {
                app.set_is_maximized(max_now);
            }
            apply_items(&app, items);
            app.set_selected_count(sel_count);
            app.set_first_selected_id(first_id.into());
            app.set_first_selected_filename(first_name.into());
            app.set_total_speed(total_speed as f32);
            app.set_total_speed_text(total_speed_text.into());
            app.set_total_count(total_count);
            app.set_downloading_count(downloading_count);
            app.set_completed_count(completed_count);
            app.set_paused_count(paused_count);
            app.set_queued_count(queued_count);
            app.set_error_count(error_count);

            app.set_count_compressed(count_compressed);
            app.set_count_programs(count_programs);
            app.set_count_video(count_video);
            app.set_count_music(count_music);
            app.set_count_documents(count_documents);
        });
    });

    // initial paint before loop kicks
    {
        let mut snaps = manager.list_downloads().unwrap_or_default();
        snaps.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let items: Vec<DownloadItem> = snaps.iter().map(|s| snapshot_to_item(s, false)).collect();
        app.set_downloads(ModelRc::new(VecModel::from(items)));
    }

    if !is_autostart {
        app.show().context("show window")?;
    } else {
        println!("[VDM] Started silently in background / system tray via startup.");
        trim_working_set();
    }
    // run until explicit quit (close hides to tray; the tray outlives hidden windows)
    slint::run_event_loop_until_quit().context("slint run")?;
    // keep runtime alive until window closes (guard dropped here)
    drop(_guard);
    Ok(())
}
