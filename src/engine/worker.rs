//! Per-connection streaming worker with atomic claim + remainder stealing.
use crate::engine::rate_limiter::Limiter;
use crate::storage::database::Db;
use reqwest::Client;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub const MIN_SPLIT: u64 = 1024 * 1024; // don't bother splitting < 2 MiB remainders
const PERSIST_EVERY: u64 = 256 * 1024; // persist progress every 256 KB
const MAX_CONSECUTIVE_FAILURES: u32 = 6;

#[derive(Clone, Copy, PartialEq)]
pub enum Cmd {
    Run,
    Pause,
    Stop,
}

pub type CellsGuard = Arc<Mutex<Vec<Arc<Cell>>>>;

pub struct Cell {
    pub idx: usize,
    /// (start, end-exclusive) — end may shrink mid-flight when a sibling steals
    pub bounds: Mutex<(u64, u64)>,
    /// bytes written relative to start
    pub done: AtomicU64,
    pub speed: AtomicU64,
    pub active: AtomicBool,
}

impl Cell {
    pub fn new(idx: usize, start: u64, end: u64) -> Self {
        Self {
            idx,
            bounds: Mutex::new((start, end)),
            done: AtomicU64::new(0),
            speed: AtomicU64::new(0),
            active: AtomicBool::new(false),
        }
    }
    /// bytes still missing in this cell
    pub fn remaining(&self) -> u64 {
        let (s, e) = *self.bounds.lock().unwrap();
        e.saturating_sub(s + self.done.load(Ordering::Relaxed))
    }
    pub fn span(&self) -> (u64, u64) {
        *self.bounds.lock().unwrap()
    }
}

/// unknown-total sentinel (single-stream fallback)
pub const UNKNOWN_END: u64 = u64::MAX;

#[derive(Clone)]
pub struct WorkerCtx {
    pub client: Client,
    pub limiter: Arc<Limiter>,
    pub db: Arc<Db>,
    pub cells: CellsGuard,
    pub task_id: String,
    pub url: String,
    pub file_path: String,
    pub headers: HashMap<String, String>,
    pub etag: String,
    pub last_modified: String,
    /// false => plain GET without Range (server can't seek); resume = restart from zero
    pub ranged: bool,
    /// fatal marker shared across workers
    pub failed: Arc<AtomicBool>,
}

pub enum DriveOutcome {
    Done,
    Paused,
    Stopped,
    Failed(String),
}

fn open_handle(path: &str) -> std::io::Result<File> {
    OpenOptions::new().create(true).write(true).open(path)
}

pub fn persist_all_cells(db: &Db, task_id: &str, cells: &CellsGuard) {
    if let Ok(guard) = cells.lock() {
        let chunk_rows: Vec<crate::storage::database::ChunkRow> = guard
            .iter()
            .map(|c| {
                let (s, e) = c.span();
                crate::storage::database::ChunkRow {
                    idx: c.idx as i64,
                    start: s as i64,
                    end: if e == UNKNOWN_END {
                        -1
                    } else {
                        (e.saturating_sub(1)) as i64
                    },
                    done: c.done.load(Ordering::Relaxed) as i64,
                }
            })
            .collect();
        db.replace_chunks(task_id, &chunk_rows).ok();
    }
}

/// take ownership of unfinished work; steal the fattest active-cell remainder
/// when every incomplete cell is claimed (adaptive work-stealing).
fn claim_or_steal(ctx: &WorkerCtx) -> Option<Arc<Cell>> {
    let mut guard = ctx.cells.lock().unwrap();

    // pass 1: free incomplete cell
    for c in guard.iter() {
        if c.active.load(Ordering::Acquire) {
            continue;
        }
        let unknown = c.span().1 == UNKNOWN_END;
        if c.remaining() > 0 || (unknown && c.done.load(Ordering::Relaxed) == 0) {
            c.active.store(true, Ordering::Release);
            return Some(c.clone());
        }
    }

    // pass 2: split largest remainder of an ACTIVE cell and become its thief
    let mut best_idx: Option<usize> = None;
    let mut best_rem = MIN_SPLIT * 2;
    for (i, c) in guard.iter().enumerate() {
        if !c.active.load(Ordering::Acquire) {
            continue;
        }
        let rem = c.remaining();
        if rem > best_rem {
            best_rem = rem;
            best_idx = Some(i);
        }
    }
    let vi = best_idx?;
    let victim = guard[vi].clone();
    let (s, e) = *victim.bounds.lock().unwrap();
    let vpos = s + victim.done.load(Ordering::Relaxed);
    if e == UNKNOWN_END || e <= vpos {
        return None;
    }
    let rem = e - vpos;
    if rem < MIN_SPLIT * 2 {
        return None;
    }
    let mid = vpos + rem / 2;
    *victim.bounds.lock().unwrap() = (s, mid);
    let thief = Cell::new(guard.len(), mid, e);
    thief.active.store(true, Ordering::Release);
    let t = Arc::new(thief);
    guard.push(t.clone());

    // Immediately persist updated chunk bounds & new stolen chunk to DB
    let chunk_rows: Vec<crate::storage::database::ChunkRow> = guard
        .iter()
        .map(|c| {
            let (cs, ce) = c.span();
            crate::storage::database::ChunkRow {
                idx: c.idx as i64,
                start: cs as i64,
                end: if ce == UNKNOWN_END {
                    -1
                } else {
                    (ce.saturating_sub(1)) as i64
                },
                done: c.done.load(Ordering::Relaxed) as i64,
            }
        })
        .collect();
    drop(guard);
    ctx.db.replace_chunks(&ctx.task_id, &chunk_rows).ok();

    Some(t)
}

