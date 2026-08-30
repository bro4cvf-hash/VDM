const VDM_API_PORT = 9191;
const VDM_ADD_URL = `http://127.0.0.1:${VDM_API_PORT}/add-download`;

// Initialize context menus on install
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: "vdm-download-link",
    title: "Download with VDM",
    contexts: ["link", "image", "video", "audio"]
  }, () => void chrome.runtime.lastError); // duplicate id after reload is harmless
  chrome.storage.local.get(["interceptEnabled"], (res) => {
    if (res.interceptEnabled === undefined) {
      chrome.storage.local.set({ interceptEnabled: true });
    }
  });
});

// Format cookies into a standard "name=value; name2=val2" string
async function getCookiesForUrl(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url: url });
    if (!cookies || cookies.length === 0) return "";
    return cookies.map(c => `${c.name}=${c.value}`).join("; ");
  } catch (err) {
    console.warn("[VDM] Could not extract cookies:", err);
    return "";
  }
}

// Send download payload to VDM local API
async function sendToVDM(payload) {
  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 2000);
    const res = await fetch(VDM_ADD_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
      signal: controller.signal
    });
    clearTimeout(timeoutId);
    if (!res.ok) return false;
    const data = await res.json();
    return data && data.success === true;
  } catch (err) {
    console.warn("[VDM] Local server unreachable:", err);
    return false;
  }
}

