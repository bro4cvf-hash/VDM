//! High-performance BitTorrent engine for VDM
//! Powered by librqbit with DHT, PEX, speed limiting, and auto public tracker injection.

use anyhow::Context;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, SessionOptions,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// High-speed public tier-1 BitTorrent trackers for rapid magnet swarm discovery
pub const DEFAULT_PUBLIC_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://tracker.openbittorrent.com:80/announce",
    "udp://explodie.org:6969/announce",
    "udp://p4p.arenabg.com:1337/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://tracker.coppersurfer.tk:6969/announce",
    "udp://tracker.leechers-paradise.org:6969/announce",
    "udp://9.rarbg.to:2920/announce",
    "udp://9.rarbg.me:2970/announce",
    "udp://tracker.internetwarriors.net:1337/announce",
    "udp://tracker.cyberia.is:6969/announce",
    "udp://tracker.moeking.me:6969/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://tracker.pomf.se:80/announce",
    "udp://tracker.theoks.net:6969/announce",
    "udp://tracker.armifi.org:6969/announce",
    "http://tracker.openbittorrent.com:80/announce",
    "https://opentracker.i2p.rocks:443/announce",
];

#[derive(Clone, Debug)]
pub struct TorrentSettings {
    pub max_download_bps: u64,
    pub max_upload_bps: u64,
    pub max_peers: usize,
    pub enable_dht: bool,
    pub enable_pex: bool,
    pub enable_auto_trackers: bool,
}

