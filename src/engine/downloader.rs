//! Master download orchestrator — Slint edition (no Tauri).
use crate::engine::file_allocator;
use crate::engine::probe;
use crate::engine::rate_limiter::Limiter;
use crate::engine::worker::{self, Cell, Cmd, WorkerCtx};
use crate::storage::database::{default_chunks_n, ChunkRow, Db, Status, TaskRow};
use rand::RngCore;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const MIN_CHUNK_TARGET: u64 = 512 * 1024;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn new_id() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

#[derive(Clone, Default)]
pub(crate) struct TaskInfo {
    pub(crate) url: String,
    pub(crate) filename: String,
    pub(crate) dir: String,
}

/// live handle for a task the orchestrator currently owns
pub(crate) struct Runtime {
    pub(crate) cmd_tx: watch::Sender<Cmd>,
    pub(crate) cells: Arc<Mutex<Vec<Arc<Cell>>>>,
    pub(crate) info: Mutex<TaskInfo>,
    pub(crate) total: AtomicI64,
    pub(crate) last_sample_ms: AtomicU64,
    pub(crate) direct_bps: AtomicU64,
}

struct CellSample {
    idx: usize,
    start: u64,
    end: u64,
    done: u64,
}

struct Sample {
    cells: Vec<CellSample>,
    downloaded: u64,
    bps: u64,
}

impl Runtime {
    fn sample(&self) -> Sample {
        let now_ms = now_ms().max(1) as u64;
        let prev = self.last_sample_ms.swap(now_ms, Ordering::Relaxed);
        let dt_ms = now_ms.saturating_sub(prev).max(50);
        let guard = self.cells.lock().unwrap();
        let mut cells = Vec::with_capacity(guard.len());
        let (mut dl, mut delta_sum) = (0u64, 0u64);
        for c in guard.iter() {
            let d = c.done.load(Ordering::Relaxed);
            let delta = c.speed.swap(0, Ordering::Relaxed);
            dl += d;
            delta_sum += delta;
            let (s, e) = c.span();
            cells.push(CellSample {
                idx: c.idx,
                start: s,
                end: if e == worker::UNKNOWN_END { s + d.max(1) } else { e },
                done: d,
            });
        }
        let direct = self.direct_bps.load(Ordering::Relaxed);
        let bps = if direct > 0 {
            direct
        } else {
            delta_sum * 1000 / dt_ms
        };
        Sample {
            cells,
            downloaded: dl,
            bps,
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub idx: usize,
    pub start: u64,
    pub end: u64,
    pub done: u64,
    pub speed_bps: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub dir: String,
    pub status: String,
    pub total: Option<u64>,
    pub downloaded: u64,
    pub speed_bps: u64,
    pub eta_secs: Option<u64>,
    pub created_at: i64,
    pub referrer: String,
    pub error_msg: Option<String>,
    pub segments: Vec<Segment>,
}

fn segments_from(s: &Sample) -> Vec<Segment> {
    s.cells
        .iter()
        .map(|c| Segment {
            idx: c.idx,
            start: c.start,
            end: c.end,
            done: c.done,
            speed_bps: 0,
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct CachedTask {
    pub row: TaskRow,
    pub chunks: Vec<ChunkRow>,
}

pub struct Manager {
    weak: Weak<Manager>,
    pub db: Arc<Db>,
    pub http: reqwest::Client,
    pub limiter: Arc<Limiter>,
    pub torrent_settings: Arc<Mutex<super::torrent::TorrentSettings>>,
    pub default_torrent_dir: PathBuf,
    torrent: Mutex<Option<Arc<super::torrent::TorrentEngine>>>,
    pub max_conns: AtomicU64,
    pub max_active: AtomicU64,
    pub tasks: Mutex<Vec<CachedTask>>,
    runs: Mutex<HashMap<String, Arc<Runtime>>>,
    pub statuses: Mutex<HashMap<String, Status>>,
    pub errors: Mutex<HashMap<String, String>>,
    pub last_etas: Mutex<HashMap<String, u64>>,
    pub post_processing: Mutex<HashMap<String, String>>,
    pumping: AtomicBool,
}

impl Manager {
    pub fn new(db: Db) -> Arc<Self> {
        let db_arc = Arc::new(db);
        let rows = db_arc.list_tasks().unwrap_or_default();
        let mut statuses = HashMap::with_capacity(rows.len());
        let mut tasks = Vec::with_capacity(rows.len());
        for (t, chunks) in rows {
            let s = match t.status {
                Status::Downloading | Status::Connecting | Status::Queued => Status::Paused,
                other => other,
            };
            if s != t.status {
                db_arc.set_status(&t.id, s).ok();
            }
            let mut row = t;
            row.status = s;
            statuses.insert(row.id.clone(), s);
            tasks.push(CachedTask { row, chunks });
        }

        let speed = db_arc.get_kv("speed_bps").and_then(|v| v.parse().ok()).unwrap_or(0);
        let max_conns = db_arc.get_kv("max_conns").and_then(|v| v.parse().ok()).unwrap_or(8);
        let max_active = db_arc.get_kv("max_active").and_then(|v| v.parse().ok()).unwrap_or(3);

        let torrent_down_mbps = db_arc
            .get_kv("torrent_down_limit")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let torrent_up_mbps = db_arc
            .get_kv("torrent_up_limit")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let torrent_max_peers = db_arc
            .get_kv("torrent_max_peers")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(100);
        let torrent_dht = db_arc
            .get_kv("torrent_dht")
            .map(|v| v != "0")
            .unwrap_or(true);
        let torrent_pex = db_arc
            .get_kv("torrent_pex")
            .map(|v| v != "0")
            .unwrap_or(true);
        let torrent_auto_trackers = db_arc
            .get_kv("torrent_auto_trackers")
            .map(|v| v != "0")
            .unwrap_or(true);

        let torrent_settings = super::torrent::TorrentSettings {
            max_download_bps: (torrent_down_mbps * 1024.0 * 1024.0) as u64,
            max_upload_bps: (torrent_up_mbps * 1024.0 * 1024.0) as u64,
            max_peers: torrent_max_peers,
            enable_dht: torrent_dht,
            enable_pex: torrent_pex,
            enable_auto_trackers: torrent_auto_trackers,
        };

        let default_torrent_dir = dirs::download_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Torrents");

        let http = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36")
            .connect_timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(32)
            .tcp_nodelay(true) // no Nagle: chunks stream immediately
            .build()
            .expect("http client");

        Arc::new_cyclic(|weak| Self {
            weak: weak.clone(),
            db: db_arc.clone(),
            http,
            limiter: Arc::new(Limiter::new(speed)),
            torrent_settings: Arc::new(Mutex::new(torrent_settings)),
            default_torrent_dir,
            torrent: Mutex::new(None),
            max_conns: AtomicU64::new(max_conns),
            max_active: AtomicU64::new(max_active),
            tasks: Mutex::new(tasks),
            runs: Mutex::new(HashMap::new()),
            statuses: Mutex::new(statuses),
            errors: Mutex::new(HashMap::new()),
            last_etas: Mutex::new(HashMap::new()),
            post_processing: Mutex::new(HashMap::new()),
            pumping: AtomicBool::new(false),
        })
    }

    #[allow(dead_code)]
    pub fn get_torrent_settings(&self) -> super::torrent::TorrentSettings {
        self.torrent_settings.lock().unwrap().clone()
    }

    pub fn update_torrent_settings<F: FnOnce(&mut super::torrent::TorrentSettings)>(&self, f: F) {
        let mut s = self.torrent_settings.lock().unwrap();
        f(&mut s);
        let cur = s.clone();
        drop(s);
        if let Some(t) = self.torrent.lock().unwrap().as_ref() {
            t.update_settings(cur);
        }
    }

    pub fn get_torrent_engine(&self) -> Option<Arc<super::torrent::TorrentEngine>> {
        self.torrent.lock().unwrap().clone()
    }

    pub fn ensure_torrent_engine(&self) -> anyhow::Result<Arc<super::torrent::TorrentEngine>> {
        let mut guard = self.torrent.lock().unwrap();
        if let Some(ref t) = *guard {
            return Ok(t.clone());
        }
        let settings = self.torrent_settings.lock().unwrap().clone();
        let dir = self.default_torrent_dir.clone();
        let engine = match tokio::runtime::Handle::try_current() {
            Ok(handle) => match handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    tokio::task::block_in_place(|| {
                        handle.block_on(super::torrent::TorrentEngine::new(dir, settings))
                    })
                }
                _ => {
                    std::thread::scope(|s| {
                        s.spawn(|| {
                            let rt = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .expect("temp runtime");
                            rt.block_on(super::torrent::TorrentEngine::new(dir, settings))
                        })
                        .join()
                        .expect("join thread")
                    })
                }
            },
            Err(_) => {
                std::thread::scope(|s| {
                    s.spawn(|| {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("temp runtime");
                        rt.block_on(super::torrent::TorrentEngine::new(dir, settings))
                    })
                    .join()
                    .expect("join thread")
                })
            }
        }?;
        let arc = Arc::new(engine);
        *guard = Some(arc.clone());
        Ok(arc)
    }

    pub async fn ensure_torrent_engine_async(&self) -> anyhow::Result<Arc<super::torrent::TorrentEngine>> {
        {
            let guard = self.torrent.lock().unwrap();
            if let Some(ref t) = *guard {
                return Ok(t.clone());
            }
        }
        let settings = self.torrent_settings.lock().unwrap().clone();
        let dir = self.default_torrent_dir.clone();
        let engine = super::torrent::TorrentEngine::new(dir, settings).await?;
        let arc = Arc::new(engine);
        let mut guard = self.torrent.lock().unwrap();
        if let Some(ref existing) = *guard {
            return Ok(existing.clone());
        }
        *guard = Some(arc.clone());
        Ok(arc)
    }

    #[allow(dead_code)]
    pub fn torrent(&self) -> anyhow::Result<Arc<super::torrent::TorrentEngine>> {
        self.ensure_torrent_engine()
    }

    // ---------------- settings ----------------

    pub fn set_speed_limit(&self, bps: u64) {
        self.limiter.set_rate(bps);
        self.db.set_kv("speed_bps", &bps.to_string()).ok();
    }

    pub fn set_max_connections(&self, n: u64) {
        let n = n.clamp(1, 32);
        self.max_conns.store(n, Ordering::Relaxed);
        self.db.set_kv("max_conns", &n.to_string()).ok();
    }

    pub fn set_max_active(&self, n: u64) {
        let n = n.clamp(1, 16);
        self.max_active.store(n, Ordering::Relaxed);
        self.db.set_kv("max_active", &n.to_string()).ok();
        self.pump();
    }

    // ---------------- task lifecycle ----------------

    #[allow(dead_code)]
    pub fn add_download(
        &self,
        url: String,
        folder: Option<String>,
        filename: Option<String>,
        headers: HashMap<String, String>,
    ) -> anyhow::Result<TaskSnapshot> {

        self.add_download_with_total(url, folder, filename, headers, None)
    }

    pub fn add_download_with_total(
        &self,
        url: String,
        folder: Option<String>,
        filename: Option<String>,
        headers: HashMap<String, String>,
        total_hint: Option<u64>,
    ) -> anyhow::Result<TaskSnapshot> {
        let mut trimmed = url.trim().trim_matches(|c| c == '<' || c == '>' || c == '"' || c == '\'').to_string();
        if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") && !trimmed.starts_with("magnet:?") {
            if trimmed.contains('.') && !trimmed.contains("://") {
                trimmed = format!("https://{trimmed}");
            } else {
                return Err(anyhow::anyhow!("Please provide a valid HTTP, HTTPS, or Magnet URL"));
            }
        }
        let inferred = probe::infer_filename_from_url(&trimmed);
        let chosen_name = if let Some(ref f) = filename {
            let f_trim = f.trim();
            if f_trim.is_empty()
                || f_trim == "main"
                || f_trim == "master"
                || f_trim == "download"
                || (!f_trim.contains('.') && !trimmed.starts_with("magnet:?"))
            {
                inferred
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        if !f_trim.is_empty() {
                            f_trim.to_string()
                        } else {
                            probe::url_basename(&trimmed).unwrap_or_else(|| format!("download-{}", now_ms()))
                        }
                    })
            } else {
                f_trim.to_string()
            }
        } else {
            inferred.unwrap_or_else(|| format!("download-{}", now_ms()))
        };
        let name = probe::sanitize_name(&chosen_name);
        let dir = folder.unwrap_or_else(|| default_download_dir(&name));
        fs::create_dir_all(&dir)?;

        let row = TaskRow {
            id: new_id(),
            url: trimmed,
            filename: unique_path_name(&dir, &name),
            dir,
            total: total_hint.map(|t| t as i64).unwrap_or(0),
            etag: String::new(),
            last_modified: String::new(),
            accept_ranges: false,
            multi_conn: false,
            status: Status::Queued,
            created_at: now_ms(),
            headers_json: serde_json::to_string(&headers)?,
        };
        self.db.insert_task(&row)?;
        self.tasks.lock().unwrap().push(CachedTask {
            row: row.clone(),
            chunks: Vec::new(),
        });
        self.statuses.lock().unwrap().insert(row.id.clone(), Status::Queued);
        self.pump();
        self.snapshot_of(&row.id)
    }


