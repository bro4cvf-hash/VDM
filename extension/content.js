(() => {
  "use strict";
  if (window.top !== window.self || window.__vdmInjected) return;
  window.__vdmInjected = true;

  const TYPES = {
    mp4: "MP4", m4v: "M4V", webm: "WebM", mkv: "MKV", mov: "MOV", avi: "AVI",
    flv: "FLV", ogv: "OGV", ts: "TS", "3gp": "3GP", mp3: "MP3", m4a: "M4A",
    aac: "AAC", wav: "WAV", ogg: "OGG", oga: "OGA", opus: "Opus", flac: "FLAC",
    vtt: "VTT", srt: "SRT", ttml: "TTML"
  };
  const AUDIO_EXTS = new Set(["mp3", "m4a", "aac", "wav", "ogg", "oga", "opus", "flac"]);
  const MEDIA_RE = /\.(mp4|m4v|webm|mkv|mov|avi|flv|ogv|ts|3gp|mp3|m4a|aac|wav|ogg|oga|opus|flac|vtt|srt)(?:[?#]|$)/i;

  const YT_ITAGS = {
    // Combined Video + Audio (Progressive) -> hasAudio: true
    18: { quality: "360p", ext: "MP4", isAudio: false, hasAudio: true, height: 360 },
    22: { quality: "720p HD", ext: "MP4", isAudio: false, hasAudio: true, height: 720 },
    37: { quality: "1080p Full HD", ext: "MP4", isAudio: false, hasAudio: true, height: 1080 },
    38: { quality: "4K UHD", ext: "MP4", isAudio: false, hasAudio: true, height: 2160 },
    43: { quality: "360p", ext: "WebM", isAudio: false, hasAudio: true, height: 360 },
    44: { quality: "480p", ext: "WebM", isAudio: false, hasAudio: true, height: 480 },
    45: { quality: "720p HD", ext: "WebM", isAudio: false, hasAudio: true, height: 720 },
    46: { quality: "1080p Full HD", ext: "WebM", isAudio: false, hasAudio: true, height: 1080 },

    // Video Only (DASH Adaptive) -> hasAudio: false
    299: { quality: "1080p HD 60 fps", ext: "MP4", isAudio: false, hasAudio: false, height: 1080 },
    303: { quality: "1080p HD 60 fps", ext: "WebM", isAudio: false, hasAudio: false, height: 1080 },
    137: { quality: "1080p Full HD", ext: "MP4", isAudio: false, hasAudio: false, height: 1080 },
    248: { quality: "1080p Full HD", ext: "WebM", isAudio: false, hasAudio: false, height: 1080 },
    399: { quality: "1080p Full HD", ext: "MP4", isAudio: false, hasAudio: false, height: 1080 },

    298: { quality: "720p HD 60 fps", ext: "MP4", isAudio: false, hasAudio: false, height: 720 },
    302: { quality: "720p HD 60 fps", ext: "WebM", isAudio: false, hasAudio: false, height: 720 },
    136: { quality: "720p HD", ext: "MP4", isAudio: false, hasAudio: false, height: 720 },
    247: { quality: "720p HD", ext: "WebM", isAudio: false, hasAudio: false, height: 720 },
    398: { quality: "720p HD", ext: "MP4", isAudio: false, hasAudio: false, height: 720 },

    135: { quality: "480p", ext: "MP4", isAudio: false, hasAudio: false, height: 480 },
    244: { quality: "480p", ext: "WebM", isAudio: false, hasAudio: false, height: 480 },
    397: { quality: "480p", ext: "MP4", isAudio: false, hasAudio: false, height: 480 },

    134: { quality: "360p", ext: "MP4", isAudio: false, hasAudio: false, height: 360 },
    243: { quality: "360p", ext: "WebM", isAudio: false, hasAudio: false, height: 360 },
    396: { quality: "360p", ext: "MP4", isAudio: false, hasAudio: false, height: 360 },

    133: { quality: "240p", ext: "MP4", isAudio: false, hasAudio: false, height: 240 },
    242: { quality: "240p", ext: "WebM", isAudio: false, hasAudio: false, height: 240 },
    395: { quality: "240p", ext: "MP4", isAudio: false, hasAudio: false, height: 240 },

    160: { quality: "144p", ext: "MP4", isAudio: false, hasAudio: false, height: 144 },
    278: { quality: "144p", ext: "WebM", isAudio: false, hasAudio: false, height: 144 },
    394: { quality: "144p", ext: "MP4", isAudio: false, hasAudio: false, height: 144 },

    308: { quality: "1440p QHD 60 fps", ext: "WebM", isAudio: false, hasAudio: false, height: 1440 },
    271: { quality: "1440p QHD", ext: "WebM", isAudio: false, hasAudio: false, height: 1440 },
    400: { quality: "1440p QHD", ext: "MP4", isAudio: false, hasAudio: false, height: 1440 },

    315: { quality: "2160p 4K 60 fps", ext: "WebM", isAudio: false, hasAudio: false, height: 2160 },
    313: { quality: "2160p 4K", ext: "WebM", isAudio: false, hasAudio: false, height: 2160 },
    401: { quality: "2160p 4K", ext: "MP4", isAudio: false, hasAudio: false, height: 2160 },

    // Audio Only
    140: { quality: "128 kbps", ext: "M4A", isAudio: true, hasAudio: true, height: 0 },
    251: { quality: "160 kbps", ext: "Opus", isAudio: true, hasAudio: true, height: 0 },
    250: { quality: "70 kbps", ext: "Opus", isAudio: true, hasAudio: true, height: 0 },
    249: { quality: "50 kbps", ext: "Opus", isAudio: true, hasAudio: true, height: 0 },
    139: { quality: "48 kbps", ext: "M4A", isAudio: true, hasAudio: true, height: 0 }
  };

  const media = new Map();     // url -> {url, name, type, quality, size, isAudio, isSubtitle, hasAudio, height, itag}
  const perVideo = new WeakMap();
  const pills = new Set();
  let currentActiveVideoId = null;

  function extOf(u) {
    try {
      const m = /\.([a-z0-9]{2,5})$/i.exec(new URL(u, location.href).pathname);
      return m ? m[1].toLowerCase() : "";
    } catch { return ""; }
  }
  function nameOf(u) {
    try {
      const p = new URL(u, location.href);
      const n = decodeURIComponent(p.pathname.split("/").pop() || p.hostname);
      return n || "media";
    } catch { return "media"; }
  }
  function qualityOf(v) {
    const h = v.videoHeight || 0;
    if (!h) return "";
    const tag = h >= 4320 ? "8K" : h >= 2160 ? "4K" : h >= 1440 ? "QHD" : h >= 1080 ? "Full HD" : h >= 720 ? "HD" : "";
    return `${h}p${tag ? " " + tag : ""}`;
  }

  function resolveMediaInfo(url, formatObj = null) {
    let itag = null;
    if (formatObj && formatObj.itag) {
      itag = parseInt(formatObj.itag, 10);
    } else {
      const m = /[?&]itag=(\d+)/.exec(url) || /\/itag\/(\d+)/.exec(url);
      if (m) itag = parseInt(m[1], 10);
    }

    const itagInfo = itag ? YT_ITAGS[itag] : null;

    let isAudio = false;
    let hasAudio = true;
    let ext = "MP4";
    let quality = "";
    let height = 0;
    let size = 0;

    if (formatObj) {
      const mime = (formatObj.mimeType || "").toLowerCase();
      isAudio = mime.startsWith("audio");
      hasAudio = isAudio || !!formatObj.audioChannels || !!formatObj.audioQuality;
      if (formatObj.contentLength) size = parseInt(formatObj.contentLength, 10);
      height = formatObj.height || 0;
      if (formatObj.fps && formatObj.qualityLabel) {
        quality = formatObj.qualityLabel;
        if (formatObj.fps > 30 && !quality.includes("fps") && !quality.includes(String(formatObj.fps))) {
          quality += ` ${formatObj.fps} fps`;
        }
      }
    }

    if (itagInfo) {
      isAudio = itagInfo.isAudio;
      hasAudio = itagInfo.hasAudio;
      ext = itagInfo.ext;
      if (!quality) quality = itagInfo.quality;
      if (!height) height = itagInfo.height;
    }

    if (!quality) {
      if (height >= 2160) quality = "2160p 4K";
      else if (height >= 1440) quality = "1440p QHD";
      else if (height >= 1080) quality = "1080p Full HD";
      else if (height >= 720) quality = "720p HD";
      else if (height > 0) quality = `${height}p`;
      else quality = isAudio ? "Audio" : "HD";
    }

    if (!ext) {
      ext = isAudio ? "MP3" : "MP4";
    }

    return { isAudio, hasAudio, ext, quality, height, size, itag };
  }

  function getYouTubeVideoId() {
    try {
      const params = new URLSearchParams(location.search);
      if (params.get("v")) return params.get("v");
      const m = /\/shorts\/([a-zA-Z0-9_-]+)/.exec(location.pathname);
      if (m) return m[1];
      const emb = /\/embed\/([a-zA-Z0-9_-]+)/.exec(location.pathname);
      if (emb) return emb[1];
    } catch {}
    return "";
  }

  function resetForNewVideo(newVidId) {
    if (newVidId && currentActiveVideoId === newVidId) return;
    currentActiveVideoId = newVidId;
    media.clear();
    try {
      chrome.runtime.sendMessage({ type: "vdm-clear-media" });
    } catch {}
    for (const p of pills) {
      if (p.video) {
        p.video.__vdmPill = null;
      }
      p.host.remove();
    }
    pills.clear();
    for (const v of document.querySelectorAll("video")) {
      v.__vdmPill = null;
    }
  }

  function addUrl(u, q, isAudio, height, hasAudio = true) {
    if (!/^https?:/i.test(u)) return;
    const info = resolveMediaInfo(u);
    const finalQuality = q || info.quality;
    const isAud = isAudio !== undefined ? isAudio : info.isAudio;
    const ext = extOf(u) ? (TYPES[extOf(u)] || extOf(u).toUpperCase()) : info.ext;
    const cur = media.get(u);
    if (cur) {
      if (finalQuality && (!cur.quality || cur.quality === "HD")) cur.quality = finalQuality;
      if (isAud) cur.isAudio = true;
      if (height && !cur.height) cur.height = height;
      return;
    }
    media.set(u, {
      url: u,
      name: nameOf(u),
      type: ext,
      quality: finalQuality,
      size: info.size || 0,
      isAudio: isAud,
      isSubtitle: false,
      hasAudio: hasAudio !== undefined ? hasAudio : info.hasAudio,
      height: height || info.height || 0,
      itag: info.itag
    });
  }

  // --- YouTube Deep Player Response Extractor (IDM-style) ---
  function parseYouTubeData(pr) {
    if (!pr || !pr.streamingData) return;
    const videoId = pr.videoDetails?.videoId || getYouTubeVideoId() || "";
    if (videoId && videoId !== currentActiveVideoId) {
      resetForNewVideo(videoId);
    }

    const title = (pr.videoDetails && pr.videoDetails.title) || document.title || "video";
    const cleanTitle = title.replace(/ - YouTube$/i, "").trim().slice(0, 80).replace(/[/\\?%*:|"<>]/g, "_");

    const allFormats = [
      ...(pr.streamingData.formats || []),
      ...(pr.streamingData.adaptiveFormats || [])
    ];

    for (const f of allFormats) {
      let streamUrl = f.url;
      if (!streamUrl && (f.signatureCipher || f.cipher)) {
        try {
          const params = new URLSearchParams(f.signatureCipher || f.cipher);
          streamUrl = params.get("url");
          const s = params.get("s");
          const sp = params.get("sp") || "sig";
          if (streamUrl && s) streamUrl += `&${sp}=${encodeURIComponent(s)}`;
        } catch (e) {}
      }

      const itag = f.itag || 0;
      const targetUrl = (streamUrl && /^https?:/i.test(streamUrl))
        ? streamUrl
        : `https://www.youtube.com/watch?v=${videoId}&itag=${itag}`;

      const info = resolveMediaInfo(targetUrl, f);

      media.set(targetUrl, {
        url: targetUrl,
        name: cleanTitle,
        type: info.ext,
        quality: info.quality,
        size: info.size,
        isAudio: info.isAudio,
        isSubtitle: false,
        hasAudio: info.hasAudio,
        height: info.height,
        itag: itag
      });
    }

    // Extract Subtitles / Captions (WebVTT)
    const captionTracks = pr.captions?.playerCaptionsTracklistRenderer?.captionTracks || [];
    for (const c of captionTracks) {
      if (c.baseUrl) {
        const lang = c.name?.simpleText || c.languageCode || "EN";
        let subUrl = c.baseUrl;
        if (!subUrl.includes("fmt=")) {
          subUrl += (subUrl.includes("?") ? "&" : "?") + "fmt=vtt";
        }
        media.set(subUrl, {
          url: subUrl,
          name: `${cleanTitle}.${lang}`,
          type: "VTT",
          quality: `${lang} Subtitles`,
          size: 0,
          isAudio: false,
          isSubtitle: true,
          hasAudio: false,
          height: -1
        });
      }
    }

    const mainVid = document.querySelector(".html5-main-video") || document.querySelector("video");
    if (mainVid) ensurePill(mainVid);
  }

  function initYouTubeExtractor() {
    if (!/youtube\.com/i.test(location.hostname)) return;

    const currentV = getYouTubeVideoId();
    if (currentV && currentV !== currentActiveVideoId) {
      resetForNewVideo(currentV);
    }

    // Extract from inline scripts rendered by YouTube
    for (const s of document.querySelectorAll("script")) {
      const txt = s.textContent || "";
      if (txt.includes("ytInitialPlayerResponse")) {
        try {
          const match = /ytInitialPlayerResponse\s*=\s*({.+?});/s.exec(txt) ||
                        /var\s+ytInitialPlayerResponse\s*=\s*({.+?});/s.exec(txt);
          if (match) parseYouTubeData(JSON.parse(match[1]));
        } catch (e) {}
      }
    }

    // Fallback standard presets for instant complete format list
    if (currentV && media.size <= 2) {
      const pageTitle = (document.title || "video").replace(/ - YouTube$/i, "").trim().slice(0, 80).replace(/[/\\?%*:|"<>]/g, "_");
      const ytPresets = [
        { itag: 401, quality: "2160p 4K 60 fps", type: "MP4", isAudio: false, hasAudio: false, height: 2160 },
        { itag: 400, quality: "1440p HD 60 fps", type: "MP4", isAudio: false, hasAudio: false, height: 1440 },
        { itag: 299, quality: "1080p HD 60 fps", type: "MP4", isAudio: false, hasAudio: false, height: 1080 },
        { itag: 22,  quality: "720p HD", type: "MP4", isAudio: false, hasAudio: true, height: 720 },
        { itag: 135, quality: "480p", type: "MP4", isAudio: false, hasAudio: false, height: 480 },
        { itag: 18,  quality: "360p", type: "MP4", isAudio: false, hasAudio: true, height: 360 },
        { itag: 251, quality: "320 kbps High Quality", type: "MP3", isAudio: true, hasAudio: true, height: 0 },
        { itag: 140, quality: "128 kbps Medium", type: "M4A", isAudio: true, hasAudio: true, height: 0 },
      ];
      for (const p of ytPresets) {
        const u = `https://www.youtube.com/watch?v=${currentV}&itag=${p.itag}`;
        if (!media.has(u)) {
          media.set(u, {
            url: u,
            name: pageTitle,
            type: p.type,
            quality: p.quality,
            size: 0,
            isAudio: p.isAudio,
            isSubtitle: false,
            hasAudio: p.hasAudio,
            height: p.height,
            itag: p.itag
          });
        }
      }
    }
  }

  function scan() {
    initYouTubeExtractor();

    for (const v of document.querySelectorAll("video")) {
      const q = qualityOf(v);
      const urls = new Set();
      if (/^https?:/i.test(v.currentSrc)) urls.add(v.currentSrc);
      if (/^https?:/i.test(v.src)) urls.add(v.src);
      for (const s of v.querySelectorAll("source")) if (/^https?:/i.test(s.src)) urls.add(s.src);
      for (const u of urls) addUrl(u, q, false, v.videoHeight || 0, true);
      perVideo.set(v, [...urls]);
      if (!v.__vdmMeta) {
        v.__vdmMeta = true;
        v.addEventListener("loadedmetadata", schedule, { once: true });
      }
    }
    for (const a of document.querySelectorAll("audio")) {
      const u = a.currentSrc || a.src;
      if (/^https?:/i.test(u)) addUrl(u, "Audio", true, 0, true);
    }

    // Only scan <a> direct media downloads on non-youtube/non-video pages
    if (!/youtube\.com|vimeo\.com|dailymotion\.com|tiktok\.com|bilibili\.com/i.test(location.hostname)) {
      for (const a of document.querySelectorAll("a[href]")) {
        if (MEDIA_RE.test(a.href)) {
          const isAud = AUDIO_EXTS.has(extOf(a.href));
          addUrl(a.href, isAud ? "Audio" : "", isAud, 0, true);
        }
      }
    }
  }

  // --- Listen to YouTube SPA navigation events ---
  window.addEventListener("message", (e) => {
    if (e.source !== window || !e.data || e.data.type !== "__VDM_YT_DATA__") return;
    parseYouTubeData(e.data.data);
  });

  const onNav = () => {
    const v = getYouTubeVideoId();
    if (v && v !== currentActiveVideoId) {
      resetForNewVideo(v);
    }
    setTimeout(sync, 200);
    setTimeout(sync, 700);
  };
  document.addEventListener("yt-navigate-finish", onNav);
  document.addEventListener("yt-player-updated", onNav);
  document.addEventListener("spfdone", onNav);
  window.addEventListener("popstate", onNav);
  window.addEventListener("hashchange", onNav);

  const STYLE = `
    :host { all: initial; position: absolute; z-index: 2147483647; pointer-events: none; }
    * { box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI Variable", "Segoe UI", system-ui, sans-serif; }
    .wrap { position: relative; pointer-events: auto; transform: translateX(calc(-100% - 10px)); }
    .wrap.edge-left { transform: none; }
    .wrap.dragging { transition: none !important; user-select: none; }

    .pill {
      display: inline-flex; align-items: center; gap: 7px; height: 34px; padding: 0 9px 0 11px;
      border-radius: 999px; cursor: grab; user-select: none; white-space: nowrap;
      background: rgba(24, 24, 27, 0.88);
      -webkit-backdrop-filter: blur(20px) saturate(1.8); backdrop-filter: blur(20px) saturate(1.8);
      border: 1px solid rgba(255, 255, 255, 0.13);
      box-shadow: 0 4px 16px rgba(0, 0, 0, 0.38), inset 0 1px 0 rgba(255, 255, 255, 0.08);
      color: #f5f5f7; font-size: 12.5px; font-weight: 590; letter-spacing: -0.01em;
      animation: pillIn 0.4s cubic-bezier(0.34, 1.4, 0.64, 1) both;
      transition: background 0.2s ease, transform 0.2s cubic-bezier(0.32, 0.72, 0, 1), opacity 0.25s ease;
      touch-action: none;
    }
    .pill:hover { background: rgba(38, 38, 42, 0.95); transform: translateY(-1px); }
    .pill:active, .wrap.dragging .pill { cursor: grabbing; }
    @keyframes pillIn { from { opacity: 0; transform: translateY(-8px) scale(0.9); } }
    .ic { width: 15px; height: 15px; fill: #32d74b; filter: drop-shadow(0 0 6px rgba(50, 215, 75, 0.45)); flex: none; pointer-events: none; }
    .pill span { pointer-events: none; }
    .x {
      display: inline-flex; align-items: center; justify-content: center;
      width: 18px; height: 18px; margin-left: 2px; border-radius: 50%;
      color: rgba(255, 255, 255, 0.55); font-size: 14px; line-height: 1; cursor: pointer; flex: none;
      pointer-events: auto !important;
      transition: background 0.2s ease, color 0.2s ease;
    }
    .x:hover { background: rgba(255, 255, 255, 0.16); color: #fff; }
    .open .pill { opacity: 0; transform: scale(0.85); pointer-events: none; }

    .panel {
      position: absolute; top: 0; right: 0; width: 380px;
      border-radius: 18px; overflow: hidden;
      background: rgba(24, 24, 27, 0.95);
      -webkit-backdrop-filter: blur(28px) saturate(1.8); backdrop-filter: blur(28px) saturate(1.8);
      border: 1px solid rgba(255, 255, 255, 0.14);
      box-shadow: 0 12px 36px rgba(0, 0, 0, 0.45), 0 28px 80px rgba(0, 0, 0, 0.6), inset 0 1px 0 rgba(255, 255, 255, 0.08);
      transform-origin: top right;
      transform: scale(0.92) translateY(-6px); opacity: 0; pointer-events: none;
      transition: transform 0.28s cubic-bezier(0.32, 0.72, 0, 1), opacity 0.22s ease;
    }
    .edge-left .panel { right: auto; left: 0; transform-origin: top left; }
    .open .panel { transform: none; opacity: 1; pointer-events: auto; }

    .head {
      display: flex; align-items: center; gap: 8px; padding: 10px 12px; cursor: grab; user-select: none;
      color: #f5f5f7; font-size: 13px; font-weight: 600; letter-spacing: -0.01em;
      border-bottom: 1px solid rgba(255, 255, 255, 0.08);
      touch-action: none;
    }
    .head:active, .wrap.dragging .head { cursor: grabbing; }
    .head-action {
      display: inline-flex; align-items: center; gap: 6px; padding: 4px 8px; border-radius: 8px;
      cursor: pointer; transition: background 0.2s ease;
    }
    .head-action:hover { background: rgba(255, 255, 255, 0.09); }
    .head-action:active { background: rgba(255, 255, 255, 0.15); }
    .head .ic2 { width: 16px; height: 16px; flex: none; pointer-events: none; }
    .head .badge {
      margin-left: auto; font-size: 11px; font-weight: 560; color: rgba(255, 255, 255, 0.55);
      background: rgba(255, 255, 255, 0.08); border-radius: 999px; padding: 2px 8px; pointer-events: none;
    }
    .head .x { margin-left: 4px; }

    .tabs {
      display: flex; gap: 4px; padding: 6px 8px; background: rgba(0, 0, 0, 0.25);
      border-bottom: 1px solid rgba(255, 255, 255, 0.07);
    }
    .tab {
      flex: 1; display: flex; align-items: center; justify-content: center; gap: 5px;
      padding: 6px 8px; border-radius: 9px; font-size: 11.5px; font-weight: 600;
      color: rgba(255, 255, 255, 0.55); cursor: pointer; user-select: none;
      border: 1px solid transparent; background: transparent;
      transition: all 0.2s ease;
    }
    .tab:hover { color: #f5f5f7; background: rgba(255, 255, 255, 0.06); }
    .tab.active {
      color: #fff; background: rgba(255, 255, 255, 0.13);
      border-color: rgba(255, 255, 255, 0.12);
      box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
    }
    .tab-badge {
      font-size: 10px; padding: 1px 6px; border-radius: 999px;
      background: rgba(255, 255, 255, 0.1); color: inherit;
    }
    .tab.active .tab-badge { background: rgba(50, 215, 75, 0.25); color: #32d74b; }

    .list { max-height: min(340px, calc(100vh - 140px)); overflow-y: auto; overscroll-behavior: contain; padding: 6px; }
    .list::-webkit-scrollbar { width: 6px; }
    .list::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.18); border-radius: 999px; }
    .list::-webkit-scrollbar-thumb:hover { background: rgba(255, 255, 255, 0.3); }

    .row {
      display: flex; align-items: center; justify-content: space-between; gap: 8px;
      padding: 7px 10px; border-radius: 11px; cursor: pointer; user-select: none;
      background: rgba(255, 255, 255, 0.035); margin-bottom: 4px;
      border: 1px solid rgba(255, 255, 255, 0.05);
      transition: background 0.18s ease, transform 0.18s cubic-bezier(0.32, 0.72, 0, 1),
                  border-color 0.18s ease;
    }
    .row:hover { background: rgba(255, 255, 255, 0.08); border-color: rgba(255, 255, 255, 0.12); }
    .row:active { transform: scale(0.99); }
    .row.sent { border-color: rgba(50, 215, 75, 0.4); background: rgba(50, 215, 75, 0.08); }
    .row.fail { border-color: rgba(255, 69, 58, 0.4); background: rgba(255, 69, 58, 0.08); }

    .row-meta { display: flex; align-items: center; gap: 6px; flex: 1; min-width: 0; flex-wrap: wrap; }
    .badge-quality {
      font-size: 11.5px; font-weight: 650; color: #fff; background: rgba(10, 132, 255, 0.22);
      border: 1px solid rgba(10, 132, 255, 0.45); padding: 2px 7px; border-radius: 6px;
      white-space: nowrap; letter-spacing: -0.01em;
    }
    .badge-quality.audio {
      background: rgba(191, 90, 242, 0.22); border-color: rgba(191, 90, 242, 0.45); color: #e5a8ff;
    }
    .badge-quality.sub {
      background: rgba(255, 159, 10, 0.22); border-color: rgba(255, 159, 10, 0.45); color: #ffb340;
    }
    .badge-ext {
      font-size: 11px; font-weight: 600; color: rgba(255, 255, 255, 0.75);
      background: rgba(255, 255, 255, 0.09); padding: 2px 6px; border-radius: 5px;
      text-transform: uppercase; letter-spacing: 0.02em;
    }

    /* Red Sound Icon Badge for Video without audio */
    .badge-sound {
      display: inline-flex; align-items: center; gap: 4px; padding: 2px 6px; border-radius: 5px;
      font-size: 10px; font-weight: 600; letter-spacing: -0.01em;
    }
    .badge-sound.no-audio {
      background: rgba(255, 69, 58, 0.18); border: 1px solid rgba(255, 69, 58, 0.45); color: #ff6961;
    }
    .badge-sound.with-audio {
      background: rgba(48, 209, 88, 0.14); border: 1px solid rgba(48, 209, 88, 0.35); color: #30d158;
    }
    .sound-ic { width: 12px; height: 12px; flex: none; }

    .item-size {
      font-size: 11px; font-weight: 500; color: rgba(235, 235, 245, 0.6); margin-left: auto; padding-right: 4px;
      white-space: nowrap;
    }

    .dl-btn {
      display: inline-flex; align-items: center; gap: 4px; height: 26px; padding: 0 9px;
      border-radius: 7px; border: 1px solid rgba(50, 215, 75, 0.35);
      background: rgba(50, 215, 75, 0.15); color: #32d74b;
      font-size: 11px; font-weight: 600; cursor: pointer; flex: none;
      transition: all 0.18s ease;
    }
    .dl-btn:hover { background: rgba(50, 215, 75, 0.28); transform: translateY(-1px); }
    .dl-btn:active { transform: scale(0.96); }
    .row.sent .dl-btn { background: #32d74b; color: #000; border-color: #32d74b; }
    .row.fail .dl-btn { background: rgba(255, 69, 58, 0.2); color: #ff453a; border-color: rgba(255, 69, 58, 0.4); }
    .btn-ic { width: 12px; height: 12px; flex: none; }
    .spin-ic { width: 12px; height: 12px; animation: spin 0.8s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

    .empty-state { padding: 28px 16px; text-align: center; font-size: 12px; color: rgba(255, 255, 255, 0.45); }

    @media (prefers-reduced-motion: reduce) {
      .pill, .panel, .row, .head, .dl-btn { animation: none; transition: none; }
    }
  `;

  const HEAD_ICON = `<svg class="ic2" viewBox="0 0 24 24" fill="none"><rect width="24" height="24" rx="7" fill="#32d74b" fill-opacity="0.18"/><path d="M12 6.5v7m0 0l-3-3m3 3l3-3" stroke="#32d74b" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><path d="M7.5 16.5h9" stroke="#32d74b" stroke-width="1.8" stroke-linecap="round"/></svg>`;

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined) n.textContent = text;
    return n;
  }

  function close(en) {
    en.open = false;
    en.wrap.classList.remove("open");
  }
  function closeAll() { for (const p of pills) if (p.open) close(p); }

  function fmtSize(b) {
    if (!b || b <= 0) return "";
    const u = ["B", "KB", "MB", "GB"];
    let i = 0;
    while (b >= 1024 && i < 3) { b /= 1024; i++; }
    return `${b >= 100 ? Math.round(b) : b.toFixed(1)} ${u[i]}`;
  }

  function setupDraggable(en, handle, onClick) {
    let pointerDown = false;
    let hasMoved = false;
    let startX = 0, startY = 0;
    let initialLeft = 0, initialTop = 0;

    handle.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      if (e.target.closest(".x") || e.target.closest(".dl-btn") || e.target.closest(".tab") || e.target.closest(".head-action")) return;

      pointerDown = true;
      hasMoved = false;
      startX = e.clientX;
      startY = e.clientY;

      const r = en.host.getBoundingClientRect();
      initialLeft = parseFloat(en.host.style.left) || (r.left + window.scrollX);
      initialTop = parseFloat(en.host.style.top) || (r.top + window.scrollY);

      try { handle.setPointerCapture(e.pointerId); } catch {}
      e.stopPropagation();
    });

    handle.addEventListener("pointermove", (e) => {
      if (!pointerDown) return;
      const dx = e.clientX - startX;
      const dy = e.clientY - startY;

      if (!hasMoved && Math.hypot(dx, dy) > 4) {
        hasMoved = true;
        en.wrap.classList.add("dragging");
      }

      if (hasMoved) {
        const newLeft = initialLeft + dx;
        const newTop = initialTop + dy;
        en.host.style.left = `${newLeft}px`;
        en.host.style.top = `${newTop}px`;

        if (en.video && en.video.isConnected) {
          const vr = en.video.getBoundingClientRect();
          en.customOffset = {
            x: newLeft - (vr.left + window.scrollX),
            y: newTop - (vr.top + window.scrollY)
          };
          en.wrap.classList.toggle("edge-left", (vr.left + en.customOffset.x) < 380);
        }
      }
    });

    const finish = (e) => {
      if (!pointerDown) return;
      pointerDown = false;
      en.wrap.classList.remove("dragging");
      try { handle.releasePointerCapture(e.pointerId); } catch {}
      if (hasMoved) {
        en.__justDragged = true;
        setTimeout(() => { en.__justDragged = false; }, 100);
      }
    };

    handle.addEventListener("pointerup", finish);
    handle.addEventListener("pointercancel", finish);

    if (onClick) {
      handle.addEventListener("click", (e) => {
        if (en.__justDragged) {
          en.__justDragged = false;
          e.stopPropagation();
          e.preventDefault();
          return;
        }
        onClick(e);
      });
    }
  }

  function deduplicateAndSort(items, activeTab) {
    const seen = new Set();
    const unique = [];

    for (const item of items) {
      const q = item.quality || "";
      const ext = item.type || "";
      const key = `${ext}_${q}`;
      if (!seen.has(key)) {
        seen.add(key);
        unique.push(item);
      }
    }

    if (activeTab === "video") {
      return unique.sort((a, b) => {
        const hA = a.height || parseInt(a.quality, 10) || 0;
        const hB = b.height || parseInt(b.quality, 10) || 0;
        if (hB !== hA) return hB - hA;
        return (b.size || 0) - (a.size || 0);
      });
    } else if (activeTab === "audio") {
      return unique.sort((a, b) => {
        const qA = parseInt(a.quality, 10) || 0;
        const qB = parseInt(b.quality, 10) || 0;
        if (qB !== qA) return qB - qA;
        return (b.size || 0) - (a.size || 0);
      });
    } else {
      return unique; // Subtitles
    }
  }

  function renderList(en, items, activeTab) {
    const list = en.panel.querySelector(".list");
    if (!list) return;
    list.innerHTML = "";

    let rawFiltered = [];
    if (activeTab === "video") {
      rawFiltered = items.filter(m => !m.isAudio && !m.isSubtitle);
    } else if (activeTab === "audio") {
      rawFiltered = items.filter(m => m.isAudio);
    } else if (activeTab === "subtitles") {
      rawFiltered = items.filter(m => m.isSubtitle);
    }

    const filtered = deduplicateAndSort(rawFiltered, activeTab);

    const badge = en.panel.querySelector(".head .badge");
    if (badge) badge.textContent = `${filtered.length} item${filtered.length === 1 ? "" : "s"}`;

    if (!filtered.length) {
      const empty = el("div", "empty-state", activeTab === "audio" ? "No audio tracks detected" : (activeTab === "subtitles" ? "No subtitles detected" : "No video formats detected"));
      list.appendChild(empty);
      return;
    }

    const pageTitle = (document.title || "").replace(/ - YouTube$/i, "").trim().slice(0, 70).replace(/[/\\?%*:|"<>]/g, "_") || "media";

    filtered.forEach((m) => {
      const isAud = m.isAudio === true;
      const isSub = m.isSubtitle === true;
      const ext = (m.type || (isSub ? "VTT" : isAud ? "MP3" : "MP4")).toUpperCase();
      const quality = m.quality || (isSub ? "Subtitle" : isAud ? "Audio" : "HD");
      const sz = fmtSize(m.size);
      const filename = `${m.name || pageTitle}.${ext.toLowerCase()}`;

      const row = el("div", "row");
      const meta = el("div", "row-meta");

      const qBadgeClass = isSub ? "badge-quality sub" : (isAud ? "badge-quality audio" : "badge-quality");
      const qBadge = el("span", qBadgeClass, quality);
      meta.appendChild(qBadge);

      const extBadge = el("span", "badge-ext", ext);
      meta.appendChild(extBadge);

      // Red sound icon indicator for video formats without audio
      if (!isAud && !isSub) {
        if (!m.hasAudio) {
          const soundBadge = el("span", "badge-sound no-audio");
          soundBadge.title = "Video Only — No Audio Track (DASH Stream)";
          soundBadge.innerHTML = `<svg class="sound-ic" viewBox="0 0 24 24" fill="none" stroke="#FF453A" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" fill="rgba(255, 69, 58, 0.2)"/><line x1="23" y1="9" x2="17" y2="15"/><line x1="17" y1="9" x2="23" y2="15"/></svg><span>No Audio</span>`;
          meta.appendChild(soundBadge);
        } else {
          const soundBadge = el("span", "badge-sound with-audio");
          soundBadge.title = "Includes Audio Track";
          soundBadge.innerHTML = `<svg class="sound-ic" viewBox="0 0 24 24" fill="none" stroke="#30D158" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" fill="rgba(48, 209, 88, 0.2)"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/></svg><span>Audio</span>`;
          meta.appendChild(soundBadge);
        }
      }

      if (sz) {
        const sizeSpan = el("span", "item-size", sz);
        meta.appendChild(sizeSpan);
      }

      row.appendChild(meta);

      const dlBtn = el("button", "dl-btn");
      dlBtn.innerHTML = `<svg class="btn-ic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg><span>Download</span>`;
      row.appendChild(dlBtn);

      const triggerDownload = (e) => {
        if (e) {
          e.stopPropagation();
          e.preventDefault();
        }
        sendOne(m.url, filename, row, dlBtn, m.size || 0);
      };

      dlBtn.addEventListener("click", triggerDownload);
      dlBtn.addEventListener("pointerdown", (e) => e.stopPropagation());
      row.addEventListener("click", (e) => {
        if (e.target.closest(".dl-btn")) return;
        triggerDownload(e);
      });
      row.addEventListener("pointerdown", (e) => e.stopPropagation());

      list.appendChild(row);
    });
  }

  function buildPanel(en) {
    const urls = perVideo.get(en.video) || [];
    const set = new Set(urls);
    for (const u of media.keys()) set.add(u);
    const allItems = [...set].map(u => media.get(u)).filter(Boolean);
    if (!allItems.length) return false;

    const videoItems = deduplicateAndSort(allItems.filter(m => !m.isAudio && !m.isSubtitle), "video");
    const audioItems = deduplicateAndSort(allItems.filter(m => m.isAudio), "audio");
    const subItems = deduplicateAndSort(allItems.filter(m => m.isSubtitle), "subtitles");

    if (!en.activeTab) {
      en.activeTab = videoItems.length ? "video" : (audioItems.length ? "audio" : "subtitles");
    }

    const pageTitle = (document.title || "").replace(/ - YouTube$/i, "").trim().slice(0, 70).replace(/[/\\?%*:|"<>]/g, "_") || "media";

    const head = el("div", "head");
    const headAction = el("div", "head-action");
    headAction.insertAdjacentHTML("afterbegin", HEAD_ICON);
    headAction.appendChild(el("span", null, "Download all"));
    head.appendChild(headAction);

    const totalCount = videoItems.length + audioItems.length + subItems.length;
    head.appendChild(el("span", "badge", `${totalCount} items`));

    const closeBtn = el("span", "x", "×");
    closeBtn.title = "Close";
    head.appendChild(closeBtn);

    const tabs = el("div", "tabs");
    const tabVideo = el("button", `tab${en.activeTab === "video" ? " active" : ""}`);
    tabVideo.innerHTML = `<span>🎬 Video</span><span class="tab-badge">${videoItems.length}</span>`;

    const tabAudio = el("button", `tab${en.activeTab === "audio" ? " active" : ""}`);
    tabAudio.innerHTML = `<span>🎵 MP3 / Audio</span><span class="tab-badge">${audioItems.length}</span>`;

    tabs.appendChild(tabVideo);
    tabs.appendChild(tabAudio);

    let tabSubs = null;
    if (subItems.length > 0) {
      tabSubs = el("button", `tab${en.activeTab === "subtitles" ? " active" : ""}`);
      tabSubs.innerHTML = `<span>💬 Subtitles</span><span class="tab-badge">${subItems.length}</span>`;
      tabs.appendChild(tabSubs);
    }

    tabVideo.addEventListener("pointerdown", (e) => e.stopPropagation());
    tabAudio.addEventListener("pointerdown", (e) => e.stopPropagation());
    if (tabSubs) tabSubs.addEventListener("pointerdown", (e) => e.stopPropagation());

    tabVideo.addEventListener("click", (e) => {
      e.stopPropagation();
      e.preventDefault();
      en.activeTab = "video";
      tabVideo.classList.add("active");
      tabAudio.classList.remove("active");
      if (tabSubs) tabSubs.classList.remove("active");
      renderList(en, allItems, "video");
    });

    tabAudio.addEventListener("click", (e) => {
      e.stopPropagation();
      e.preventDefault();
      en.activeTab = "audio";
      tabAudio.classList.add("active");
      tabVideo.classList.remove("active");
      if (tabSubs) tabSubs.classList.remove("active");
      renderList(en, allItems, "audio");
    });

    if (tabSubs) {
      tabSubs.addEventListener("click", (e) => {
        e.stopPropagation();
        e.preventDefault();
        en.activeTab = "subtitles";
        tabSubs.classList.add("active");
        tabVideo.classList.remove("active");
        tabAudio.classList.remove("active");
        renderList(en, allItems, "subtitles");
      });
    }

    const list = el("div", "list");

    headAction.addEventListener("click", (e) => {
      e.stopPropagation();
      let currentFiltered = [];
      if (en.activeTab === "video") currentFiltered = videoItems;
      else if (en.activeTab === "audio") currentFiltered = audioItems;
      else if (en.activeTab === "subtitles") currentFiltered = subItems;
      sendAll(currentFiltered.length ? currentFiltered : allItems, pageTitle);
    });

    closeBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      close(en);
    });

    setupDraggable(en, head, null);

    en.panel.replaceChildren(head, tabs, list);
    renderList(en, allItems, en.activeTab);
    return true;
  }

  async function directBrowserDownload(url, filename) {
    try {
      const res = await fetch(url, { credentials: "include" });
      if (res.ok) {
        const blob = await res.blob();
        const blobUrl = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = blobUrl;
        a.download = filename || "download";
        document.body.appendChild(a);
        a.click();
        setTimeout(() => {
          a.remove();
          URL.revokeObjectURL(blobUrl);
        }, 8000);
        return;
      }
    } catch (e) {}

    try {
      const a = document.createElement("a");
      a.href = url;
      a.download = filename || "download";
      a.target = "_blank";
      a.rel = "noreferrer noopener";
      document.body.appendChild(a);
      a.click();
      setTimeout(() => a.remove(), 1000);
    } catch (e) {
      console.warn("[VDM] Direct trigger error:", e);
    }
  }

  function sendOne(url, filename, row, btn, fileSize) {
    if (btn) {
      btn.disabled = true;
      btn.innerHTML = `<svg class="spin-ic" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" fill="none" stroke-dasharray="28" stroke-dashoffset="10"/></svg><span>Saving...</span>`;
    }

    try {
      chrome.runtime.sendMessage({
        type: "vdm-add",
        url,
        filename,
        file_size: fileSize || 0,
        referrer: location.href
      }, (res) => {
        const ok = !!(res && (res.success || res.fallback));
        if (ok) {
          if (row) row.classList.add("sent");
          if (btn) btn.innerHTML = `<svg class="btn-ic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg><span>Downloading</span>`;
        } else {
          directBrowserDownload(url, filename);
          if (row) row.classList.add("sent");
          if (btn) btn.innerHTML = `<svg class="btn-ic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg><span>Started</span>`;
        }
        setTimeout(() => {
          if (row) row.classList.remove("sent", "fail");
          if (btn) {
            btn.disabled = false;
            btn.innerHTML = `<svg class="btn-ic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg><span>Download</span>`;
          }
        }, 2200);
      });
    } catch {
      directBrowserDownload(url, filename);
      if (row) row.classList.add("sent");
      if (btn) {
        btn.innerHTML = `<span>Started</span>`;
        setTimeout(() => {
          if (row) row.classList.remove("sent", "fail");
          btn.disabled = false;
          btn.innerHTML = `<svg class="btn-ic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg><span>Download</span>`;
        }, 2200);
      }
    }
  }

  function sendAll(items, fallbackName) {
    items.forEach((m, i) => setTimeout(() => {
      try {
        const isAud = m.isAudio === true;
        const isSub = m.isSubtitle === true;
        const ext = (m.type || (isSub ? "VTT" : isAud ? "MP3" : "MP4")).toLowerCase();
        const fn = `${m.name || fallbackName}.${ext}`;
        sendOne(m.url, fn, null, null, m.size || 0);
      } catch {}
    }, i * 200));
  }

  function ensurePill(video) {
    const isYT = /youtube\.com|youtu\.be/i.test(location.hostname);
    if (isYT && !video.classList.contains("html5-main-video")) {
      const mainVid = document.querySelector(".html5-main-video");
      if (mainVid && mainVid !== video) return;
    }

    const urls = perVideo.get(video);
    if (!urls || !urls.length) {
      if (media.size > 0) perVideo.set(video, [...media.keys()]);
      else return;
    }
    if (video.__vdmPill) {
      if (video.__vdmPill.host && video.__vdmPill.host.isConnected) {
        place(video.__vdmPill);
        return;
      } else {
        if (video.__vdmPill.host) video.__vdmPill.host.remove();
        pills.delete(video.__vdmPill);
        video.__vdmPill = null;
      }
    }

    const host = el("div", "host");
    const root = host.attachShadow({ mode: "open" });
    root.innerHTML = `<style>${STYLE}</style>
      <div class="wrap">
        <div class="pill" role="button" tabindex="0">
          <svg class="ic" viewBox="0 0 24 24"><path d="M8.2 5.6a1 1 0 0 1 1.52-.86l9.2 6.4a1 1 0 0 1 0 1.72l-9.2 6.4a1 1 0 0 1-1.52-.86z"/></svg>
          <span>Download this video</span>
          <span class="x" title="Dismiss">×</span>
        </div>
        <div class="panel"></div>
      </div>`;
    document.documentElement.appendChild(host);

    const wrap = root.querySelector(".wrap");
    const pill = root.querySelector(".pill");
    const en = { host, root, wrap, panel: root.querySelector(".panel"), video, open: false, customOffset: null, activeTab: null };
    video.__vdmPill = en;
    pills.add(en);

    wrap.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
    });

    setupDraggable(en, pill, (e) => {
      if (e.target.closest(".x")) {
        host.remove();
        video.__vdmPill = null;
        pills.delete(en);
        return;
      }
      if (!en.open && buildPanel(en)) {
        closeAll();
        en.open = true;
        wrap.classList.add("open");
      } else if (en.open) {
        close(en);
      }
    });

    pill.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") { e.preventDefault(); pill.click(); }
    });

    place(en);
  }

  function place(en) {
    if (!en.video || !en.video.isConnected) {
      if (en.host) en.host.remove();
      pills.delete(en);
      if (en.video) en.video.__vdmPill = null;
      return;
    }
    const r = en.video.getBoundingClientRect();
    const visible = r.width >= 120 && r.height >= 80 &&
      r.bottom > 20 && r.top < window.innerHeight && r.right > 20 && r.left < window.innerWidth;
    en.host.style.display = visible ? "block" : "none";
    if (!visible) { if (en.open) close(en); return; }

    if (en.customOffset) {
      const left = r.left + window.scrollX + en.customOffset.x;
      const top = r.top + window.scrollY + en.customOffset.y;
      en.host.style.left = `${left}px`;
      en.host.style.top = `${top}px`;
      en.wrap.classList.toggle("edge-left", (r.left + en.customOffset.x) < 380);
    } else {
      en.host.style.left = `${Math.min(r.right + window.scrollX, window.scrollX + window.innerWidth - 8)}px`;
      en.host.style.top = `${Math.max(r.top + window.scrollY + 6, window.scrollY + 6)}px`;
      en.wrap.classList.toggle("edge-left", r.left < 380);
    }
  }

  function sync() {
    scan();
    try {
      chrome.runtime.sendMessage({ type: "vdm-get-media" }, (res) => {
        if (chrome.runtime.lastError) { finishSync(null); return; }
        finishSync(res && res.items);
      });
    } catch { finishSync(null); }
  }

  function finishSync(sniffed) {
    if (sniffed && sniffed.length) {
      for (const it of sniffed) {
        if (/^https?:/i.test(it.url) && !media.has(it.url)) {
          media.set(it.url, {
            url: it.url,
            name: it.name || "",
            type: it.type,
            quality: it.quality || "",
            size: it.size || 0,
            isAudio: it.isAudio || false,
            isSubtitle: false,
            hasAudio: it.hasAudio !== undefined ? it.hasAudio : true,
            height: it.height || 0
          });
        }
      }
      let big = null, area = 0;
      for (const v of document.querySelectorAll("video")) {
        const r = v.getBoundingClientRect();
        if (r.width * r.height > area) { area = r.width * r.height; big = v; }
      }
      if (big) {
        perVideo.set(big, [...new Set([...(perVideo.get(big) || []), ...sniffed.map((s) => s.url)])]);
      }
    }
    const isYT = /youtube\.com|youtu\.be/i.test(location.hostname);
    if (isYT) {
      const mainVid = document.querySelector(".html5-main-video") ||
                      [...document.querySelectorAll("video")].sort((a, b) => {
                        const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
                        return (rb.width * rb.height) - (ra.width * ra.height);
                      })[0];
      if (mainVid) {
        for (const p of [...pills]) {
          if (p.video !== mainVid) {
            p.host.remove();
            pills.delete(p);
          }
        }
        ensurePill(mainVid);
      }
    } else {
      for (const v of document.querySelectorAll("video")) ensurePill(v);
    }
    for (const p of [...pills]) {
      if (!p.video.isConnected) { p.host.remove(); pills.delete(p); }
      else place(p);
    }
  }

  let timer = 0;
  function schedule() { clearTimeout(timer); timer = setTimeout(sync, 600); }

  let raf = 0;
  function onMove() {
    if (raf) return;
    raf = requestAnimationFrame(() => { raf = 0; for (const p of pills) place(p); });
  }

  document.addEventListener("pointerdown", (e) => {
    const path = e.composedPath ? e.composedPath() : [];
    for (const p of pills) {
      if (p.open) {
        const isInside = (p.host && path.includes(p.host)) ||
                         (p.wrap && path.includes(p.wrap)) ||
                         (p.root && path.includes(p.root)) ||
                         (e.target && p.host && p.host.contains(e.target));
        if (!isInside) {
          close(p);
        }
      }
    }
  }, true);
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeAll(); });

  addEventListener("scroll", onMove, { passive: true });
  addEventListener("resize", onMove);
  setInterval(() => { if (!document.hidden) for (const p of pills) place(p); }, 400);
  addEventListener("load", schedule);
  new MutationObserver((muts) => {
    if (muts.some((m) => m.addedNodes.length)) schedule();
  }).observe(document.documentElement, { childList: true, subtree: true });

  sync();
})();
