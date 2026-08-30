const VDM_HEALTH_URL = "http://127.0.0.1:9191/health";

document.addEventListener("DOMContentLoaded", async () => {
  const statusDot = document.getElementById("statusDot");
  const statusText = document.getElementById("statusText");
  const interceptToggle = document.getElementById("interceptToggle");

  // Load toggle setting
  chrome.storage.local.get(["interceptEnabled"], (res) => {
    interceptToggle.checked = res.interceptEnabled !== false;
  });

  interceptToggle.addEventListener("change", () => {
    chrome.storage.local.set({ interceptEnabled: interceptToggle.checked });
  });

  // Check health of VDM loopback server
  try {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 1200);
    const res = await fetch(VDM_HEALTH_URL, { signal: controller.signal });
    clearTimeout(timeout);
    if (res.ok) {
      const data = await res.json();
      statusDot.className = "dot connected";
      statusText.textContent = `Connected (${data.version || "v0.1.0"})`;
      statusText.style.color = "var(--green)";
    } else {
      throw new Error("HTTP error " + res.status);
    }
  } catch (err) {
    statusDot.className = "dot disconnected";
    statusText.textContent = "VDM Offline";
    statusText.style.color = "var(--red)";
  }
});
