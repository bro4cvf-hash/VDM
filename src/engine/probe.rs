//! pre-flight inspection: ONE ranged GET (bytes=0-0) tells us
//! total size + range support (+ probe is lighter/more reliable than HEAD across CDNs)
use reqwest::{Client, Method, Request, Url};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Probe {
    pub total: Option<u64>,
    pub accept_ranges: bool,
    pub etag: String,
    pub last_modified: String,
    pub filename_hint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MagnetInfo {
    pub info_hash: String,
    pub display_name: Option<String>,
    pub total_size: Option<u64>,
    pub trackers: Vec<String>,
}

pub fn parse_magnet(raw: &str) -> Option<MagnetInfo> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("magnet:?") {
        return None;
    }
    let query = &trimmed["magnet:?".len()..];
    let mut info = MagnetInfo::default();

    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let val = parts.next().unwrap_or("").trim();
        let decoded = percent_decode(val);

        match key.as_str() {
            "xt" => {
                let dec_lower = decoded.to_ascii_lowercase();
                if let Some(pos) = dec_lower.find("urn:btih:") {
                    info.info_hash = decoded[pos + "urn:btih:".len()..].to_string();
                } else if let Some(pos) = dec_lower.find("urn:btmh:") {
                    info.info_hash = decoded[pos + "urn:btmh:".len()..].to_string();
                } else {
                    info.info_hash = decoded;
                }
            }
            "dn" => {
                if !decoded.is_empty() {
                    info.display_name = Some(decoded);
                }
            }
            "xl" => {
                if let Ok(bytes) = decoded.parse::<u64>() {
                    info.total_size = Some(bytes);
                }
            }
            "tr" => {
                if !decoded.is_empty() {
                    info.trackers.push(decoded);
                }
            }
            _ => {}
        }
    }

    if info.info_hash.is_empty() && info.display_name.is_none() {
        return None;
    }
    Some(info)
}