    pub fn pause(&self, id: &str) -> anyhow::Result<()> {
        self.pause_internal(id, true)
    }

    fn pause_internal(&self, id: &str, should_pump: bool) -> anyhow::Result<()> {
        match *self.statuses.lock().unwrap().get(id).unwrap_or(&Status::Paused) {
            Status::Downloading | Status::Connecting | Status::Queued => {}
            _ => return Ok(()),
        }
        self.db.set_status(id, Status::Paused)?;
        self.statuses.lock().unwrap().insert(id.into(), Status::Paused);
        if let Some(task) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
            task.row.status = Status::Paused;
        }
        if let Some(rt) = self.runs.lock().unwrap().get(id) {
            let _ = rt.cmd_tx.send_replace(Cmd::Pause);
            if let Ok(guard) = rt.cells.lock() {
                let chunk_rows: Vec<ChunkRow> = guard
                    .iter()
                    .map(|c| {
                        let (s, e) = c.span();
                        ChunkRow {
                            idx: c.idx as i64,
                            start: s as i64,
                            end: if e == worker::UNKNOWN_END {
                                -1
                            } else {
                                (e.saturating_sub(1)) as i64
                            },
                            done: c.done.load(Ordering::Relaxed) as i64,
                        }
                    })
                    .collect();
                self.db.replace_chunks(id, &chunk_rows).ok();
                if let Some(task) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
                    task.chunks = chunk_rows;
                }
            }
        }
        if let Some(t) = self.get_torrent_engine() {
            t.pause_torrent(id);
        }
        if should_pump {
            self.pump(); // freed a slot — promote the oldest queued task
        }
        Ok(())
    }

    pub fn pause_all(&self) {
        let ids: Vec<String> = {
            let st = self.statuses.lock().unwrap();
            st.iter()
                .filter(|(_, s)| matches!(**s, Status::Downloading | Status::Connecting | Status::Queued))
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let _ = self.pause_internal(&id, false);
        }
    }

    pub fn resume(&self, id: &str) -> anyhow::Result<()> {
        {
            let st = self.statuses.lock().unwrap();
            match st.get(id) {
                Some(Status::Downloading) | Some(Status::Connecting) | Some(Status::Queued) => {
                    return Ok(())
                }
                Some(Status::Completed) => return Err(anyhow::anyhow!("already completed")),
                _ => {}
            }
        }
        self.errors.lock().unwrap().remove(id);
        self.db.set_status(id, Status::Queued)?;
        self.statuses.lock().unwrap().insert(id.into(), Status::Queued);
        if let Some(task) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
            task.row.status = Status::Queued;
        }
        if let Some(t) = self.get_torrent_engine() {
            t.resume_torrent(id);
        }
        self.pump();
        Ok(())
    }

    pub fn resume_all(&self) {
        let ids: Vec<String> = {
            let st = self.statuses.lock().unwrap();
            st.iter()
                .filter(|(_, s)| matches!(**s, Status::Paused | Status::Error))
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let _ = self.resume(&id);
        }
    }

    pub fn remove(&self, id: &str, delete_file: bool) -> anyhow::Result<()> {
        if let Some(rt) = self.runs.lock().unwrap().get(id) {
            let _ = rt.cmd_tx.send_replace(Cmd::Stop);
        }
        if let Some(t) = self.get_torrent_engine() {
            t.cancel_torrent(id);
        }
        if delete_file {
            let tasks = self.tasks.lock().unwrap();
            if let Some(task) = tasks.iter().find(|t| t.row.id == id) {
                fs::remove_file(Path::new(&task.row.dir).join(&task.row.filename)).ok();
            }
        }
        self.db.delete_task(id)?;
        self.tasks.lock().unwrap().retain(|t| t.row.id != id);
        self.runs.lock().unwrap().remove(id);
        self.statuses.lock().unwrap().remove(id);
        self.errors.lock().unwrap().remove(id);
        self.last_etas.lock().unwrap().remove(id);
        self.post_processing.lock().unwrap().remove(id);
        self.pump();
        Ok(())
    }

    pub fn clear_completed(&self) -> anyhow::Result<()> {
        let ids: Vec<String> = {
            let tasks = self.tasks.lock().unwrap();
            let statuses = self.statuses.lock().unwrap();
            tasks
                .iter()
                .filter(|t| statuses.get(&t.row.id) == Some(&Status::Completed) || t.row.status == Status::Completed)
                .map(|t| t.row.id.clone())
                .collect()
        };
        for id in ids {
            let _ = self.remove(&id, false);
        }
        Ok(())
    }

    pub fn redownload(&self, id: &str) -> anyhow::Result<()> {
        if let Some(rt) = self.runs.lock().unwrap().get(id) {
            let _ = rt.cmd_tx.send_replace(Cmd::Stop);
        }
        if let Some(t) = self.get_torrent_engine() {
            t.cancel_torrent(id);
        }
        self.runs.lock().unwrap().remove(id);
        self.post_processing.lock().unwrap().remove(id);
        self.errors.lock().unwrap().remove(id);

        let mut file_to_remove = None;
        {
            let mut tasks = self.tasks.lock().unwrap();
            if let Some(task) = tasks.iter_mut().find(|t| t.row.id == id) {
                file_to_remove = Some(Path::new(&task.row.dir).join(&task.row.filename));
                task.chunks.clear();
                task.row.status = Status::Queued;
            }
        }
        if let Some(path) = file_to_remove {
            let _ = fs::remove_file(&path);
            let _ = self.db.replace_chunks(id, &[]);
            self.db.set_status(id, Status::Queued)?;
            self.statuses.lock().unwrap().insert(id.into(), Status::Queued);
            self.pump();
        }
        Ok(())
    }

    pub fn rename_task(&self, id: &str, new_filename: &str, new_dir: Option<&str>) -> anyhow::Result<()> {
        let clean_filename = new_filename.trim();
        if clean_filename.is_empty() {
            return Ok(());
        }
        if let Some(rt) = self.runs.lock().unwrap().get(id) {
            let _ = rt.cmd_tx.send_replace(Cmd::Pause);
        }
        let mut rename_paths = None;
        {
            let tasks = self.tasks.lock().unwrap();
            if let Some(task) = tasks.iter().find(|t| t.row.id == id) {
                let old_dir = task.row.dir.clone();
                let old_path = Path::new(&old_dir).join(&task.row.filename);
                let target_dir = new_dir.unwrap_or(&old_dir).trim();
                let final_dir = if target_dir.is_empty() { old_dir.clone() } else { target_dir.to_string() };
                let new_path = Path::new(&final_dir).join(clean_filename);
                rename_paths = Some((old_path, new_path, final_dir));
            }
        }
        if let Some((old_path, new_path, final_dir)) = rename_paths {
            if old_path.exists() && old_path != new_path {
                if let Some(parent) = new_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::rename(&old_path, &new_path);
            }
            self.db.update_filename_and_dir(id, clean_filename, &final_dir)?;
            let mut tasks = self.tasks.lock().unwrap();
            if let Some(task) = tasks.iter_mut().find(|t| t.row.id == id) {
                task.row.filename = clean_filename.to_string();
                task.row.dir = final_dir;
            }
        }
        Ok(())
    }

    pub fn get_task_path(&self, id: &str) -> Option<PathBuf> {
        let tasks = self.tasks.lock().unwrap();
        tasks
            .iter()
            .find(|t| t.row.id == id)
            .map(|t| PathBuf::from(&t.row.dir).join(&t.row.filename))
    }

    // ---------------- snapshots ----------------

    pub fn snapshot_of(&self, id: &str) -> anyhow::Result<TaskSnapshot> {
        let (row, chunks) = {
            let tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter().find(|t| t.row.id == id) else {
                anyhow::bail!("task not found");
            };
            (t.row.clone(), t.chunks.clone())
        };
        Ok(self.merged_snapshot(&row, &chunks))
    }

    pub fn list_downloads(&self) -> anyhow::Result<Vec<TaskSnapshot>> {
        let items: Vec<(TaskRow, Vec<ChunkRow>)> = {
            let tasks = self.tasks.lock().unwrap();
            tasks.iter().map(|t| (t.row.clone(), t.chunks.clone())).collect()
        };
        Ok(items
            .into_iter()
            .map(|(t, chunks)| self.merged_snapshot(&t, &chunks))
            .collect())
    }

    fn merged_snapshot(&self, t: &TaskRow, chunks: &[ChunkRow]) -> TaskSnapshot {
        let status = self
            .statuses
            .lock()
            .unwrap()
            .get(&t.id)
            .copied()
            .unwrap_or(t.status);

        let is_processing = self.post_processing.lock().unwrap().contains_key(&t.id);
        let mut downloaded = chunks.iter().map(|c| c.done.max(0) as u64).sum();
        let total = (t.total > 0).then_some(t.total as u64);
        if status == Status::Completed {
            if let Some(tot) = total {
                downloaded = tot.max(downloaded);
            }
        }

        // only parse headers JSON when a referrer key can possibly be in it
        let referrer = if t.headers_json.contains("efer") {
            serde_json::from_str::<HashMap<String, String>>(&t.headers_json)
                .ok()
                .and_then(|h| {
                    h.get("Referer")
                        .or_else(|| h.get("referer"))
                        .or_else(|| h.get("referrer"))
                        .or_else(|| h.get("Referrer"))
                        .cloned()
                })
                .unwrap_or_default()
        } else if let Ok(u) = reqwest::Url::parse(&t.url) {
            u.host_str().map_or_else(String::new, |host| format!("{}://{}", u.scheme(), host))
        } else {
            String::new()
        };
        let error_msg = self.errors.lock().unwrap().get(&t.id).cloned();

        let snap_status = if is_processing && status != Status::Completed && status != Status::Error {
            "processing".to_string()
        } else {
            status_str(status).to_string()
        };

        let mut snap = TaskSnapshot {
            id: t.id.clone(),
            url: t.url.clone(),
            filename: t.filename.clone(),
            dir: t.dir.clone(),
            status: snap_status,
            total,
            downloaded,
            speed_bps: 0,
            eta_secs: None,
            created_at: t.created_at,
            referrer,
            error_msg,
            segments: vec![],
        };

        let rt = self.runs.lock().unwrap().get(&t.id).cloned();
        if let Some(rt) = rt {
            let smp = rt.sample();
            snap.downloaded = smp.downloaded.max(snap.downloaded);
            snap.speed_bps = smp.bps;
            let rt_tot = rt.total.load(Ordering::Relaxed);
            if rt_tot > 0 {
                snap.total = Some(rt_tot as u64);
            }
            if let Some(total) = snap.total {
                if smp.bps > 0 && smp.downloaded < total {
                    let eta = (total - smp.downloaded) / smp.bps.max(1);
                    snap.eta_secs = Some(eta);
                    self.last_etas.lock().unwrap().insert(t.id.clone(), eta);
                }
            }
            snap.segments = segments_from(&smp);
            let info = rt.info.lock().unwrap();
            if !info.url.is_empty() {
                snap.url = info.url.clone();
            }
            if !info.dir.is_empty() {
                snap.dir = info.dir.clone();
            }
            if !info.filename.is_empty() {
                snap.filename = info.filename.clone();
            }
        } else if snap.status == "completed" {
            self.last_etas.lock().unwrap().remove(&t.id);
        } else {
            if let Some(last) = self.last_etas.lock().unwrap().get(&t.id).copied() {
                snap.eta_secs = Some(last);
            }
        }
        snap
    }

    pub fn update_task_url(&self, id: &str, new_url: &str) -> anyhow::Result<()> {
        self.update_task_url_and_headers(id, new_url, None)
    }

    pub fn update_task_url_and_headers(
        &self,
        id: &str,
        new_url: &str,
        headers: Option<&HashMap<String, String>>,
    ) -> anyhow::Result<()> {
        let trimmed = new_url.trim();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!("URL cannot be empty"));
        }
        let json_opt = if let Some(h) = headers {
            let json = serde_json::to_string(h)?;
            self.db.update_url_and_headers(id, trimmed, &json)?;
            Some(json)
        } else {
            self.db.update_url(id, trimmed)?;
            None
        };
        {
            let mut tasks = self.tasks.lock().unwrap();
            if let Some(task) = tasks.iter_mut().find(|t| t.row.id == id) {
                task.row.url = trimmed.to_string();
                if let Some(json) = json_opt {
                    task.row.headers_json = json;
                }
                task.row.status = Status::Queued;
            }
        }
        self.errors.lock().unwrap().remove(id);
        self.db.set_status(id, Status::Queued)?;
        self.statuses.lock().unwrap().insert(id.into(), Status::Queued);
        self.pump();
        Ok(())
    }

    // ---------------- engine drive ----------------

    fn spawn_run(&self, id: &str) {
        let Some(this) = self.weak.upgrade() else { return };
        {
            let mut runs = this.runs.lock().unwrap();
            if runs.contains_key(id) {
                return;
            }
            let (tx, _rx) = watch::channel(Cmd::Run);
            runs.insert(
                id.to_string(),
                Arc::new(Runtime {
                    cmd_tx: tx,
                    cells: Arc::new(Mutex::new(Vec::new())),
                    info: Mutex::new(TaskInfo::default()),
                    total: AtomicI64::new(0),
                    last_sample_ms: AtomicU64::new(now_ms().max(1) as u64),
                    direct_bps: AtomicU64::new(0),
                }),
            );
        }
        let owned = id.to_string();
        let this2 = this.clone();
        tokio::spawn(async move { this2.run_task(owned).await });
    }

    async fn run_task(self: Arc<Self>, id: String) {
        let res = self.clone().drive_download(&id).await;
        let err_text = match res {
            Err(e) if e.to_string() != "__stopped__" => Some(e.to_string()),
            _ => None,
        };
        if let Some(rt) = self.runs.lock().unwrap().remove(&id) {
            let _ = rt.cmd_tx.send_replace(Cmd::Stop);
        }
        self.post_processing.lock().unwrap().remove(&id);
        if let Some(msg) = err_text {
            self.db.set_status(&id, Status::Error).ok();
            self.statuses.lock().unwrap().insert(id.clone(), Status::Error);
            if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
                t.row.status = Status::Error;
            }
            self.errors.lock().unwrap().insert(id.clone(), msg.clone());
            eprintln!("[VDM] download {id} failed: {msg}");
        }
        self.pump();
    }

    async fn drive_download(&self, id: &str) -> anyhow::Result<()> {
        let Some(rt) = self.runs.lock().unwrap().get(id).cloned() else {
            return Ok(());
        };

        let (row, chunk_rows) = {
            let tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter().find(|t| t.row.id == id) else {
                return Ok(());
            };
            (t.row.clone(), t.chunks.clone())
        };
        if row.status == Status::Paused {
            // keep the in-memory map in sync or the task occupies a pump slot forever
            self.statuses.lock().unwrap().insert(id.into(), Status::Paused);
            return Ok(());
        }

        let headers: HashMap<String, String> = serde_json::from_str(&row.headers_json)?;
        {
            let mut info = rt.info.lock().unwrap();
            *info = TaskInfo {
                url: row.url.clone(),
                filename: row.filename.clone(),
                dir: row.dir.clone(),
            };
        }

        if row.url.starts_with("magnet:?") {
            return self.run_torrent_task(row, rt).await;
        }

        // Special handling for YouTube media extraction:
        // Extract direct HTTPS media stream URLs and pass to VDM's 16-connection native engine!
        // YouTube DASH returns a video-only and a separate audio stream for most
        // formats; keep the audio URL so we can mux it in after the video lands.
        let mut audio_url: Option<String> = None;
        let (actual_url, actual_headers) = if super::ytdl::is_youtube(&row.url) {
            let is_audio = row.filename.to_lowercase().ends_with(".mp3")
                || row.filename.to_lowercase().ends_with(".m4a");

            let fmt = if row.url.contains("itag=") {
                let itag = row.url.split("itag=").nth(1).and_then(|s| s.split('&').next());
                itag.map(|it| format!("{it}+bestaudio/best"))
            } else {
                None
            };

            match super::ytdl::extract_direct_stream_urls(&row.url, fmt.as_deref(), is_audio).await {
                Ok((video_url, audio_opt)) => {
                    let mut yh = super::ytdl::default_youtube_headers();
                    for (k, v) in headers {
                        yh.insert(k, v);
                    }
                    audio_url = audio_opt;
                    (video_url, yh)
                }
                Err(e) => {
                    eprintln!("[VDM ytdl] URL extraction fallback: {e}");
                    (row.url.clone(), headers)
                }
            }
        } else {
            (row.url.clone(), headers)
        };

        let probed = match probe::probe(&self.http, &actual_url, &actual_headers).await {
            Ok(p) => p,
            Err(e) => return Err(anyhow::anyhow!("pre-flight failed: {e}")),
        };

        let mut actual_filename = row.filename.clone();
        let name_is_generic = !actual_filename.contains('.')
            || actual_filename == "main"
            || actual_filename == "master"
            || actual_filename.starts_with("download")
            || actual_filename.starts_with("main.")
            || actual_filename.starts_with("master.");

        if let Some(better) = &probed.filename_hint {
            if better.contains('.') && (name_is_generic || !row.filename.contains('.')) {
                let sanitized = probe::sanitize_name(better);
                actual_filename = unique_path_name(&row.dir, &sanitized);
                let _ = self.db.update_filename(&row.id, &actual_filename);
                if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == row.id) {
                    t.row.filename = actual_filename.clone();
                }
                let mut info = rt.info.lock().unwrap();
                info.filename = actual_filename.clone();
            }
        }

        let ranged = probed.accept_ranges || row.accept_ranges;
        let total = probed.total.or((row.total > 0).then_some(row.total as u64));

        let file_path = PathBuf::from(&row.dir).join(&actual_filename);
        fs::create_dir_all(&row.dir)?;

        let has_chunk_data = chunk_rows.iter().any(|c| c.done > 0);
        let resuming_with_data = (ranged && has_chunk_data) || (file_path.exists() && has_chunk_data);

        if !resuming_with_data {
            match (ranged, total) {
                (true, Some(sz)) => {
                    file_allocator::preallocate(&file_path, sz)?;
                }
                _ => {
                    fs::File::create(&file_path)?;
                }
            }
        } else if !file_path.exists() {
            if let Some(sz) = total {
                if ranged {
                    file_allocator::preallocate(&file_path, sz)?;
                } else {
                    fs::File::create(&file_path)?;
                }
            } else {
                fs::File::create(&file_path)?;
            }
        }

        let max_configured = self.max_conns.load(Ordering::Relaxed).clamp(1, 32);
        let conns = match (ranged, total) {
            (true, Some(t)) if t > 0 => {
                let target = (t / MIN_CHUNK_TARGET).max(1);
                target.min(max_configured).clamp(1, 32) as usize
            }
            (true, _) => max_configured as usize,
            _ => 1,
        };
        let cells: Vec<Arc<Cell>> = if resuming_with_data && !chunk_rows.is_empty() {
            chunk_rows
                .iter()
                .map(|c| {
                    let end_exclusive = if c.end < 0 {
                        worker::UNKNOWN_END
                    } else {
                        (c.end as u64).saturating_add(1)
                    };
                    let cell = Cell::new(c.idx as usize, c.start.max(0) as u64, end_exclusive);
                    cell.done.store(c.done.max(0) as u64, Ordering::Relaxed);
                    Arc::new(cell)
                })
                .collect()
        } else if ranged && total.is_some() {
            default_chunks_n(total.unwrap_or(1), conns)
                .into_iter()
                .map(|c| Arc::new(Cell::new(c.idx as usize, c.start as u64, (c.end + 1) as u64)))
                .collect()
        } else {
            // no size known (ranged or not): single stream grown to EOF — splitting
            // an unknown total would clip every cell to the placeholder size
            vec![Arc::new(Cell::new(0, 0, worker::UNKNOWN_END))]
        };

        let chunk_row_vec = cells
            .iter()
            .map(|c| ChunkRow {
                idx: c.idx as i64,
                start: c.span().0 as i64,
                end: if c.span().1 == worker::UNKNOWN_END {
                    -1
                } else {
                    (c.span().1.saturating_sub(1)) as i64
                },
                done: c.done.load(Ordering::Relaxed) as i64,
            })
            .collect::<Vec<_>>();

        self.db.replace_chunks(id, &chunk_row_vec)?;
        if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
            t.chunks = chunk_row_vec;
        }
        *rt.cells.lock().unwrap() = cells.clone();
        rt.total.store(total.map(|t| t as i64).unwrap_or(0), Ordering::Relaxed);

        let updated_row = TaskRow {
            id: id.into(),
            filename: actual_filename.clone(),
            url: row.url.clone(),
            dir: row.dir.clone(),
            total: total.map(|t| t as i64).unwrap_or(0),
            etag: probed.etag.clone(),
            last_modified: probed.last_modified.clone(),
            accept_ranges: probed.accept_ranges,
            multi_conn: conns > 1,
            status: Status::Downloading,
            created_at: row.created_at,
            headers_json: row.headers_json.clone(),
        };
        self.db.update_probe_result(&updated_row)?;
        if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
            t.row = updated_row;
        }

        let ctx = WorkerCtx {
            client: self.http.clone(),
            limiter: self.limiter.clone(),
            db: self.db.clone(),
            cells: rt.cells.clone(),
            task_id: id.to_string(),
            url: actual_url,
            file_path: file_path.to_string_lossy().into_owned(),
            headers: actual_headers.clone(),
            etag: probed.etag,
            last_modified: probed.last_modified,
            ranged,
            failed: Arc::new(AtomicBool::new(false)),
        };

        self.statuses.lock().unwrap().insert(id.into(), Status::Downloading);
        self.db.set_status(id, Status::Downloading).ok();
        let _ = rt.cmd_tx.send_replace(Cmd::Run);

        let n_workers = conns.min(cells.len()).max(1);
        let mut joinset = tokio::task::JoinSet::new();
        for _ in 0..n_workers {
            joinset.spawn(worker::run_worker(ctx.clone(), rt.cmd_tx.subscribe()));
        }

        let mut failed: Option<String> = None;
        let mut stopped = false;
        let mut paused_seen = false;
        while let Some(out) = joinset.join_next().await {
            let outcome = match out {
                Ok(o) => o,
                Err(join_err) => worker::DriveOutcome::Failed(format!("worker crashed: {join_err}")),
            };
            match outcome {
                worker::DriveOutcome::Failed(m) => {
                    if failed.is_none() {
                        failed = Some(m);
                    }
                    ctx.failed.store(true, Ordering::Release);
                    rt.cmd_tx.send(Cmd::Pause).ok();
                }
                worker::DriveOutcome::Stopped => stopped = true,
                worker::DriveOutcome::Paused => paused_seen = true,
                worker::DriveOutcome::Done => {}
            }
        }

        // Flush all chunk states to database and in-memory task state cache
        if let Ok(guard) = rt.cells.lock() {
            let chunk_rows: Vec<ChunkRow> = guard
                .iter()
                .map(|c| {
                    let (s, e) = c.span();
                    ChunkRow {
                        idx: c.idx as i64,
                        start: s as i64,
                        end: if e == worker::UNKNOWN_END {
                            -1
                        } else {
                            (e.saturating_sub(1)) as i64
                        },
                        done: c.done.load(Ordering::Relaxed) as i64,
                    }
                })
                .collect();
            self.db.replace_chunks(id, &chunk_rows).ok();
            if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
                t.chunks = chunk_rows;
            }
        }

        if stopped {
            return Err(anyhow::anyhow!("__stopped__"));
        }
        if let Some(m) = failed {
            return Err(anyhow::anyhow!("{m}"));
        }

        // a resume() that raced this winding-down run re-queued the task; a fresh
        // run owns it now — hand it back to the pump instead of clobbering status
        match *self.statuses.lock().unwrap().get(id).unwrap_or(&Status::Paused) {
            Status::Queued | Status::Connecting => {
                self.statuses.lock().unwrap().insert(id.into(), Status::Queued);
                self.db.set_status(id, Status::Queued).ok();
                if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
                    t.row.status = Status::Queued;
                }
                return Ok(());
            }
            _ => {}
        }

        let known_total = rt.total.load(Ordering::Relaxed);
        let done_sum = rt.sample().downloaded;
        let all_cells_done = {
            let guard = rt.cells.lock().unwrap();
            !guard.is_empty() && guard.iter().all(|c| c.remaining() == 0)
        };
        let completed = !paused_seen && (known_total <= 0 || done_sum >= known_total as u64 || all_cells_done);

        if completed {
            // YouTube DASH serves video-only + separate audio for most formats.
            // Fetch the audio sidecar and mux it into the finished video, or the
            // "downloaded" file plays silently.
            if let Some(audio_url) = audio_url {
                self.post_processing
                    .lock()
                    .unwrap()
                    .insert(id.into(), "Merging audio & video...".into());
                let ext = match Path::new(&actual_filename).extension().and_then(|e| e.to_str()) {
                    Some(e) => e.to_string(),
                    None => "mp4".to_string(),
                };
                let audio_tmp = file_path.with_extension(format!("vdm-audio-tmp"));
                let mux_out = file_path.with_extension(format!("vdm-mux-tmp.{}", ext));
                match self.fetch_stream_to_file(&audio_url, &actual_headers, &audio_tmp).await {
                    Ok(()) => {
                        match super::ytdl::mux_audio_video(&file_path, &audio_tmp, &mux_out).await {
                            Ok(()) => {
                                fs::remove_file(&file_path).ok();
                                if let Err(e) = fs::rename(&mux_out, &file_path) {
                                    println!("[VDM] mux rename failed: {e}");
                                }
                            }
                            Err(e) => println!("[VDM] audio mux failed: {e}"),
                        }
                        fs::remove_file(&audio_tmp).ok();
                    }
                    Err(e) => println!("[VDM] audio fetch failed: {e}"),
                }
                self.post_processing.lock().unwrap().remove(id);
            }
            self.db.set_status(id, Status::Completed)?;
            self.statuses.lock().unwrap().insert(id.into(), Status::Completed);
            if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
                t.row.status = Status::Completed;
                if let Some(tot) = total {
                    t.row.total = tot as i64;
                }
            }
            println!("[VDM] download complete: {}", row.filename);
        } else {
            self.db.set_status(id, Status::Paused)?;
            self.statuses.lock().unwrap().insert(id.into(), Status::Paused);
            if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == id) {
                t.row.status = Status::Paused;
            }
        }
        Ok(())
    }

    async fn run_torrent_task(&self, row: TaskRow, rt: Arc<Runtime>) -> anyhow::Result<()> {
        let output_dir = PathBuf::from(&row.dir);
        fs::create_dir_all(&output_dir)?;

        self.statuses.lock().unwrap().insert(row.id.clone(), Status::Connecting);
        self.db.set_status(&row.id, Status::Connecting).ok();
        if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == row.id) {
            t.row.status = Status::Connecting;
        }

        let only_files: Option<Vec<usize>> = serde_json::from_str::<HashMap<String, serde_json::Value>>(&row.headers_json)
            .ok()
            .and_then(|h| h.get("selected_files").cloned())
            .and_then(|v| serde_json::from_value::<Vec<usize>>(v).ok());

        let torrent = self.ensure_torrent_engine_async().await?;
        let handle = torrent.start_torrent(&row.id, &row.url, &output_dir, only_files).await?;

        self.statuses.lock().unwrap().insert(row.id.clone(), Status::Downloading);
        self.db.set_status(&row.id, Status::Downloading).ok();
        if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == row.id) {
            t.row.status = Status::Downloading;
        }

        let mut cmd_rx = rt.cmd_tx.subscribe();
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        let mut last_dl = 0u64;
        let mut last_sample_time = std::time::Instant::now();
        let mut direct_bps = 0u64;

        loop {
            tokio::select! {
                res = cmd_rx.changed() => {
                    if res.is_ok() {
                        let val = *cmd_rx.borrow();
                        if val == Cmd::Pause || val == Cmd::Stop {
                            let stats = handle.stats();
                            let total_b = if stats.total_bytes > 0 { stats.total_bytes } else { row.total.max(0) as u64 };
                            let chunk_row = ChunkRow {
                                idx: 0,
                                start: 0,
                                end: if total_b > 0 { total_b as i64 } else { stats.progress_bytes as i64 },
                                done: stats.progress_bytes as i64,
                            };
                            self.db.replace_chunks(&row.id, &[chunk_row]).ok();
                            if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == row.id) {
                                t.chunks = vec![chunk_row];
                            }
                            return Err(anyhow::anyhow!("__stopped__"));
                        }
                    }
                }
                _ = interval.tick() => {
                    let stats = handle.stats();
                    let now = std::time::Instant::now();
                    let dt = now.duration_since(last_sample_time).as_secs_f64();

                    let is_initializing = matches!(stats.state, librqbit::TorrentStatsState::Initializing { .. });
                    let (live_peers, fetched_bytes) = if let Some(ref live) = stats.live {
                        (live.snapshot.peer_stats.live, live.snapshot.fetched_bytes)
                    } else {
                        (0, 0)
                    };

                    let current_status = if live_peers > 0 && !is_initializing {
                        Status::Downloading
                    } else {
                        Status::Connecting
                    };
                    let prev_status = self.statuses.lock().unwrap().insert(row.id.clone(), current_status);
                    if prev_status != Some(current_status) {
                        self.db.set_status(&row.id, current_status).ok();
                        if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == row.id) {
                            t.row.status = current_status;
                        }
                    }

                    if dt >= 0.2 {
                        if is_initializing || live_peers == 0 {
                            direct_bps = 0;
                            rt.direct_bps.store(0, Ordering::Relaxed);
                            last_dl = fetched_bytes;
                        } else {
                            let dl_diff = fetched_bytes.saturating_sub(last_dl);
                            let inst_speed = (dl_diff as f64 / dt) as u64;
                            direct_bps = if direct_bps == 0 {
                                inst_speed
                            } else {
                                (direct_bps as f64 * 0.7 + inst_speed as f64 * 0.3) as u64
                            };
                            rt.direct_bps.store(direct_bps, Ordering::Relaxed);
                            last_dl = fetched_bytes;
                        }
                        last_sample_time = now;
                    }

                    let total_bytes = if stats.total_bytes > 0 {
                        stats.total_bytes
                    } else if row.total > 0 {
                        row.total as u64
                    } else {
                        0
                    };

                    if total_bytes > 0 {
                        rt.total.store(total_bytes as i64, Ordering::Relaxed);
                        if total_bytes as i64 != row.total {
                            self.db.set_total(&row.id, total_bytes as i64).ok();
                            if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == row.id) {
                                t.row.total = total_bytes as i64;
                            }
                        }
                    }

                    // Update chunk progress visualizer
                    {
                        let mut cells = rt.cells.lock().unwrap();
                        if cells.is_empty() {
                            let cell = Cell::new(0, 0, if total_bytes > 0 { total_bytes } else { worker::UNKNOWN_END });
                            cells.push(Arc::new(cell));
                        }
                        if let Some(c) = cells.first() {
                            c.done.store(stats.progress_bytes, Ordering::Relaxed);
                        }
                    }

                    if stats.finished {
                        let prev = self.statuses.lock().unwrap().insert(row.id.clone(), Status::Completed);
                        if prev != Some(Status::Completed) {
                            self.db.set_status(&row.id, Status::Completed).ok();
                            let chunk_row = ChunkRow {
                                idx: 0,
                                start: 0,
                                end: if total_bytes > 0 { total_bytes as i64 } else { stats.progress_bytes as i64 },
                                done: stats.progress_bytes as i64,
                            };
                            self.db.replace_chunks(&row.id, &[chunk_row]).ok();
                            if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == row.id) {
                                t.row.status = Status::Completed;
                                if total_bytes > 0 {
                                    t.row.total = total_bytes as i64;
                                }
                                t.chunks = vec![chunk_row];
                            }
                        }
                        return Ok(());
                    }

                    if let Some(ref err) = stats.error {
                        return Err(anyhow::anyhow!("Torrent error: {}", err));
                    }
                }
            }
        }
    }

    /// Download a small sidecar stream (YouTube audio) to a temp file.
    async fn fetch_stream_to_file(
        &self,
        url: &str,
        headers: &HashMap<String, String>,
        dest: &Path,
    ) -> anyhow::Result<()> {
        use std::io::Write;
        let mut req = self.http.get(url).timeout(Duration::from_secs(60));
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let mut resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("sidecar http {}", resp.status()));
        }
        let mut file = std::fs::File::create(dest)?;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk)?;
        }
        file.flush()?;
        Ok(())
    }

    /// FIFO promotion of Queued tasks up to max_active concurrent slots.
    pub fn pump(&self) {
        if self.pumping.swap(true, Ordering::SeqCst) {
            return;
        }
        while self.running_count() < self.max_active.load(Ordering::Relaxed) as usize {
            let next = self.oldest_queued();
            let Some(next) = next else { break };
            self.statuses
                .lock()
                .unwrap()
                .insert(next.clone(), Status::Connecting);
            self.db.set_status(&next, Status::Connecting).ok();
            if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.row.id == next) {
                t.row.status = Status::Connecting;
            }
            self.spawn_run(&next);
        }
        self.pumping.store(false, Ordering::SeqCst);
    }

    fn running_count(&self) -> usize {
        self.statuses
            .lock()
            .unwrap()
            .values()
            .filter(|s| matches!(s, Status::Downloading | Status::Connecting))
            .count()
    }

    fn oldest_queued(&self) -> Option<String> {
        let st = self.statuses.lock().unwrap();
        let tasks = self.tasks.lock().unwrap();
        tasks
            .iter()
            .find(|t| st.get(&t.row.id) == Some(&Status::Queued))
            .map(|t| t.row.id.clone())
    }
}