// --- Media sniffing (IDM-style): watch responses, keep per-tab media list ---
const TYPES = {
  mp4: "MP4", m4v: "M4V", webm: "WebM", mkv: "MKV", mov: "MOV", avi: "AVI",
  flv: "FLV", ogv: "OGV", mp3: "MP3", m4a: "M4A", aac: "AAC", wav: "WAV",
  ogg: "OGG", oga: "OGA", opus: "Opus", flac: "FLAC"
};
const MEDIA_EXT_RE = /\.(mp4|m4v|webm|mkv|mov|avi|flv|ogv|mp3|m4a|aac|wav|ogg|oga|opus|flac)(?:[?#]|$)/i;
const YT_ITAGS = {
  18: { quality: "360p", ext: "MP4", isAudio: false, height: 360 },
  22: { quality: "720p HD", ext: "MP4", isAudio: false, height: 720 },
  37: { quality: "1080p Full HD", ext: "MP4", isAudio: false, height: 1080 },
  38: { quality: "4K UHD", ext: "MP4", isAudio: false, height: 2160 },
  43: { quality: "360p", ext: "WebM", isAudio: false, height: 360 },
  44: { quality: "480p", ext: "WebM", isAudio: false, height: 480 },
  45: { quality: "720p HD", ext: "WebM", isAudio: false, height: 720 },
  46: { quality: "1080p Full HD", ext: "WebM", isAudio: false, height: 1080 },

  299: { quality: "1080p HD 60 fps", ext: "MP4", isAudio: false, height: 1080 },
  303: { quality: "1080p HD 60 fps", ext: "WebM", isAudio: false, height: 1080 },
  137: { quality: "1080p Full HD", ext: "MP4", isAudio: false, height: 1080 },
  248: { quality: "1080p Full HD", ext: "WebM", isAudio: false, height: 1080 },
  399: { quality: "1080p Full HD", ext: "MP4", isAudio: false, height: 1080 },

  298: { quality: "720p HD 60 fps", ext: "MP4", isAudio: false, height: 720 },
  302: { quality: "720p HD 60 fps", ext: "WebM", isAudio: false, height: 720 },
  136: { quality: "720p HD", ext: "MP4", isAudio: false, height: 720 },
  247: { quality: "720p HD", ext: "WebM", isAudio: false, height: 720 },
  398: { quality: "720p HD", ext: "MP4", isAudio: false, height: 720 },

  135: { quality: "480p", ext: "MP4", isAudio: false, height: 480 },
  244: { quality: "480p", ext: "WebM", isAudio: false, height: 480 },
  397: { quality: "480p", ext: "MP4", isAudio: false, height: 480 },

  134: { quality: "360p", ext: "MP4", isAudio: false, height: 360 },
  243: { quality: "360p", ext: "WebM", isAudio: false, height: 360 },
  396: { quality: "360p", ext: "MP4", isAudio: false, height: 360 },

  133: { quality: "240p", ext: "MP4", isAudio: false, height: 240 },
  242: { quality: "240p", ext: "WebM", isAudio: false, height: 240 },
  395: { quality: "240p", ext: "MP4", isAudio: false, height: 240 },

  160: { quality: "144p", ext: "MP4", isAudio: false, height: 144 },
  278: { quality: "144p", ext: "WebM", isAudio: false, height: 144 },
  394: { quality: "144p", ext: "MP4", isAudio: false, height: 144 },

  308: { quality: "1440p QHD 60 fps", ext: "WebM", isAudio: false, height: 1440 },
  271: { quality: "1440p QHD", ext: "WebM", isAudio: false, height: 1440 },
  400: { quality: "1440p QHD", ext: "MP4", isAudio: false, height: 1440 },

  315: { quality: "2160p 4K 60 fps", ext: "WebM", isAudio: false, height: 2160 },
  313: { quality: "2160p 4K", ext: "WebM", isAudio: false, height: 2160 },
  401: { quality: "2160p 4K", ext: "MP4", isAudio: false, height: 2160 },

  140: { quality: "128 kbps", ext: "M4A", isAudio: true, height: 0 },
  251: { quality: "160 kbps", ext: "Opus", isAudio: true, height: 0 },
  250: { quality: "70 kbps", ext: "Opus", isAudio: true, height: 0 },
  249: { quality: "50 kbps", ext: "Opus", isAudio: true, height: 0 },
  139: { quality: "48 kbps", ext: "M4A", isAudio: true, height: 0 }
};

const tabMedia = new Map(); // tabId -> Map(normalizedUrl -> item)

function tabList(tabId) {
  if (!tabMedia.has(tabId)) tabMedia.set(tabId, new Map());
  return tabMedia.get(tabId);
}

function normalizeUrl(u) {
  try {
    const p = new URL(u);
    for (const k of [...p.searchParams.keys()]) {
      if (/^(range|rn|rbuf|ump)$/i.test(k)) p.searchParams.delete(k);
    }
    return p.toString();
  } catch { return u; }
}

function qlabel(h) {
  const tag = h >= 4320 ? "8K" : h >= 2160 ? "4K" : h >= 1440 ? "QHD" : h >= 1080 ? "Full HD" : h >= 720 ? "HD" : "";
  return `${h}p${tag ? " " + tag : ""}`;
}

function ytItem(u, len) {
  const p = new URL(u).searchParams;
  const mime = (p.get("mime") || "").split(";")[0];
  const audio = mime.startsWith("audio");
  const itagNum = parseInt(p.get("itag") || "0", 10);
  const itagInfo = YT_ITAGS[itagNum];

  let type = itagInfo ? itagInfo.ext : (audio ? (mime.endsWith("mp4") ? "M4A" : "Opus") : (mime.endsWith("webm") ? "WebM" : "MP4"));
  let q = itagInfo ? itagInfo.quality : "";

  if (!q) {
    if (audio) {
      const bitrates = { "140": "128 kbps", "251": "160 kbps", "250": "70 kbps", "249": "50 kbps" };
      q = bitrates[String(itagNum)] || "128 kbps";
    } else {
      const size = p.get("size") || "";
      const h = size.includes("x") ? parseInt(size.split("x")[1], 10) : 0;
      q = h ? qlabel(h) : (itagNum ? `${itagNum}p` : "HD");
    }
  }

  const size = parseInt(p.get("clen") || 0, 10) || len;
  return { url: u, name: "", type, quality: q, size, isAudio: audio, height: itagInfo ? itagInfo.height : 0 };
}

function fileItem(u, ctype, len) {
  const m = /\.([a-z0-9]{2,5})$/i.exec(new URL(u, "http://x").pathname);
  const ext = m ? m[1].toLowerCase() : "";
  const audioExts = ["mp3", "m4a", "aac", "wav", "ogg", "oga", "opus", "flac"];
  const isAudio = audioExts.includes(ext) || /^audio\//i.test(ctype);
  const type = TYPES[ext] || (isAudio ? (ext ? ext.toUpperCase() : "MP3") : (ext ? ext.toUpperCase() : "MP4"));
  return { url: u, name: "", type, quality: isAudio ? "Audio" : "", size: len, isAudio };
}

function header(headers, name) {
  const h = headers && headers.find((x) => x.name.toLowerCase() === name);
  return h ? h.value || "" : "";
}

chrome.webRequest.onResponseStarted.addListener((details) => {
  try {
    if (details.tabId < 0) return;
    const url = details.url;
    if (!/^https?:/i.test(url) || SEGMENT_RE.test(url)) return;
    const ctype = header(details.responseHeaders, "content-type");
    const isVp = /videoplayback/.test(url);
    if (!isVp && !MEDIA_EXT_RE.test(url) && !/^(video|audio)\//i.test(ctype)) return;

    const len = parseInt(header(details.responseHeaders, "content-length"), 10) || 0;
    const norm = normalizeUrl(url);
    const item = isVp ? ytItem(norm, len) : fileItem(norm, ctype, len);
    if (!item) return;
    const list = tabList(details.tabId);
    if (list.has(norm)) return;
    list.set(norm, item);
    if (list.size > 40) list.delete(list.keys().next().value); // ponytail: FIFO trim, LRU if needed
  } catch (e) {
    console.warn("[VDM] sniff error:", e);
  }
}, { urls: ["<all_urls>"] }, ["responseHeaders"]);

chrome.tabs.onRemoved.addListener((tabId) => tabMedia.delete(tabId));
chrome.tabs.onUpdated.addListener((tabId, info) => {
  if (info.status === "loading") tabMedia.delete(tabId);
});

// Media requests from the content script video panel
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!msg) return;
  if (msg.type === "vdm-clear-media") {
    if (sender.tab && sender.tab.id >= 0) {
      tabMedia.delete(sender.tab.id);
    }
    sendResponse({ success: true });
    return;
  }
  if (msg.type === "vdm-get-media") {
    const items = sender.tab && sender.tab.id >= 0
      ? [...(tabMedia.get(sender.tab.id)?.values() || [])]
      : [];
    sendResponse({ items });
    return;
  }
  if (msg.type !== "vdm-add" || !msg.url) return;
  (async () => {
    const cookies = await getCookiesForUrl(msg.url);
    const cleanFilename = (msg.filename || "").replace(/[/\\?%*:|"<>]/g, "_").trim();
    const referrer = msg.referrer || (sender.tab && sender.tab.url) || "https://www.youtube.com/";
    const ok = await sendToVDM({
      url: msg.url,
      filename: cleanFilename,
      referrer: referrer,
      cookies: cookies,
      user_agent: navigator.userAgent,
      file_size: msg.file_size || 0
    });
    if (ok) {
      sendResponse({ success: true, vdm: true });
      return;
    }
    // Fallback: Trigger browser download directly if VDM desktop app is offline
    try {
      const headers = [];
      if (referrer) headers.push({ name: "Referer", value: referrer });
      const downloadOptions = {
        url: msg.url,
        filename: cleanFilename || undefined,
        saveAs: false,
        headers: headers.length ? headers : undefined
      };
      const dlId = await chrome.downloads.download(downloadOptions);
      if (dlId) {
        processedDownloads.add(dlId);
      }
      sendResponse({ success: true, fallback: true });
    } catch (e) {
      console.warn("[VDM] Download fallback error:", e);
      sendResponse({ success: false, error: e.message });
    }
  })();
  return true; // async response
});

// Handle Context Menu download trigger
chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const targetUrl = info.linkUrl || info.srcUrl;
  if (!targetUrl || (!targetUrl.startsWith("http://") && !targetUrl.startsWith("https://"))) {
    return;
  }
  const cookies = await getCookiesForUrl(targetUrl);
  const payload = {
    url: targetUrl,
    filename: "",
    referrer: info.pageUrl || (tab ? tab.url : ""),
    cookies: cookies,
    user_agent: navigator.userAgent
  };
  sendToVDM(payload);
});

