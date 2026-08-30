use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;

pub const DEFAULT_SERVER_PORT: u16 = 9191;

/// browsers always send Origin on POST; extension workers send chrome-extension://
/// or none. A plain web page's origin (CSRF / DNS-rebinding) is rejected.
fn origin_allowed(origin: &str) -> bool {
    origin.is_empty()
        || origin.starts_with("chrome-extension://")
        || origin.starts_with("moz-extension://")
        || origin.starts_with("safari-extension://")
        || origin.starts_with("http://localhost")
        || origin.starts_with("http://127.0.0.1")
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(default)]
pub struct DownloadPayload {
    pub url: String,
    #[serde(alias = "fileName")]
    pub filename: String,
    #[serde(alias = "referer")]
    pub referrer: String,
    pub cookies: String,
    #[serde(alias = "userAgent")]
    pub user_agent: String,
    #[serde(alias = "fileSize")]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum ServerEvent {
    AddDownload(DownloadPayload),
    ShowWindow,
}

pub struct LoopbackServer {
    port: u16,
    sender: UnboundedSender<ServerEvent>,
}

impl LoopbackServer {
    pub fn new(port: u16, sender: UnboundedSender<ServerEvent>) -> Self {
        Self { port, sender }
    }

    pub async fn run(self: Arc<Self>) {
        let addr = format!("127.0.0.1:{}", self.port);
        let mut listener_opt = None;

        // Retry binding with backoff in case a closing instance is freeing the port
        for attempt in 1..=5 {
            match TcpListener::bind(&addr).await {
                Ok(l) => {
                    println!("[VDM Server] Loopback API listening on http://{}", addr);
                    listener_opt = Some(l);
                    break;
                }
                Err(e) => {
                    if attempt < 5 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                    } else {
                        println!("[VDM Server] Note: Port {} is managed by active instance ({})", self.port, e);
                    }
                }
            }
        }

        let listener = match listener_opt {
            Some(l) => l,
            None => return,
        };

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let s = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = s.handle_connection(stream).await {
                            eprintln!("[VDM Server] Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("[VDM Server] Accept error: {}", e);
                }
            }
        }
    }

    async fn handle_connection(&self, mut stream: TcpStream) -> anyhow::Result<()> {
        let mut buffer = Vec::new();
        let mut temp = [0u8; 4096];
        let mut header_end = None;
        let mut content_length = 0;
        let mut origin = String::new();

        loop {
            let n = stream.read(&mut temp).await?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&temp[..n]);

            if header_end.is_none() {
                if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(pos + 4);
                    let header_str = String::from_utf8_lossy(&buffer[..pos]);
                    for line in header_str.lines() {
                        if let Some((k, v)) = line.split_once(':') {
                            let key = k.trim();
                            if key.eq_ignore_ascii_case("content-length") {
                                content_length = v.trim().parse::<usize>().unwrap_or(0);
                            } else if key.eq_ignore_ascii_case("origin") {
                                origin = v.trim().to_string();
                            }
                        }
                    }
                }
            }

            if let Some(hend) = header_end {
                if buffer.len() >= hend + content_length {
                    break;
                }
            }
        }

        if buffer.is_empty() {
            return Ok(());
        }

        let request_str = String::from_utf8_lossy(&buffer);
        let mut lines = request_str.lines();
        let request_line = match lines.next() {
            Some(l) => l,
            None => return Ok(()),
        };

        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");

        // Handle CORS Preflight
        if method == "OPTIONS" {
            let response = "HTTP/1.1 204 No Content\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                Access-Control-Allow-Headers: Content-Type, Authorization\r\n\
                Access-Control-Max-Age: 86400\r\n\
                Content-Length: 0\r\n\
                \r\n";
            stream.write_all(response.as_bytes()).await?;
            return Ok(());
        }

        // Show/Bring Window to Front (Single-Instance Handler)
        if (method == "GET" || method == "POST") && (path == "/show" || path == "/open") {
            if !origin_allowed(&origin) {
                let response = "HTTP/1.1 403 Forbidden\r\nAccess-Control-Allow-Origin: null\r\nContent-Length: 0\r\n\r\n";
                stream.write_all(response.as_bytes()).await?;
                return Ok(());
            }
            let _ = self.sender.send(ServerEvent::ShowWindow);            let body = serde_json::json!({ "success": true, "message": "Window shown" }).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                Content-Type: application/json\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Content-Length: {}\r\n\
                \r\n\
                {}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            return Ok(());
        }

        // Health Check
        if method == "GET" && (path == "/health" || path == "/status") {
            let body = serde_json::json!({
                "status": "ok",
                "app": "VDM",
                "version": "0.2.0"
            })
            .to_string();

            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                Content-Type: application/json\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Content-Length: {}\r\n\
                \r\n\
                {}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await?;
            return Ok(());
        }

        // Add Download Interception
        if method == "POST" && path == "/add-download" {
            if !origin_allowed(&origin) {
                let res_body =
                    serde_json::json!({ "success": false, "error": "forbidden origin" }).to_string();
                let response = format!(
                    "HTTP/1.1 403 Forbidden\r\n\
                    Content-Type: application/json\r\n\
                    Access-Control-Allow-Origin: null\r\n\
                    Content-Length: {}\r\n\
                    \r\n\
                    {}",
                    res_body.len(),
                    res_body
                );
                stream.write_all(response.as_bytes()).await?;
                return Ok(());
            }
            let body_str = if let Some(hend) = header_end {
                if buffer.len() >= hend {
                    &request_str[hend..]
                } else {
                    ""
                }
            } else {
                ""
            };

            if let Ok(payload) = serde_json::from_str::<DownloadPayload>(body_str) {
                if !payload.url.trim().is_empty() {
                    let _ = self.sender.send(ServerEvent::AddDownload(payload));

                    let res_body = serde_json::json!({ "success": true }).to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\n\
                        Content-Type: application/json\r\n\
                        Access-Control-Allow-Origin: *\r\n\
                        Content-Length: {}\r\n\
                        \r\n\
                        {}",
                        res_body.len(),
                        res_body
                    );
                    stream.write_all(response.as_bytes()).await?;
                    return Ok(());
                }
            }

            let res_body = serde_json::json!({ "success": false, "error": "Invalid payload" }).to_string();
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\n\
                Content-Type: application/json\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Content-Length: {}\r\n\
                \r\n\
                {}",
                res_body.len(),
                res_body
            );
            stream.write_all(response.as_bytes()).await?;
            return Ok(());
        }

        // Not Found
        let response = "HTTP/1.1 404 Not Found\r\n\
            Access-Control-Allow-Origin: *\r\n\
            Content-Length: 0\r\n\
            \r\n";
        stream.write_all(response.as_bytes()).await?;
        Ok(())
    }
}