fn cd_filename(v: &str) -> Option<String> {
    // RFC 5987 first, then plain filename=
    for part in v.split(';') {
        let p = part.trim();
        let p_lower = p.to_ascii_lowercase();
        if let Some(pos) = p_lower.find("filename*=") {
            let rest = &p[pos + "filename*=".len()..];
            if let Some(name) = rest.rsplit("''").next() {
                let clean = name.trim_matches('"').trim();
                let decoded = percent_decode(clean);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
    }
    for part in v.split(';') {
        let p = part.trim();
        let p_lower = p.to_ascii_lowercase();
        if let Some(pos) = p_lower.find("filename=") {
            let rest = &p[pos + "filename=".len()..];
            let clean = rest.trim_matches('"').trim();
            let decoded = percent_decode(clean);
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
    }
    None
}

pub fn extension_from_mime(mime: &str) -> Option<&'static str> {
    let lower = mime.to_ascii_lowercase();
    let main_type = lower.split(';').next().unwrap_or("").trim();
    match main_type {
        "application/zip" | "application/x-zip-compressed" | "application/x-zip" => Some(".zip"),
        "application/x-rar-compressed" | "application/vnd.rar" | "application/x-rar" => Some(".rar"),
        "application/x-7z-compressed" | "application/x-7z" => Some(".7z"),
        "application/x-tar" => Some(".tar"),
        "application/gzip" | "application/x-gzip" => Some(".tar.gz"),
        "application/x-msdownload" | "application/x-msdos-program" | "application/x-exe" | "application/exe" => Some(".exe"),
        "application/x-msi" => Some(".msi"),
        "application/x-iso9660-image" | "application/x-iso-image" => Some(".iso"),
        "application/pdf" => Some(".pdf"),
        "video/mp4" => Some(".mp4"),
        "video/x-matroska" => Some(".mkv"),
        "video/webm" => Some(".webm"),
        "video/quicktime" => Some(".mov"),
        "video/x-msvideo" => Some(".avi"),
        "audio/mpeg" | "audio/mp3" => Some(".mp3"),
        "audio/wav" | "audio/x-wav" => Some(".wav"),
        "audio/flac" | "audio/x-flac" => Some(".flac"),
        "audio/aac" => Some(".aac"),
        "image/jpeg" => Some(".jpg"),
        "image/png" => Some(".png"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        "image/svg+xml" => Some(".svg"),
        "application/vnd.android.package-archive" => Some(".apk"),
        "application/epub+zip" => Some(".epub"),
        _ => None,
    }
}

pub fn infer_filename_from_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.starts_with("magnet:?") {
        if let Some(mag) = parse_magnet(trimmed) {
            if let Some(dn) = mag.display_name {
                if !dn.is_empty() {
                    return Some(dn);
                }
            }
            if !mag.info_hash.is_empty() {
                let short = &mag.info_hash[..mag.info_hash.len().min(10)];
                return Some(format!("torrent_{short}"));
            }
        }
        return Some("torrent_download".into());
    }

    let u = Url::parse(raw).ok()?;

    // 1. Query parameters
    for (k, v) in u.query_pairs() {
        let k_lower = k.to_ascii_lowercase();
        if (k_lower == "filename" || k_lower == "file" || k_lower == "name" || k_lower == "response-content-disposition") && v.contains('.') {
            if k_lower == "response-content-disposition" {
                if let Some(cd_name) = cd_filename(&v) {
                    return Some(cd_name);
                }
            }
            return Some(v.into_owned());
        }
        if k_lower.contains("content-disposition") {
            if let Some(cd_name) = cd_filename(&v) {
                return Some(cd_name);
            }
        }
    }

    // 2. GitHub / GitLab / Git repository archive URLs
    if let Some(segments) = u.path_segments() {
        let segs: Vec<&str> = segments.filter(|s| !s.is_empty()).collect();
        if let Some(host) = u.host_str() {
            let host_lower = host.to_ascii_lowercase();
            if host_lower.contains("github.com") || host_lower.contains("codeload") || host_lower.contains("gitlab.com") {
                if segs.len() >= 2 && (segs.iter().any(|s| *s == "archive" || *s == "zip" || *s == "tar.gz" || s.ends_with(".zip") || s.ends_with(".tar.gz"))) {
                    let repo = segs.get(1).copied().unwrap_or("repo");
                    let last_seg = segs.last().copied().unwrap_or("main");
                    let clean_branch = last_seg.trim_end_matches(".zip").trim_end_matches(".tar.gz");
                    let is_tar = segs.iter().any(|s| s.contains("tar"));
                    let ext = if is_tar { ".tar.gz" } else { ".zip" };
                    return Some(format!("{}-{}{}", repo, clean_branch, ext));
                }
            }
        }
        if let Some(last) = segs.last() {
            let decoded = percent_decode(&last.replace('+', "%20"));
            if !decoded.is_empty() {
                return Some(decoded);
            }
        }
        if segs.is_empty() {
            return Some(String::new());
        }
    }

    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(n);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// extract terminal path segment of URL for default name
pub fn url_basename(raw: &str) -> Option<String> {
    infer_filename_from_url(raw)
}

pub fn sanitize_name(name: &str) -> String {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\u{0}'..='\u{1f}' => '_',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').trim();
    let stem_upper = trimmed.split(['.', '-']).next().unwrap_or("").to_ascii_uppercase();
    let safe = if trimmed.is_empty() || RESERVED.contains(&stem_upper.as_str()) {
        "download".to_string()
    } else {
        trimmed.to_string()
    };
    if safe.chars().count() > 200 {
        safe.chars().take(200).collect()
    } else {
        safe
    }
}

pub async fn probe(
    client: &Client,
    url: &str,
    extra_headers: &HashMap<String, String>,
) -> anyhow::Result<Probe> {
    let trimmed = url.trim();
    if trimmed.starts_with("magnet:?") {
        let mag = parse_magnet(trimmed).unwrap_or_default();
        let fname = mag.display_name.or_else(|| {
            if !mag.info_hash.is_empty() {
                let short = &mag.info_hash[..mag.info_hash.len().min(10)];
                Some(format!("torrent_{short}"))
            } else {
                Some("torrent_download".into())
            }
        });
        return Ok(Probe {
            total: mag.total_size,
            accept_ranges: true,
            etag: String::new(),
            last_modified: String::new(),
            filename_hint: fname,
        });
    }

    let is_sub_or_unranged = url.contains("timedtext") || url.ends_with(".vtt") || url.ends_with(".srt");

    let resp = if is_sub_or_unranged {
        let mut plain = Request::new(Method::GET, Url::parse(url)?);
        *plain.headers_mut() = headers_from(extra_headers);
        client.execute(plain).await?
    } else {
        let mut build = Request::new(Method::GET, Url::parse(url)?);
        *build.headers_mut() = headers_from(extra_headers);
        build.headers_mut().insert("Range", "bytes=0-0".parse()?);

        match client.execute(build).await {
            Ok(r) if r.status().is_success() || r.status() == reqwest::StatusCode::PARTIAL_CONTENT => r,
            _ => {
                // Fallback: try standard GET without Range header
                let mut plain = Request::new(Method::GET, Url::parse(url)?);
                *plain.headers_mut() = headers_from(extra_headers);
                let r2 = client.execute(plain).await?;
                if !r2.status().is_success() {
                    return Err(anyhow::anyhow!("server returned status {}", r2.status()));
                }
                r2
            }
        }
    };

    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let last_modified = resp
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let cd = resp
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .and_then(cd_filename);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let content_length = resp.content_length();
    let server_accepts_ranges = resp
        .headers()
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("bytes"))
        .unwrap_or(false);

    let final_url = resp.url().as_str().to_string();

    // 206 partial => ranges supported; Content-Range gives authoritative total
    let mut probe = match resp.status() {
        s if s == reqwest::StatusCode::PARTIAL_CONTENT => {
            let cr = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let total = cr.rsplit('/').next().and_then(|t| t.parse::<u64>().ok());
            Probe { total, accept_ranges: true, etag, last_modified, filename_hint: cd }
        }
        s if s == reqwest::StatusCode::RANGE_NOT_SATISFIABLE => {
            // some servers reject 0-0 but accept offsets > 0; fall through as ranged=false
            Probe { total: None, accept_ranges: false, etag, last_modified, filename_hint: cd }
        }
        _ /* 200 OK etc. */ => {
            Probe {
                total: content_length,
                accept_ranges: server_accepts_ranges && content_length.is_some(),
                etag,
                last_modified,
                filename_hint: cd,
            }
        }
    };

    if probe.filename_hint.is_none() {
        probe.filename_hint = infer_filename_from_url(&final_url).or_else(|| infer_filename_from_url(url));
    }

    // If filename has no extension, append based on MIME type
    if let Some(ref mut name) = probe.filename_hint {
        if !name.contains('.') {
            if let Some(ext) = extension_from_mime(&content_type) {
                name.push_str(ext);
            }
        }
    }

    Ok(probe)
}

pub fn headers_from(map: &HashMap<String, String>) -> reqwest::header::HeaderMap {
    let mut hm = reqwest::header::HeaderMap::new();
    for (k, v) in map {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            hm.insert(name, val);
        }
    }
    hm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_parsing_and_sanitize() {
        assert_eq!(
            cd_filename("attachment; filename=\"setup v2.exe\"; size=10"),
            Some("setup v2.exe".into())
        );
        assert_eq!(
            cd_filename("attachment; filename*=UTF-8''na%C3%AFve%20file.zip"),
            Some("na\u{ef}ve file.zip".into())
        );
        let s = sanitize_name("CON");
        assert_eq!(s, "download");
        let s = sanitize_name("bad:name?.zip");
        assert!(!s.contains(':') && !s.contains('?'));
        assert_eq!(sanitize_name("  trailing...  "), "trailing");
    }

    #[test]
    fn url_basename_decodes() {
        assert_eq!(url_basename("https://x.io/a/b/My%20File.zip?token=1"), Some("My File.zip".into()));
        assert_eq!(url_basename("https://x.io/"), Some(String::new()));
        assert_eq!(
            url_basename("https://codeload.github.com/FlowiseAI/Flowise/zip/refs/heads/main"),
            Some("Flowise-main.zip".into())
        );
        assert_eq!(
            url_basename("https://github.com/FlowiseAI/Flowise/archive/refs/heads/main.zip"),
            Some("Flowise-main.zip".into())
        );
        assert_eq!(
            url_basename("https://github.com/FlowiseAI/Flowise/archive/main.zip"),
            Some("Flowise-main.zip".into())
        );
    }
}