async fn wait_run(rx: &mut watch::Receiver<Cmd>) -> bool {
    // returns false when Stopped
    loop {
        match *rx.borrow() {
            Cmd::Run => return true,
            Cmd::Stop => return false,
            Cmd::Pause => {}
        }
        if rx.changed().await.is_err() {
            return false;
        }
    }
}

/// stream HTTP GET(s) into `cell` until complete / paused / stopped
async fn drive_cell(ctx: &WorkerCtx, cell: &Arc<Cell>, rx: &mut watch::Receiver<Cmd>) -> DriveOutcome {
    let mut attempts: u32 = 0;
    loop {
        let cur = *rx.borrow();
        match cur {
            Cmd::Pause => {
                if !wait_run(rx).await {
                    return DriveOutcome::Stopped;
                }
            }
            Cmd::Stop => return DriveOutcome::Stopped,
            Cmd::Run => {}
        }

        let (s, e) = cell.span();
        let pos = s + cell.done.load(Ordering::Relaxed);
        if e != UNKNOWN_END && pos >= e {
            return DriveOutcome::Done;
        }

        let mut req = ctx.client.get(&ctx.url);
        let send_range = ctx.ranged;
        if send_range && e != UNKNOWN_END {
            req = req.header("Range", format!("bytes={}-{}", pos, e - 1));
        } else if send_range {
            req = req.header("Range", format!("bytes={}-", pos));
        }
        // Only send If-Range if etag is strong (not weak W/...)
        if !ctx.etag.is_empty() && !ctx.etag.starts_with("W/") && !ctx.etag.starts_with("w/") {
            req = req.header("If-Range", &ctx.etag);
        } else if !ctx.last_modified.is_empty() {
            req = req.header("If-Range", &ctx.last_modified);
        }
        for (k, v) in &ctx.headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let mut resp = match req.send().await {
            Ok(r) => r,
            Err(err) => {
                attempts += 1;
                if attempts > MAX_CONSECUTIVE_FAILURES {
                    return DriveOutcome::Failed(format!("connect: {err}"));
                }
                backoff(attempts).await;
                continue;
            }
        };

        let status = resp.status();
        // server answering 200-full-file while we asked to skip ahead would corrupt us
        let strict_206 = send_range && pos > 0;
        if strict_206 && status != reqwest::StatusCode::PARTIAL_CONTENT {
            attempts += 1;
            if attempts > MAX_CONSECUTIVE_FAILURES {
                return DriveOutcome::Failed(format!("resume refused: http {status}"));
            }
            backoff(attempts).await;
            continue;
        }
        if !status.is_success() {
            let retryable =
                status.as_u16() == 429 || status.as_u16() == 503 || status.is_server_error();
            attempts += 1;
            if !retryable || attempts > MAX_CONSECUTIVE_FAILURES {
                return DriveOutcome::Failed(format!("http {status}"));
            }
            backoff(attempts).await;
            continue;
        }

        if let Some(cl) = resp.content_length() {
            if cl > 0 {
                let current_total = ctx.db.get_task_total(&ctx.task_id);
                if current_total <= 0 {
                    let total_sz = pos + cl;
                    ctx.db.set_total(&ctx.task_id, total_sz as i64).ok();
                    if let Some(rt_cells) = ctx.cells.lock().ok() {
                        if rt_cells.len() == 1 {
                            let (s, e) = rt_cells[0].span();
                            if e == UNKNOWN_END {
                                *rt_cells[0].bounds.lock().unwrap() = (s, total_sz);
                            }
                        }
                    }
                }
            }
        }

        let mut file = match open_handle(&ctx.file_path) {
            Ok(f) => f,
            Err(err) => return DriveOutcome::Failed(format!("open file: {err}")),
        };

        let mut cursor = pos;
        let mut written_since_persist: u64 = 0;
        let mut progressed = false;
        let mut stopped = false;
        let mut paused = false;
        let mut eof = false;
        let mut stream_err: Option<String> = None;

        enum Action {
            Ctrl(Cmd),
            Data(bytes::Bytes),
            Eof,
            Err(String),
        }

        loop {
            let action = tokio::select! {
                biased;
                _ = rx.changed() => Action::Ctrl(*rx.borrow()),
                chunk = resp.chunk() => match chunk {
                    Ok(None) => Action::Eof,
                    Ok(Some(b)) => Action::Data(b),
                    Err(err) => Action::Err(format!("read: {err}")),
                },
            };


            match action {
                Action::Ctrl(Cmd::Stop) => {
                    stopped = true;
                    break;
                }
                Action::Ctrl(Cmd::Pause) => {
                    paused = true;
                    break;
                }
                Action::Ctrl(Cmd::Run) => {}
                Action::Err(m) => {
                    stream_err = Some(m);
                    break;
                }
                Action::Eof => {
                    eof = true;
                    break;
                }
                Action::Data(buf) => {
                    // bounds may have shrunk due to theft â€” clamp overflow away
                    let (_, cur_end) = cell.span();
                    let usable = if cur_end == UNKNOWN_END {
                        buf.len()
                    } else {
                        ((cur_end as usize).saturating_sub(cursor as usize)).min(buf.len())
                    };
                    if usable == 0 {
                        break; // rest belongs to the thief now
                    }
                    let g = ctx.limiter.acquire(usable).await;
                    let grant = &buf[..g];
                    // ponytail: sync seek+write â€” kernel-buffered sub-ms, avoids tokio::io plumbing
                    let wres = file.seek(SeekFrom::Start(cursor)).and_then(|_| file.write_all(grant));
                    if let Err(err) = wres {
                        stream_err = Some(format!("write: {err}"));
                        break;
                    }
                    cursor += g as u64;
                    progressed = true;
                    cell.done.fetch_add(g as u64, Ordering::Relaxed);
                    cell.speed.fetch_add(g as u64, Ordering::Relaxed);
                    written_since_persist += g as u64;
                    if written_since_persist >= PERSIST_EVERY {
                        persist_cell(ctx, cell);
                        written_since_persist = 0;
                    }
                    if cur_end != UNKNOWN_END && cursor >= cur_end {
                        break;
                    }
                }
            }
        }
        drop(file);

        if stopped {
            persist_cell(ctx, cell);
            return DriveOutcome::Stopped;
        }

        // unknown total resolved by EOF: finalize real size in-place
        if eof && cell.span().1 == UNKNOWN_END {
            let total = cursor;
            *cell.bounds.lock().unwrap() = (0, total);
            ctx.db.set_total(&ctx.task_id, total as i64).ok();
            persist_cell(ctx, cell);
            if let Ok(f) = File::options().write(true).open(&ctx.file_path) {
                f.set_len(total).ok();
            }
            return DriveOutcome::Done;
        }

        persist_cell(ctx, cell);

        if let Some(m) = stream_err {
            if progressed {
                attempts = 0; // real progress made — hiccups shouldn't doom the task
            }
            attempts += 1;
            if attempts > MAX_CONSECUTIVE_FAILURES {
                return DriveOutcome::Failed(m);
            }
            backoff(attempts).await;
            continue;
        }
        let (_, ce) = cell.span();
        if !paused && ce != UNKNOWN_END && cursor >= ce {
            return DriveOutcome::Done;
        }
        if paused {
            return DriveOutcome::Paused;
        }
        attempts = 0; // healthy progress resets failure budget
    }
}

