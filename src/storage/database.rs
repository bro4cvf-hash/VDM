use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Queued,
    Connecting,
    Downloading,
    Paused,
    Completed,
    Error,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Connecting => "connecting",
            Status::Downloading => "downloading",
            Status::Paused => "paused",
            Status::Completed => "completed",
            Status::Error => "error",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "queued" => Status::Queued,
            "connecting" => Status::Connecting,
            "downloading" => Status::Downloading,
            "completed" => Status::Completed,
            "error" => Status::Error,
            _ => Status::Paused,
        }
    }
}

/// persisted per-task record
#[derive(Clone, Debug)]
pub struct TaskRow {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub dir: String,
    pub total: i64,
    pub etag: String,
    pub last_modified: String,
    pub accept_ranges: bool,
    pub multi_conn: bool,
    pub status: Status,
    pub created_at: i64,
    pub headers_json: String,
}

#[derive(Clone, Copy, Debug)]
pub struct ChunkRow {
    pub idx: i64,
    pub start: i64,
    pub end: i64,
    pub done: i64,
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?; // WAL-safe, much faster writes
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS tasks(
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                filename TEXT NOT NULL,
                dir TEXT NOT NULL,
                total INTEGER DEFAULT 0,
                etag TEXT DEFAULT '',
                last_modified TEXT DEFAULT '',
                accept_ranges INTEGER DEFAULT 0,
                multi_conn INTEGER DEFAULT 0,
                status TEXT DEFAULT 'queued',
                created_at INTEGER NOT NULL,
                headers_json TEXT DEFAULT '{}'
            );
            CREATE TABLE IF NOT EXISTS chunks(
                task_id TEXT NOT NULL,
                idx INTEGER NOT NULL,
                start INTEGER NOT NULL,
                end INTEGER NOT NULL,
                done INTEGER DEFAULT 0,
                PRIMARY KEY(task_id, idx)
            );
            CREATE TABLE IF NOT EXISTS kv(
                k TEXT PRIMARY KEY,
                v TEXT NOT NULL
            );
            "#,
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn set_total(&self, id: &str, total: i64) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET total=?2 WHERE id=?1",
            params![id, total],
        )?;
        Ok(())
    }

    pub fn get_kv(&self, key: &str) -> Option<String> {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT v FROM kv WHERE k=?1", params![key], |r| r.get(0))
            .ok()
    }

    pub fn set_kv(&self, key: &str, val: &str) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            params![key, val],
        )?;
        Ok(())
    }

    pub fn insert_task(&self, t: &TaskRow) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO tasks(id,url,filename,dir,total,etag,last_modified,accept_ranges,multi_conn,status,created_at,headers_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                t.id,
                t.url,
                t.filename,
                t.dir,
                t.total,
                t.etag,
                t.last_modified,
                t.accept_ranges as i64,
                t.multi_conn as i64,
                t.status.as_str(),
                t.created_at,
                t.headers_json
            ],
        )?;
        Ok(())
    }

    pub fn replace_chunks(&self, task_id: &str, chunks: &[ChunkRow]) -> rusqlite::Result<()> {
        let mut c = self.conn.lock().unwrap();
        // task removed mid-run: drop the write instead of orphaning chunk rows
        let exists: i64 = c.query_row(
            "SELECT COUNT(*) FROM tasks WHERE id=?1",
            params![task_id],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Ok(());
        }
        let tx = c.transaction()?;
        tx.execute("DELETE FROM chunks WHERE task_id=?1", params![task_id])?;
        for ch in chunks {
            tx.execute(
                "INSERT INTO chunks(task_id,idx,start,end,done) VALUES(?1,?2,?3,?4,?5)",
                params![task_id, ch.idx, ch.start, ch.end, ch.done],
            )?;
        }
        tx.commit()
    }

    /// fast incremental persist of one chunk's progress
    pub fn update_chunk_done(&self, task_id: &str, idx: i64, done: i64) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE chunks SET done=?3 WHERE task_id=?1 AND idx=?2",
            params![task_id, idx, done],
        )?;
        Ok(())
    }

    pub fn set_status(&self, id: &str, s: Status) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET status=?2 WHERE id=?1",
            params![id, s.as_str()],
        )?;
        Ok(())
    }

    pub fn update_filename(&self, id: &str, filename: &str) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET filename=?2 WHERE id=?1",
            params![id, filename],
        )?;
        Ok(())
    }

    pub fn update_url(&self, id: &str, url: &str) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET url=?2 WHERE id=?1",
            params![id, url],
        )?;
        Ok(())
    }

    pub fn update_url_and_headers(&self, id: &str, url: &str, headers_json: &str) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET url=?2, headers_json=?3 WHERE id=?1",
            params![id, url, headers_json],
        )?;
        Ok(())
    }

    pub fn update_probe_result(&self, t: &TaskRow) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE tasks SET filename=?2, total=?3, etag=?4, last_modified=?5, accept_ranges=?6, multi_conn=?7 WHERE id=?1",
            params![
                t.id,
                t.filename,
                t.total,
                t.etag,
                t.last_modified,
                t.accept_ranges as i64,
                t.multi_conn as i64
            ],
        )?;
        Ok(())
    }

    pub fn delete_task(&self, id: &str) -> rusqlite::Result<()> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM chunks WHERE task_id=?1", params![id])?;
        tx.execute("DELETE FROM tasks WHERE id=?1", params![id])?;
        tx.commit()
    }

    pub fn list_tasks(&self) -> rusqlite::Result<Vec<(TaskRow, Vec<ChunkRow>)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt =
            c.prepare("SELECT id,url,filename,dir,total,etag,last_modified,accept_ranges,multi_conn,status,created_at,headers_json FROM tasks ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(TaskRow {
                id: r.get(0)?,
                url: r.get(1)?,
                filename: r.get(2)?,
                dir: r.get(3)?,
                total: r.get(4)?,
                etag: r.get(5)?,
                last_modified: r.get(6)?,
                accept_ranges: r.get::<_, i64>(7)? != 0,
                multi_conn: r.get::<_, i64>(8)? != 0,
                status: Status::from_str(&r.get::<_, String>(9)?),
                created_at: r.get(10)?,
                headers_json: r.get(11)?,
            })
        })?;
        let mut tasks: Vec<TaskRow> = rows.collect::<Result<_, _>>()?;

        // one query for all chunks instead of N+1 per task
        let mut chunk_stmt =
            c.prepare("SELECT task_id,idx,start,end,done FROM chunks ORDER BY task_id, idx")?;
        let mut chunks_by_task: HashMap<String, Vec<ChunkRow>> = HashMap::new();
        let all_chunks = chunk_stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ChunkRow {
                    idx: r.get(1)?,
                    start: r.get(2)?,
                    end: r.get(3)?,
                    done: r.get(4)?,
                },
            ))
        })?;
        for row in all_chunks {
            let (tid, ch) = row?;
            chunks_by_task.entry(tid).or_default().push(ch);
        }

        let mut out = Vec::with_capacity(tasks.len());
        for t in &mut tasks {
            let mut cs = chunks_by_task.remove(&t.id).unwrap_or_default();
            if cs.is_empty() && t.multi_conn {
                // rebuild initial split from scratch info
                cs = default_chunks(t.total.max(0) as u64);
            }
            out.push((t.clone(), cs));
        }
        Ok(out)
    }

    /// single-row fetch (row + chunks) — replaces full `list_tasks()` scans by id
    pub fn get_task(&self, id: &str) -> rusqlite::Result<Option<(TaskRow, Vec<ChunkRow>)>> {
        let c = self.conn.lock().unwrap();
        let row = c
            .query_row(
                "SELECT id,url,filename,dir,total,etag,last_modified,accept_ranges,multi_conn,status,created_at,headers_json FROM tasks WHERE id=?1",
                params![id],
                |r| {
                    Ok(TaskRow {
                        id: r.get(0)?,
                        url: r.get(1)?,
                        filename: r.get(2)?,
                        dir: r.get(3)?,
                        total: r.get(4)?,
                        etag: r.get(5)?,
                        last_modified: r.get(6)?,
                        accept_ranges: r.get::<_, i64>(7)? != 0,
                        multi_conn: r.get::<_, i64>(8)? != 0,
                        status: Status::from_str(&r.get::<_, String>(9)?),
                        created_at: r.get(10)?,
                        headers_json: r.get(11)?,
                    })
                },
            )
            .ok();
        let Some(t) = row else { return Ok(None) };
        let mut cs: Vec<ChunkRow> = c
            .prepare("SELECT idx,start,end,done FROM chunks WHERE task_id=?1 ORDER BY idx")?
            .query_map(params![t.id], |r| {
                Ok(ChunkRow {
                    idx: r.get(0)?,
                    start: r.get(1)?,
                    end: r.get(2)?,
                    done: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        if cs.is_empty() && t.multi_conn {
            cs = default_chunks(t.total.max(0) as u64);
        }
        Ok(Some((t, cs)))
    }

    pub fn get_task_total(&self, id: &str) -> i64 {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT total FROM tasks WHERE id=?1", params![id], |r| r.get(0))
            .unwrap_or(0)
    }

    /// FIFO queued ids (oldest first) — caller filters against live in-memory statuses
    pub fn queued_ids(&self) -> Vec<String> {
        self.conn
            .lock()
            .unwrap()
            .prepare("SELECT id FROM tasks WHERE status='queued' ORDER BY created_at ASC")
            .and_then(|mut s| {
                s.query_map([], |r| r.get(0))
                    .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            })
            .unwrap_or_default()
    }
}

/// evenly divide [0,total) into up-to-N ranges (exclusive end)
pub fn default_chunks(total: u64) -> Vec<ChunkRow> {
    default_chunks_n(total, 0)
}

pub fn default_chunks_n(total: u64, parts: usize) -> Vec<ChunkRow> {
    if total == 0 {
        return Vec::new(); // unknown-size streams use worker::UNKNOWN_END cells instead
    }
    let n = parts.clamp(1, 32) as u64;
    let n = n.min(total); // no zero-length degenerate cells when total < parts
    let base = total / n;
    let rem = total % n;
    let mut out = Vec::new();
    let mut pos = 0u64;
    for i in 0..n {
        let len = base + if i < rem { 1 } else { 0 };
        let start = pos;
        let end = pos + len - 1;
        pos = end + 1;
        out.push(ChunkRow { idx: i as i64, start: start as i64, end: end as i64, done: 0 });
    }
    out
}