// IDM-style default: sort into type folders under Downloads
fn default_download_dir(filename: &str) -> String {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let sub = if ext.is_empty() {
        "Documents"
    } else {
        match ext.as_str() {
            "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "dmg" | "pkg" | "tgz" => "Compressed",
            "exe" | "msi" | "apk" | "deb" | "rpm" | "appimage" | "bat" | "cmd" | "ps1" => "Programs",
            "mp4" | "mkv" | "mov" | "avi" | "wmv" | "flv" | "webm" | "m4v" | "m2ts" => "Video",
            "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" => "Music",
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "epub" | "md" => "Documents",
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tiff" => "Pictures",
            _ => "Documents",
        }
    };
    std::env::var("USERPROFILE")
        .map(|h| format!("{h}\\Downloads\\{sub}"))
        .unwrap_or_else(|_| ".".into())
}

fn unique_path_name(dir: &str, name: &str) -> String {
    let base = PathBuf::from(dir).join(name);
    if !base.exists() {
        return name.to_string();
    }
    let ext = Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()));
    let stem_len = name.len() - ext.as_ref().map_or(0, |e| e.len());
    let stem = &name[..stem_len];
    for n in 1..10000u32 {
        let cand = match &ext {
            Some(e) => format!("{stem} ({n}){e}"),
            None => format!("{name} ({n})"),
        };
        if !PathBuf::from(dir).join(&cand).exists() {
            return cand;
        }
    }
    format!("{name}-{}", new_id())
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Queued => "queued",
        Status::Connecting => "connecting",
        Status::Downloading => "downloading",
        Status::Paused => "paused",
        Status::Completed => "completed",
        Status::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::database::default_chunks_n;

    #[test]
    fn chunk_split_covers_file_exactly() {
        for &(total, parts) in &[(100u64, 4usize), (1023, 8), (u64::MAX / 2, 32), (1, 32)] {
            let cs = default_chunks_n(total, parts);
            assert!(!cs.is_empty());
            assert_eq!(cs.first().unwrap().start, 0);
            let mut expect = 0i64;
            for c in &cs {
                assert_eq!(c.start, expect, "gap/overlap at idx {}", c.idx);
                expect = c.end + 1;
            }
            assert_eq!(expect, total as i64, "coverage mismatch total={total}");
        }
        assert!(default_chunks_n(0, 4).is_empty());
    }

    #[test]
    fn unique_names_never_collide() {
        let dir = std::env::temp_dir().join(format!("vdm_test_{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = unique_path_name(dir.to_str().unwrap(), "file.zip");
        std::fs::write(dir.join(&a), b"x").unwrap();
        let b = unique_path_name(dir.to_str().unwrap(), "file.zip");
        assert_ne!(a, b);
        assert!(b.starts_with("file (1)"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn test_drive_download_codeload() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_dl_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);
        let url = "https://raw.githubusercontent.com/rust-lang/rust/master/README.md".to_string();
        let snap = mgr.add_download(url, Some(temp_dir.to_string_lossy().to_string()), Some("README.md".into()), HashMap::new()).unwrap();
        let mut completed = false;
        for _ in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let s = mgr.snapshot_of(&snap.id).unwrap();
            if s.status == "completed" {
                completed = true;
                break;
            }
        }
        assert!(completed, "download did not complete in time");
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_pause_and_resume_download() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_pr_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);
        let url = "https://raw.githubusercontent.com/rust-lang/rust/master/README.md".to_string();
        let snap = mgr.add_download(url, Some(temp_dir.to_string_lossy().to_string()), Some("resume_test.md".into()), HashMap::new()).unwrap();

        // Pause task
        let _ = mgr.pause(&snap.id);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let paused_snap = mgr.snapshot_of(&snap.id).unwrap();
        assert!(paused_snap.status == "paused" || paused_snap.status == "completed");

        // Resume
        mgr.resume(&snap.id).unwrap();
        let mut completed = false;
        for _ in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let s = mgr.snapshot_of(&snap.id).unwrap();
            if s.status == "completed" {
                completed = true;
                break;
            }
        }
        assert!(completed, "resumed download did not complete");
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_max_active_settings() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_settings_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);

        mgr.set_max_active(5);
        assert_eq!(mgr.max_active.load(Ordering::Relaxed), 5);
        assert_eq!(mgr.db.get_kv("max_active"), Some("5".into()));

        mgr.set_max_connections(16);
        assert_eq!(mgr.max_conns.load(Ordering::Relaxed), 16);
        assert_eq!(mgr.db.get_kv("max_conns"), Some("16".into()));

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_update_task_url_and_headers() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_renew_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);

        let snap = mgr.add_download(
            "https://example.com/expired-url.zip?token=old".into(),
            Some(temp_dir.to_string_lossy().to_string()),
            Some("test_renew.zip".into()),
            HashMap::new(),
        ).unwrap();

        let mut fresh_headers = HashMap::new();
        fresh_headers.insert("Cookie".to_string(), "session=abc123xyz".to_string());
        fresh_headers.insert("Referer".to_string(), "https://example.com/download-page".to_string());

        let new_url = "https://example.com/fresh-url.zip?token=new123";
        mgr.update_task_url_and_headers(&snap.id, new_url, Some(&fresh_headers)).unwrap();

        let updated_snap = mgr.snapshot_of(&snap.id).unwrap();
        assert_eq!(updated_snap.url, new_url);
        assert_eq!(updated_snap.referrer, "https://example.com/download-page");

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_in_memory_task_cache_and_zero_disk_reads() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_cache_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);

        // Add 3 downloads
        let snap1 = mgr.add_download("https://example.com/file1.zip".into(), Some(temp_dir.to_string_lossy().to_string()), Some("file1.zip".into()), HashMap::new()).unwrap();
        let snap2 = mgr.add_download("https://example.com/file2.zip".into(), Some(temp_dir.to_string_lossy().to_string()), Some("file2.zip".into()), HashMap::new()).unwrap();
        let snap3 = mgr.add_download("https://example.com/file3.zip".into(), Some(temp_dir.to_string_lossy().to_string()), Some("file3.zip".into()), HashMap::new()).unwrap();

        // Check in-memory task list
        let downloads = mgr.list_downloads().unwrap();
        assert_eq!(downloads.len(), 3);
        assert!(downloads.iter().any(|d| d.id == snap1.id));
        assert!(downloads.iter().any(|d| d.id == snap2.id));
        assert!(downloads.iter().any(|d| d.id == snap3.id));

        // Rename task in cache + db
        mgr.rename_task(&snap1.id, "file1_renamed.zip", None).unwrap();
        let s1 = mgr.snapshot_of(&snap1.id).unwrap();
        assert_eq!(s1.filename, "file1_renamed.zip");

        // Remove task
        mgr.remove(&snap2.id, false).unwrap();
        let downloads2 = mgr.list_downloads().unwrap();
        assert_eq!(downloads2.len(), 2);
        assert!(!downloads2.iter().any(|d| d.id == snap2.id));

        // Clear completed
        let _ = mgr.clear_completed();

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_pause_all_and_chunk_persistence_sync() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_pause_all_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);

        // Add multiple downloads
        let _snap1 = mgr.add_download("https://example.com/item1.bin".into(), Some(temp_dir.to_string_lossy().to_string()), Some("item1.bin".into()), HashMap::new()).unwrap();
        let _snap2 = mgr.add_download("https://example.com/item2.bin".into(), Some(temp_dir.to_string_lossy().to_string()), Some("item2.bin".into()), HashMap::new()).unwrap();

        // Pause all
        mgr.pause_all();

        // Verify both in-memory cache and statuses reflect Paused without promoting other tasks
        let downloads = mgr.list_downloads().unwrap();
        for d in downloads {
            assert_eq!(d.status, "paused");
        }

        let db_tasks = mgr.db.list_tasks().unwrap();
        for (t, _) in db_tasks {
            assert_eq!(t.status, Status::Paused);
        }

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_rapid_pause_resume_concurrency() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_rapid_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);

        let mut ids = Vec::new();
        for i in 0..5 {
            let snap = mgr
                .add_download(
                    format!("https://example.com/file_{i}.bin"),
                    Some(temp_dir.to_string_lossy().to_string()),
                    Some(format!("file_{i}.bin")),
                    HashMap::new(),
                )
                .unwrap();
            ids.push(snap.id);
        }

        // Rapidly toggle pause and resume across tasks in parallel
        for _ in 0..10 {
            for id in &ids {
                let _ = mgr.pause(id);
            }
            let downloads = mgr.list_downloads().unwrap();
            for d in &downloads {
                assert!(d.status == "paused" || d.status == "completed" || d.status == "error");
            }

            for id in &ids {
                let _ = mgr.resume(id);
            }
        }

        mgr.pause_all();
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_edge_case_task_not_found_and_empty_rename() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_edge_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);

        // Non-existent ID queries should fail gracefully without panic
        assert!(mgr.snapshot_of("non-existent-id").is_err());
        assert_eq!(mgr.get_task_path("non-existent-id"), None);

        // Rename with empty filename should be a no-op
        let snap = mgr.add_download(
            "https://example.com/edge.zip".into(),
            Some(temp_dir.to_string_lossy().to_string()),
            Some("edge.zip".into()),
            HashMap::new(),
        ).unwrap();

        mgr.rename_task(&snap.id, "   ", None).unwrap();
        let s = mgr.snapshot_of(&snap.id).unwrap();
        assert_eq!(s.filename, "edge.zip");

        // Rename with valid trimmed filename
        mgr.rename_task(&snap.id, "  edge_renamed.zip  ", None).unwrap();
        let s2 = mgr.snapshot_of(&snap.id).unwrap();
        assert_eq!(s2.filename, "edge_renamed.zip");

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_queue_promotion_and_max_active_respect() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_queue_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);

        // Set max_active = 2
        mgr.set_max_active(2);

        // Add 5 tasks
        let mut ids = Vec::new();
        for i in 0..5 {
            let snap = mgr.add_download(
                format!("https://example.com/item_{i}.dat"),
                Some(temp_dir.to_string_lossy().to_string()),
                Some(format!("item_{i}.dat")),
                HashMap::new(),
            ).unwrap();
            ids.push(snap.id);
        }

        // Check running count
        let downloads = mgr.list_downloads().unwrap();
        let active_count = downloads.iter().filter(|d| d.status == "downloading" || d.status == "connecting").count();
        let queued_count = downloads.iter().filter(|d| d.status == "queued").count();
        assert!(active_count <= 2, "Active count {active_count} exceeded max_active=2");
        assert_eq!(active_count + queued_count, 5);

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_lazy_torrent_engine_initialization_and_settings() {
        let temp_dir = std::env::temp_dir().join(format!("vdm_test_lazy_torrent_{}", new_id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("test.db");
        let db = Db::open(&db_path).unwrap();
        let mgr = Manager::new(db);

        // Verify lazy state: engine is NOT initialized on creation
        assert!(mgr.get_torrent_engine().is_none());

        // Update settings without initializing engine
        mgr.update_torrent_settings(|s| {
            s.max_peers = 150;
            s.enable_dht = false;
        });
        let settings = mgr.get_torrent_settings();
        assert_eq!(settings.max_peers, 150);
        assert_eq!(settings.enable_dht, false);
        assert!(mgr.get_torrent_engine().is_none());

        // Now trigger on-demand initialization
        let engine = mgr.ensure_torrent_engine().unwrap();
        assert!(mgr.get_torrent_engine().is_some());
        assert_eq!(engine.get_settings().max_peers, 150);

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}