// Avoid re-intercepting downloads created or triggered by VDM
const processedDownloads = new Set();

// Intercept browser downloads
chrome.downloads.onCreated.addListener(async (item) => {
  const { interceptEnabled } = await chrome.storage.local.get(["interceptEnabled"]);
  if (interceptEnabled === false) return;

  const url = item.finalUrl || item.url;
  if (!url || (!url.startsWith("http://") && !url.startsWith("https://"))) {
    return;
  }

  if (processedDownloads.has(item.id)) {
    return;
  }
  processedDownloads.add(item.id);
  setTimeout(() => processedDownloads.delete(item.id), 10000);

  // Pause immediately while checking if VDM is open
  try {
    await chrome.downloads.pause(item.id);
  } catch (e) {
    // Might already be progressing
  }

  let filename = item.filename ? item.filename.split(/[\\/]/).pop() : "";
  if (filename === "main" || filename === "master" || filename === "download") {
    filename = "";
  }
  const cookies = await getCookiesForUrl(url);

  const payload = {
    url: url,
    filename: filename,
    referrer: item.referrer || "",
    cookies: cookies,
    user_agent: navigator.userAgent,
    file_size: item.fileSize > 0 ? item.fileSize : null
  };

  const success = await sendToVDM(payload);
  if (success) {
    // VDM captured the download; cancel & erase from browser list
    try {
      await chrome.downloads.cancel(item.id);
      await chrome.downloads.erase({ id: item.id });
    } catch (e) {
      console.warn("[VDM] Erase item error:", e);
    }
  } else {
    // VDM offline or rejected; resume normal browser download
    try {
      await chrome.downloads.resume(item.id);
    } catch (e) {
      console.warn("[VDM] Resume item error:", e);
    }
  }
});