fn persist_cell(ctx: &WorkerCtx, cell: &Cell) {
    ctx.db
        .update_chunk_done(&ctx.task_id, cell.idx as i64, cell.done.load(Ordering::Relaxed) as i64)
        .ok();
}

async fn backoff(attempt: u32) {
    use rand::Rng;
    let base = 250u64.saturating_mul(1 << attempt.min(5));
    let jitter = rand::thread_rng().gen_range(0..=(base / 2).max(1));
    tokio::time::sleep(std::time::Duration::from_millis(base.min(8000) + jitter)).await;
}

/// One connection's life: claim cells until none remain, then report out.
pub async fn run_worker(ctx: WorkerCtx, mut rx: watch::Receiver<Cmd>) -> DriveOutcome {
    loop {
        let Some(cell) = claim_or_steal(&ctx) else {
            return DriveOutcome::Done;
        };
        let outcome = drive_cell(&ctx, &cell, &mut rx).await;
        cell.active.store(false, Ordering::Release);
        persist_cell(&ctx, &cell);
        match outcome {
            DriveOutcome::Done => {}
            other => {
                persist_all_cells(&ctx.db, &ctx.task_id, &ctx.cells);
                return other;
            }
        }
        if ctx.failed.load(Ordering::Relaxed) {
            persist_all_cells(&ctx.db, &ctx.task_id, &ctx.cells);
            return DriveOutcome::Stopped;
        }
    }
}
