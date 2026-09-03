use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;

pub const DEFAULT_SERVER_PORT: u16 = 9191;

/// browsers always send Origin on POST; extension workers send chrome-extension://
/// or none. A plain web page's origin (CSRF / DNS-rebinding) is rejected.
fn origin_allowed(origin: &str) -> bool {
    if origin.is_empty() {
        return true;
    }
    if origin.starts_with("chrome-extension://")
        || origin.starts_with("moz-extension://")
        || origin.starts_with("safari-extension://")
    {
        return true;
    }

    if let Ok(url) = reqwest::Url::parse(origin) {
        if let Some(host) = url.host_str() {
            if host == "localhost" || host == "127.0.0.1" {
                return true;
            }
        }
    } else {
        let trimmed = origin.trim_end_matches('/');
        if trimmed == "http://localhost"
            || trimmed.starts_with("http://localhost:")
            || trimmed == "https://localhost"
            || trimmed.starts_with("https://localhost:")
            || trimmed == "http://127.0.0.1"
            || trimmed.starts_with("http://127.0.0.1:")
            || trimmed == "https://127.0.0.1"
            || trimmed.starts_with("https://127.0.0.1:")
        {
            return true;
        }
    }

    false
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
                    #[cfg(debug_assertions)]
                    println!("[VDM Server] Loopback API listening on http://{}", addr);
                    listener_opt = Some(l);
                    break;
                }
                Err(e) => {
                    let _ = &e;
                    if attempt < 5 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                    } else {
                        #[cfg(debug_assertions)]
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
                            let _ = &e;
                            #[cfg(debug_assertions)]
                            eprintln!("[VDM Server] Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    let _ = &e;
                    #[cfg(debug_assertions)]
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
                "version": env!("CARGO_PKG_VERSION")
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
                    String::from_utf8_lossy(&buffer[hend..])
                } else {
                    std::borrow::Cow::Borrowed("")
                }
            } else {
                std::borrow::Cow::Borrowed("")
            };

            if let Ok(payload) = serde_json::from_str::<DownloadPayload>(&body_str) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[test]
    fn test_origin_allowed_filtering() {
        assert!(origin_allowed(""));
        assert!(origin_allowed("chrome-extension://abcdefghijklmnop"));
        assert!(origin_allowed("moz-extension://1234-5678-90ab"));
        assert!(origin_allowed("safari-extension://some-id"));
        assert!(origin_allowed("http://localhost:8080"));
        assert!(origin_allowed("http://127.0.0.1:9191"));

        assert!(!origin_allowed("http://evil.com"));
        assert!(!origin_allowed("https://phishing.site"));
        assert!(!origin_allowed("http://localhost.evil.com"));
        assert!(!origin_allowed("http://127.0.0.1.attacker.com"));
    }

    #[tokio::test]
    async fn test_loopback_server_endpoints_and_single_instance() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        // Use an ephemeral test port
        let test_port = 19191;
        let server = Arc::new(LoopbackServer::new(test_port, tx));

        tokio::spawn(async move {
            server.run().await;
        });

        // Wait for server to bind
        tokio::time::sleep(Duration::from_millis(150)).await;

        // 1. Test GET /health
        {
            let mut stream = TcpStream::connect(format!("127.0.0.1:{}", test_port)).await.unwrap();
            stream.write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").await.unwrap();
            let mut resp = String::new();
            stream.read_to_string(&mut resp).await.unwrap();
            assert!(resp.contains("HTTP/1.1 200 OK"));
            assert!(resp.contains("\"status\":\"ok\""));
        }

        // 2. Test GET /show (single-instance wake signal)
        {
            let mut stream = TcpStream::connect(format!("127.0.0.1:{}", test_port)).await.unwrap();
            stream.write_all(b"GET /show HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://127.0.0.1\r\nConnection: close\r\n\r\n").await.unwrap();
            let mut resp = String::new();
            stream.read_to_string(&mut resp).await.unwrap();
            assert!(resp.contains("HTTP/1.1 200 OK"));
            assert!(resp.contains("Window shown"));

            let received = rx.recv().await.unwrap();
            match received {
                ServerEvent::ShowWindow => {}
                _ => panic!("Expected ShowWindow event"),
            }
        }

        // 3. Test POST /add-download with valid origin
        {
            let payload = serde_json::json!({
                "url": "https://example.com/testfile.iso",
                "fileName": "testfile.iso",
                "fileSize": 1048576,
                "referer": "https://example.com/download",
                "cookies": "sess=123"
            });
            let body = payload.to_string();
            let req = format!(
                "POST /add-download HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: chrome-extension://testextensionid\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );

            let mut stream = TcpStream::connect(format!("127.0.0.1:{}", test_port)).await.unwrap();
            stream.write_all(req.as_bytes()).await.unwrap();
            let mut resp = String::new();
            stream.read_to_string(&mut resp).await.unwrap();
            assert!(resp.contains("HTTP/1.1 200 OK"));
            assert!(resp.contains("\"success\":true"));

            let received = rx.recv().await.unwrap();
            match received {
                ServerEvent::AddDownload(p) => {
                    assert_eq!(p.url, "https://example.com/testfile.iso");
                    assert_eq!(p.filename, "testfile.iso");
                    assert_eq!(p.file_size, Some(1048576));
                    assert_eq!(p.referrer, "https://example.com/download");
                    assert_eq!(p.cookies, "sess=123");
                }
                _ => panic!("Expected AddDownload event"),
            }
        }

        // 4. Test POST /add-download with unauthorized / CSRF origin
        {
            let payload = serde_json::json!({
                "url": "https://example.com/evil.exe",
                "fileName": "evil.exe"
            });
            let body = payload.to_string();
            let req = format!(
                "POST /add-download HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil-attacker.com\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );

            let mut stream = TcpStream::connect(format!("127.0.0.1:{}", test_port)).await.unwrap();
            stream.write_all(req.as_bytes()).await.unwrap();
            let mut resp = String::new();
            stream.read_to_string(&mut resp).await.unwrap();
            assert!(resp.contains("HTTP/1.1 403 Forbidden"));
        }
    }

    #[tokio::test]
    async fn test_rapid_concurrent_secondary_instance_calls() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let test_port = 19192;
        let server = Arc::new(LoopbackServer::new(test_port, tx));

        tokio::spawn(async move {
            server.run().await;
        });

        tokio::time::sleep(Duration::from_millis(150)).await;

        let mut join_set = tokio::task::JoinSet::new();
        for _ in 0..10 {
            join_set.spawn(async move {
                let mut stream = TcpStream::connect(format!("127.0.0.1:{}", test_port)).await.unwrap();
                stream.write_all(b"GET /show HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://localhost\r\nConnection: close\r\n\r\n").await.unwrap();
                let mut resp = String::new();
                stream.read_to_string(&mut resp).await.unwrap();
                assert!(resp.contains("HTTP/1.1 200 OK"));
            });
        }

        while let Some(res) = join_set.join_next().await {
            res.unwrap();
        }

        let mut event_count = 0;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, ServerEvent::ShowWindow) {
                event_count += 1;
            }
        }
        assert_eq!(event_count, 10);
    }

    #[tokio::test]
    async fn test_loopback_server_health_version() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
        let test_port = 19193;
        let server = Arc::new(LoopbackServer::new(test_port, tx));

        tokio::spawn(async move {
            server.run().await;
        });

        tokio::time::sleep(Duration::from_millis(150)).await;

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", test_port)).await.unwrap();
        stream.write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).await.unwrap();
        assert!(resp.contains("HTTP/1.1 200 OK"));
        assert!(resp.contains(&format!("\"version\":\"{}\"", env!("CARGO_PKG_VERSION"))));
    }
}

