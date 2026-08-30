use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub fn is_youtube(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("youtube.com/watch")
        || lower.contains("youtu.be/")
        || lower.contains("youtube.com/shorts/")
        || lower.contains("youtube.com/embed/")
        || lower.contains("googlevideo.com/videoplayback")
}

pub fn find_ytdl_binary() -> Option<PathBuf> {
    // 1. Check next to current exe
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            let next_to = parent.join("yt-dlp.exe");
            if next_to.exists() {
                return Some(next_to);
            }
        }
    }

    // 2. Check AppData Python Scripts
    if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
        let programs = Path::new(&appdata).join("Programs").join("Python");
        if programs.exists() {
            if let Ok(entries) = std::fs::read_dir(programs) {
                for e in entries.flatten() {
                    let candidate = e.path().join("Scripts").join("yt-dlp.exe");
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    // 3. Check standard PATH lookup
    if let Ok(output) = std::process::Command::new("where.exe").arg("yt-dlp").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = s.lines().next() {
                let p = PathBuf::from(first_line.trim());
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    Some(PathBuf::from("yt-dlp"))
}

pub async fn extract_direct_stream_urls(
    youtube_url: &str,
    format_opt: Option<&str>,
    is_audio: bool,
) -> anyhow::Result<(String, Option<String>)> {
    let bin = find_ytdl_binary().ok_or_else(|| anyhow::anyhow!("yt-dlp binary not found"))?;

    // Watch URLs embed &itag= as a trailing filter we must drop before letting
    // yt-dlp pick the format itself; a raw googlevideo URL has &itag= *inside*
    // its query — truncating there would yield garbage.
    let clean_url = if youtube_url.contains("googlevideo.com") {
        youtube_url
    } else if let Some(idx) = youtube_url.find("&itag=") {
        &youtube_url[..idx]
    } else {
        youtube_url
    };

    let mut cmd = Command::new(&bin);
    cmd.arg("-g")
        .arg("--no-warnings");

    if is_audio {
        cmd.arg("-f").arg("bestaudio/best");
    } else if let Some(fmt) = format_opt {
        cmd.arg("-f").arg(fmt);
    } else {
        cmd.arg("-f").arg("bestvideo+bestaudio/best");
    }

    cmd.arg(clean_url);

    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let output = cmd.output().await?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("URL extraction error: {}", err.trim()));
    }

    let out_str = String::from_utf8_lossy(&output.stdout);
    let mut valid_urls = Vec::new();
    for line in out_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            valid_urls.push(trimmed.to_string());
        }
    }

    if valid_urls.is_empty() {
        return Err(anyhow::anyhow!("No playable stream URLs found"));
    }

    if valid_urls.len() >= 2 {
        Ok((valid_urls[0].clone(), Some(valid_urls[1].clone())))
    } else {
        Ok((valid_urls[0].clone(), None))
    }
}

pub fn default_youtube_headers() -> HashMap<String, String> {
    let mut h = HashMap::new();
    h.insert("User-Agent".into(), "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36".into());
    h.insert("Referer".into(), "https://www.youtube.com/".into());
    h.insert("Origin".into(), "https://www.youtube.com".into());
    h.insert("Sec-Fetch-Dest".into(), "video".into());
    h.insert("Sec-Fetch-Mode".into(), "cors".into());
    h.insert("Sec-Fetch-Site".into(), "cross-site".into());
    h.insert("Accept".into(), "*/*".into());
    h.insert("Accept-Language".into(), "en-US,en;q=0.9".into());
    h
}

pub fn find_ffmpeg_binary() -> Option<PathBuf> {
    // 1. Check next to current exe
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(parent) = exe_path.parent() {
            let next_to = parent.join("ffmpeg.exe");
            if next_to.exists() {
                return Some(next_to);
            }
        }
    }

    // 2. Check standard PATH lookup
    if let Ok(output) = std::process::Command::new("where.exe").arg("ffmpeg").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = s.lines().next() {
                let p = PathBuf::from(first_line.trim());
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    Some(PathBuf::from("ffmpeg"))
}

pub async fn mux_audio_video(
    video_path: &Path,
    audio_path: &Path,
    output_path: &Path,
) -> anyhow::Result<()> {
    let bin = find_ffmpeg_binary().unwrap_or_else(|| PathBuf::from("ffmpeg"));
    let is_webm = output_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("webm"))
        .unwrap_or(false);

    // WebM containers require Opus or Vorbis audio. MP4 containers use AAC.
    let codecs: &[&str] = if is_webm {
        &["copy", "libopus", "opus", "libvorbis"]
    } else {
        &["copy", "aac", "libmp3lame"]
    };

    let mut last_err = String::new();
    for audio_codec in codecs {
        let mut cmd = Command::new(&bin);
        cmd.arg("-y")
            .arg("-i").arg(video_path)
            .arg("-i").arg(audio_path)
            .arg("-map").arg("0:v:0")
            .arg("-map").arg("1:a:0")
            .arg("-c:v").arg("copy")
            .arg("-c:a").arg(*audio_codec)
            .arg(output_path);

        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

        let res = tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output()).await;
        match res {
            Ok(Ok(out)) => {
                if out.status.success() {
                    return Ok(());
                }
                last_err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            }
            Ok(Err(err)) => {
                last_err = err.to_string();
            }
            Err(_) => {
                last_err = "ffmpeg mux timed out after 60s".to_string();
            }
        }
    }
    Err(anyhow::anyhow!("ffmpeg mux error: {}", last_err))
}