impl Default for TorrentSettings {
    fn default() -> Self {
        Self {
            max_download_bps: 0, // unlimited
            max_upload_bps: 0,   // unlimited
            max_peers: 100,
            enable_dht: true,
            enable_pex: true,
            enable_auto_trackers: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TorrentFileMetadata {
    pub id: usize,
    pub name: String,
    pub size_bytes: u64,
    pub file_type: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct TorrentStatsSnapshot {
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub fetched_bytes: u64,
    pub download_speed_bps: u64,
    pub upload_speed_bps: u64,
    pub live_peers: usize,
    pub live_seeds: usize,
    pub state_kind: String,
    pub is_finished: bool,
    pub error: Option<String>,
}

pub struct TorrentEngine {
    session: Arc<Session>,
    settings: Arc<Mutex<TorrentSettings>>,
    handles: Arc<Mutex<HashMap<String, Arc<ManagedTorrent>>>>,
}

impl TorrentEngine {
    pub async fn new(download_dir: PathBuf, settings: TorrentSettings) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&download_dir)?;

        let make_opts = |dht_enabled: bool| {
            let mut opts = SessionOptions::default();
            opts.disable_trackers = false;
            opts.fastresume = true;
            opts.persistence = Some(librqbit::SessionPersistenceConfig::Json { folder: None });
            if dht_enabled {
                opts.dht = Some(librqbit::DhtSessionConfig::default());
            } else {
                opts.dht = None;
            }
            opts
        };

        let session = match Session::new_with_opts(download_dir.clone(), make_opts(settings.enable_dht)).await {
            Ok(s) => s,
            Err(e) => {
                if settings.enable_dht {
                    Session::new_with_opts(download_dir, make_opts(false))
                        .await
                        .context("Failed to initialize BitTorrent session")?
                } else {
                    return Err(e).context("Failed to initialize BitTorrent session");
                }
            }
        };

        Ok(Self {
            session,
            settings: Arc::new(Mutex::new(settings)),
            handles: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn update_settings(&self, new_settings: TorrentSettings) {
        let mut s = self.settings.lock().unwrap();
        *s = new_settings;
    }

    pub fn get_settings(&self) -> TorrentSettings {
        self.settings.lock().unwrap().clone()
    }

    /// Appends public tier-1 trackers to magnet links if they aren't already included
    pub fn enhance_magnet_url(&self, magnet: &str) -> String {
        let s = self.settings.lock().unwrap();
        if !s.enable_auto_trackers || !magnet.starts_with("magnet:?") {
            return magnet.to_string();
        }

        let mut enhanced = magnet.to_string();
        for tr in DEFAULT_PUBLIC_TRACKERS {
            let encoded_tr = format!("&tr={}", urlencoding::encode(tr));
            if !enhanced.contains(&encoded_tr) && !enhanced.contains(*tr) {
                enhanced.push_str(&encoded_tr);
            }
        }
        enhanced
    }

    /// Probes and lists files within a torrent/magnet before downloading
    pub async fn fetch_torrent_files(
        &self,
        magnet_or_url: &str,
    ) -> anyhow::Result<(String, u64, Vec<TorrentFileMetadata>)> {
        let enhanced_url = self.enhance_magnet_url(magnet_or_url);
        let magnet_info = super::probe::parse_magnet(&enhanced_url);

        let add_opts = AddTorrentOptions {
            list_only: true,
            overwrite: true,
            ..Default::default()
        };

        let add_torrent = AddTorrent::from_url(&enhanced_url);
        let session = self.session.clone();

        // Attempt metadata resolution with a 6-second timeout
        let list_fut = session.add_torrent(add_torrent, Some(add_opts));
        let list_res = tokio::time::timeout(std::time::Duration::from_secs(6), list_fut).await;

        if let Ok(Ok(AddTorrentResponse::ListOnly(metadata))) = list_res {
            let torrent_name = magnet_info
                .as_ref()
                .and_then(|m| m.display_name.clone())
                .unwrap_or_else(|| "Torrent Package".to_string());

            let mut files = Vec::new();
            let mut total_size = 0u64;

            for (idx, file) in metadata.info.iter_file_details().enumerate() {
                let name = file.filename.to_string();
                let size = file.len;
                total_size += size;
                let ext = name.split('.').last().unwrap_or("").to_lowercase();
                let file_type = match ext.as_str() {
                    "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "ts" => "video",
                    "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" => "audio",
                    "zip" | "rar" | "7z" | "tar" | "gz" | "iso" => "archive",
                    "exe" | "msi" | "apk" | "dmg" => "installer",
                    "jpg" | "jpeg" | "png" | "webp" | "gif" | "svg" => "image",
                    _ => "file",
                }
                .to_string();

                files.push(TorrentFileMetadata {
                    id: idx,
                    name,
                    size_bytes: size,
                    file_type,
                });
            }

            if !files.is_empty() {
                return Ok((torrent_name, total_size, files));
            }
        }

        // Fallback: If metadata exchange was still in progress or single-file torrent
        let fallback_name = magnet_info
            .as_ref()
            .and_then(|m| m.display_name.clone())
            .unwrap_or_else(|| "Torrent Download".to_string());
        let fallback_size = magnet_info
            .as_ref()
            .and_then(|m| m.total_size)
            .unwrap_or(0);

        let ext = fallback_name.split('.').last().unwrap_or("").to_lowercase();
        let file_type = match ext.as_str() {
            "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "ts" => "video",
            "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" => "audio",
            "zip" | "rar" | "7z" | "tar" | "gz" | "iso" => "archive",
            "exe" | "msi" | "apk" | "dmg" => "installer",
            "jpg" | "jpeg" | "png" | "webp" | "gif" | "svg" => "image",
            _ => "file",
        }
        .to_string();

        let files = vec![TorrentFileMetadata {
            id: 0,
            name: fallback_name.clone(),
            size_bytes: fallback_size,
            file_type,
        }];

        Ok((fallback_name, fallback_size, files))
    }

    /// Adds and starts downloading a magnet link or .torrent file with optional selective files
    pub async fn start_torrent(
        &self,
        task_id: &str,
        magnet_or_url: &str,
        output_folder: &Path,
        only_files: Option<Vec<usize>>,
    ) -> anyhow::Result<Arc<ManagedTorrent>> {
        let enhanced_url = self.enhance_magnet_url(magnet_or_url);
        std::fs::create_dir_all(output_folder)?;

        let add_opts = AddTorrentOptions {
            output_folder: Some(output_folder.to_string_lossy().to_string()),
            overwrite: true,
            paused: false,
            only_files,
            ..Default::default()
        };

        let add_torrent = AddTorrent::from_url(&enhanced_url);
        let resp = self
            .session
            .add_torrent(add_torrent, Some(add_opts))
            .await
            .context("Failed to add torrent to session")?;

        let handle = match resp {
            AddTorrentResponse::Added(_, h) => {
                let _ = self.session.unpause(&h).await;
                h
            }
            AddTorrentResponse::AlreadyManaged(_, h) => {
                let _ = self.session.unpause(&h).await;
                h
            }
            AddTorrentResponse::ListOnly(_) => {
                anyhow::bail!("Unexpected ListOnly response");
            }
        };

        self.handles
            .lock()
            .unwrap()
            .insert(task_id.to_string(), handle.clone());

        Ok(handle)
    }

    pub fn pause_torrent(&self, task_id: &str) {
        if let Some(h) = self.handles.lock().unwrap().get(task_id).cloned() {
            let session = self.session.clone();
            tokio::spawn(async move {
                let _ = session.pause(&h).await;
            });
        }
    }

    pub fn resume_torrent(&self, task_id: &str) {
        if let Some(h) = self.handles.lock().unwrap().get(task_id).cloned() {
            let session = self.session.clone();
            tokio::spawn(async move {
                let _ = session.unpause(&h).await;
            });
        }
    }

    pub fn cancel_torrent(&self, task_id: &str) {
        let handle_opt = self.handles.lock().unwrap().remove(task_id);
        if let Some(handle) = handle_opt {
            let session = self.session.clone();
            let id = librqbit::api::TorrentIdOrHash::Id(handle.id());
            tokio::spawn(async move {
                let _ = session.delete(id, false).await;
            });
        }
    }

    /// Polls stats for an active torrent
    pub fn get_stats(&self, task_id: &str) -> Option<TorrentStatsSnapshot> {
        let handles = self.handles.lock().unwrap();
        let handle = handles.get(task_id)?;

        let stats = handle.stats();

        let state_kind = match stats.state {
            librqbit::TorrentStatsState::Initializing { .. } => "initializing",
            librqbit::TorrentStatsState::Live => "live",
            librqbit::TorrentStatsState::Paused => "paused",
            librqbit::TorrentStatsState::Error => "error",
        };

        let (peers, seeds, fetched_bytes) = if let Some(ref live) = stats.live {
            let snap = &live.snapshot;
            (snap.peer_stats.live as usize, snap.peer_stats.seen as usize, snap.fetched_bytes)
        } else {
            (0, 0, 0)
        };

        Some(TorrentStatsSnapshot {
            downloaded_bytes: stats.progress_bytes,
            uploaded_bytes: stats.uploaded_bytes,
            total_bytes: if stats.total_bytes > 0 {
                Some(stats.total_bytes)
            } else {
                None
            },
            fetched_bytes,
            download_speed_bps: 0,
            upload_speed_bps: 0,
            live_peers: peers,
            live_seeds: seeds,
            state_kind: state_kind.to_string(),
            is_finished: stats.finished,
            error: stats.error.map(|e| e.to_string()),
        })
    }
}
