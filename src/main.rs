use anyhow::Context;
use i_slint_backend_winit::WinitWindowAccessor;
use rfd::FileDialog;
use slint::{Model, ModelRc, VecModel};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI32, Ordering};
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
    if let Some(parent) = parent_opt {
        center_over_main(parent, win, w, h);
    } else {
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

fn position_pill_bottom_right(win: &slint::Window, w: f64, h: f64) {
    win.with_winit_window(|winit_win| {
        if let Some(monitor) = winit_win.current_monitor().or_else(|| winit_win.primary_monitor()) {
            let scale = monitor.scale_factor();
            let mon_size = monitor.size();
            let mon_pos = monitor.position();
            let x = mon_pos.x as f64 + (mon_size.width as f64 - (w + 24.0) * scale);
            let y = mon_pos.y as f64 + (mon_size.height as f64 - (h + 50.0) * scale);
            let _ = winit_win.set_outer_position(i_slint_backend_winit::winit::dpi::PhysicalPosition::new(
                x as i32,
                y as i32,
            ));
        }
    });
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

fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return "—".to_string();
    }
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
            map_close.lock().unwrap().remove(&tid_close);
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
            map_canc.lock().unwrap().remove(&tid_canc);
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
    // ── Single-Instance Check ──
    // If VDM is already running, notify it to bring its window to front and exit.
    if let Ok(mut stream) = std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", engine::server::DEFAULT_SERVER_PORT).parse().unwrap(),
        Duration::from_millis(250),
    ) {
        use std::io::Write;
        let _ = stream.write_all(b"GET /show HTTP/1.1\r\nHost: 127.0.0.1:9191\r\nConnection: close\r\n\r\n");
        println!("[VDM] Active instance running on :{} — brought window to front.", engine::server::DEFAULT_SERVER_PORT);
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
    let renew_dialog = RenewLinkDialog::new().context("renew dialog")?;
    let duplicate_dialog = DuplicateDownloadDialog::new().context("duplicate dialog")?;
    let mini_pill = DownloadMiniPill::new().context("mini pill")?;
    let done = CompleteDialog::new().context("complete dialog")?;
    let active_renewing_task: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let progress_registry = Arc::new(ProgressRegistry::new(
        manager.clone(),
        app.as_weak(),
        renew_dialog.as_weak(),
        active_renewing_task.clone(),
    ));

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

    // shared filter / search / sort state (polled by background thread)
    let filter = Arc::new(Mutex::new(String::from("All")));
    let search = Arc::new(Mutex::new(String::new()));
    let sort_col = Arc::new(Mutex::new(String::from("date")));
    let sort_asc = Arc::new(Mutex::new(false));
    let last_sig = Arc::new(Mutex::new(String::new()));
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

    // close hides to tray — real quit lives in the tray menu
    let weak_close = app.as_weak();
    app.on_close_window(move || {
        if let Some(app) = weak_close.upgrade() {
            app.window().with_winit_window(|win| {
                win.set_visible(false);
            });
            let _ = app.hide();
        }
    });

    let weak_min = app.as_weak();
    app.on_minimize_window(move || {
        if let Some(app) = weak_min.upgrade() {
            app.window().set_minimized(true);
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
    app.on_remove_download(move |id, delete_file| {
        let s_id = String::from(id);
        let _ = m.remove(&s_id, delete_file);
        selected_ids_single_rem.lock().unwrap().remove(&s_id);
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
        a.set_selected_count(sel.len() as i32);
        a.set_first_selected_id(first_id.into());
        a.set_first_selected_filename(first_name.into());
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

    app.on_copy_url(move |url| {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(String::from(url));
        }
    });

    let settings = SettingsDialog::new().context("settings dialog")?;
    settings.set_speed_limit_mbps(init_speed_mbps);
    settings.set_max_connections(init_max_conns);
    settings.set_max_active(init_max_active);

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
        *l_f.lock().unwrap() = String::new();
    });

    let s = search.clone();
    let l_s = last_sig.clone();
    app.on_search_changed(move |v| {
        *s.lock().unwrap() = String::from(v);
        *l_s.lock().unwrap() = String::new();
    });

    let sc = sort_col.clone();
    let sa = sort_asc.clone();
    let l_sc = last_sig.clone();
    app.on_sort_changed(move |col, asc| {
        *sc.lock().unwrap() = String::from(col);
        *sa.lock().unwrap() = asc;
        *l_sc.lock().unwrap() = String::new();
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
                    .args(["/select,", &path.to_string_lossy().to_string()])
                    .spawn();
            } else if let Some(parent) = path.parent() {
                let _ = std::process::Command::new("explorer").arg(parent).spawn();
            }
        }
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
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            if let Ok(text) = clipboard.get_text() {
                let trimmed = text.trim();
                if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    if let Some(d) = weak_ren_paste.upgrade() {
                        d.set_new_url(trimmed.into());
                    }
                }
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
    duplicate_dialog.on_closed(move || {
        if let Some(d) = weak_dup_close.upgrade() {
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
    let weak_app_server = app.as_weak();
    let weak_ren_server = renew_dialog.as_weak();
    let weak_dup_server = duplicate_dialog.as_weak();
    let active_ren_server = active_renewing_task.clone();
    let m_server = manager.clone();
    let weak_settings_state = settings.as_weak();
    let progress_reg_server = progress_registry.clone();

    tokio::spawn(async move {
        while let Some(event) = download_rx.recv().await {
            let wi = weak_info_server.clone();
            let wa = weak_app_server.clone();
            let wren = weak_ren_server.clone();
            let wdup = weak_dup_server.clone();
            let act_ren = active_ren_server.clone();
            let ws = weak_settings_state.clone();
            let m = m_server.clone();
            let ps = pending_payloads_server.clone();
            let p_reg = progress_reg_server.clone();

            let _ = slint::invoke_from_event_loop(move || {
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
                                    center_dialog(main_app.as_ref().map(|a| a.window()), dup.window(), 540.0, if is_active { 320.0 } else { 380.0 });

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

    std::thread::spawn(move || {
        let rx = tray_icon::TrayIconEvent::receiver();
        while let Ok(ev) = rx.recv() {
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
                    let _ = slint::invoke_from_event_loop(move || {
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
                    let _ = slint::invoke_from_event_loop(move || {
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
                            std::thread::spawn(move || {
                                let start = std::time::Instant::now();
                                loop {
                                    std::thread::sleep(Duration::from_millis(25));

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

    let weak_menu = menu.as_weak();
    let weak_app = app.as_weak();
    let m = manager.clone();
    let menu_gen_item = menu_gen.clone();
    menu.on_item(move |what| {
        let what: String = what.into();
        close_menu_later(weak_menu.clone(), &menu_gen_item);
        match what.as_str() {
            "open" => {
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
            "pause-all" => m.pause_all(),
            "resume-all" => m.resume_all(),
            "quit" => {
                let _ = slint::quit_event_loop();
            }
            _ => {}
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
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
                .unwrap_or_default();

            if let Ok(p) = engine::probe::probe(&client, &url, &headers).await {
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

    let weak_info = info.as_weak();
    let weak_add_parent = app.as_weak();
    app.on_open_add(move || {
        if let Some(d) = weak_info.upgrade() {
            let clip = arboard::Clipboard::new()
                .ok()
                .and_then(|mut c| c.get_text().ok())
                .unwrap_or_default();
            let clip = clip.trim().to_string();
            let url = if clip.starts_with("http://") || clip.starts_with("https://") {
                clip
            } else {
                "".into()
            };
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
    info.on_url_edited(move |url| {
        if let Some(d) = weak_info_url.upgrade() {
            let u: String = url.into();
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
    let m = manager.clone();
    let weak_done_open = done.as_weak();
    done.on_open_file(move |id| {
        if let Some(d) = weak_done_open.upgrade() {
            let _ = d.hide();
        }
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("explorer").arg(&path).spawn();
            }
        }
    });

    let m = manager.clone();
    let weak_done_with = done.as_weak();
    done.on_open_with(move |id| {
        if let Some(d) = weak_done_with.upgrade() {
            let _ = d.hide();
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
    done.on_open_folder(move |id| {
        if let Some(d) = weak_done_fld.upgrade() {
            let _ = d.hide();
        }
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("explorer")
                    .args(["/select,", &path.to_string_lossy().to_string()])
                    .spawn();
            } else if let Some(parent) = path.parent() {
                let _ = std::process::Command::new("explorer").arg(parent).spawn();
            }
        }
    });

    let m = manager.clone();
    let weak_done_drag_file = done.as_weak();
    done.on_start_drag(move |id| {
        if let Some(d) = weak_done_drag_file.upgrade() {
            let _ = d.hide();
        }
        if let Some(path) = m.get_task_path(&String::from(id)) {
            if path.exists() {
                let _ = std::process::Command::new("explorer")
                    .args(["/select,", &path.to_string_lossy().to_string()])
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
    done.on_closed(move || {
        if let Some(d) = weak_done.upgrade() {
            let _ = d.hide();
        }
    });

    // ---- background poller: refresh download list every 250ms ----
    let weak = app.as_weak();
    let selected_ids_for_poll = selected_ids.clone();
    let progress_registry_for_poll = progress_registry.clone();
    let weak_pill_poll = mini_pill.as_weak();

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
    let weak_done_poll = done.as_weak();

    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        let snaps = manager_cloned_for_poll.list_downloads().unwrap_or_default();

        let sel_guard = selected_ids_for_poll.lock().unwrap();

        // cheap fingerprint: skip UI update entirely when nothing changed
        let mut fp = String::with_capacity(snaps.len() * 48);
        for s in &snaps {
            fp.push_str(&format!("{}|{}|{}|{}|{}|{}|{};", s.id, s.status, s.downloaded, s.total.unwrap_or(0), s.speed_bps, s.filename, sel_guard.contains(&s.id)));
        }
        let cur_sort_c = sort_col_for_poll.lock().unwrap().clone();
        let cur_sort_a = *sort_asc_for_poll.lock().unwrap();
        let state_sig = format!("{}|{}|{}|{}|{}", fp, filter_for_poll.lock().unwrap().clone(), search_for_poll.lock().unwrap().clone(), cur_sort_c, cur_sort_a);
        {
            let mut last = last_sig_for_poll.lock().unwrap();
            if *last == state_sig {
                continue;
            }
            *last = state_sig;
        }

        // Calculate aggregate metrics across all tasks
        let total_count = snaps.len() as i32;
        let downloading_count = snaps.iter().filter(|s| s.status == "downloading" || s.status == "connecting").count() as i32;
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
                let pct = if let Some(tot) = s.total {
                    if tot > 0 { (s.downloaded as f32 / tot as f32).min(1.0) } else { 0.0 }
                } else { 0.0 };
                let pct_str = format!("{:.0}%", pct * 100.0);
                let speed_str = fmt_speed(s.speed_bps);
                let eta_str = fmt_eta(s.eta_secs);
                let dl_str = fmt_size(s.downloaded);
                let tot_str = s.total.map(fmt_size).unwrap_or_else(|| "—".into());
                let is_p = s.status == "paused";
                let is_err = s.status == "error";
                let is_done = s.status == "completed";
                let st_text: String = match s.status.as_str() {
                    "downloading" => "Receiving data...".into(),
                    "paused" => "Paused".into(),
                    "connecting" => "Connecting...".into(),
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
                        let status_text = if is_chunk_done || s.status == "completed" {
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
                        let is_active = (s.status == "downloading" || s.status == "connecting") && i <= 8;
                        let status_text = if s.status == "completed" {
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

        // IDM-style: fresh completions pop the "Download complete" dialog
        let mut newly: Vec<(String, String, u64, String, String)> = Vec::new();
        {
            let mut seen = completed_seen.lock().unwrap();
            for s in &snaps {
                if s.status == "completed" && seen.insert(s.id.clone()) {
                    let loc = manager_cloned_for_poll
                        .get_task_path(&s.id)
                        .and_then(|p| p.parent().map(|d| d.to_string_lossy().to_string()))
                        .unwrap_or_default();
                    newly.push((s.id.clone(), s.filename.clone(), s.total.unwrap_or(0), loc, s.url.clone()));
                }
            }
        }

        // armed from the complete dialog: shut down the PC once the queue drains
        if SHUTDOWN_ARMED.load(std::sync::atomic::Ordering::Relaxed)
            && downloading_count == 0
            && completed_count > 0
            && SHUTDOWN_ARMED.swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            // 30s grace window; `shutdown /a` aborts
            let _ = std::process::Command::new("shutdown").args(["/s", "/t", "30"]).spawn();
        }

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
                    if filt == "Downloading" && !(s.status == "downloading" || s.status == "connecting") {
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
        let weak_done_poll = weak_done_poll.clone();
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

            if let Some((id, filename, total, loc, url)) = newly.pop() {
                if let Some(d) = weak_done_poll.upgrade() {
                    d.set_task_id(id.into());
                    d.set_filename(filename.clone().into());
                    d.set_size(if total > 0 { fmt_size(total) } else { "Unknown size".into() }.into());
                    d.set_total_bytes_text(format!("{} Bytes", total).into());
                    d.set_location(loc.into());
                    d.set_url(url.into());
                    if let Some(img) = engine::sys_icon::get_file_icon_image(&filename) {
                        d.set_file_icon(img);
                        d.set_has_sys_icon(true);
                    } else {
                        d.set_has_sys_icon(false);
                    }
                    let _ = d.show();
                    center_over_main(app.window(), d.window(), 500.0, 330.0);
                }
            }
        });
    });

    // initial paint before loop kicks
    {
        let mut snaps = manager.list_downloads().unwrap_or_default();
        snaps.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let items: Vec<DownloadItem> = snaps.iter().map(|s| snapshot_to_item(s, false)).collect();
        app.set_downloads(ModelRc::new(VecModel::from(items)));
    }

    app.show().context("show window")?;
    // run until explicit quit (close hides to tray; the tray outlives hidden windows)
    slint::run_event_loop_until_quit().context("slint run")?;
    // keep runtime alive until window closes (guard dropped here)
    drop(_guard);
    Ok(())
}
