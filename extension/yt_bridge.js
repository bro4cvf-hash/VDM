(() => {
  "use strict";

  function sendData(pr) {
    if (pr && pr.streamingData) {
      window.postMessage({ type: "__VDM_YT_DATA__", data: pr }, "*");
    }
  }

  // 1. Hook window.fetch to capture /youtubei/v1/player responses live
  try {
    const origFetch = window.fetch;
    window.fetch = async function(...args) {
      const response = await origFetch.apply(this, args);
      try {
        const url = typeof args[0] === "string" ? args[0] : (args[0] && args[0].url) || "";
        if (url.includes("/youtubei/v1/player")) {
          const clone = response.clone();
          clone.json().then(data => {
            sendData(data);
          }).catch(() => {});
        }
      } catch (e) {}
      return response;
    };
  } catch (e) {}

  // 2. Hook XMLHttpRequest
  try {
    const origOpen = XMLHttpRequest.prototype.open;
    const origSend = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.open = function(method, url, ...rest) {
      this.__vdmUrl = url;
      return origOpen.apply(this, [method, url, ...rest]);
    };
    XMLHttpRequest.prototype.send = function(...args) {
      this.addEventListener("load", function() {
        try {
          if (this.__vdmUrl && String(this.__vdmUrl).includes("/youtubei/v1/player")) {
            const data = JSON.parse(this.responseText);
            sendData(data);
          }
        } catch (e) {}
      });
      return origSend.apply(this, args);
    };
  } catch (e) {}

  // 3. Robust harvester from all possible YouTube DOM/Window state locations
  function harvest() {
    try {
      // Check movie_player
      const mp = document.getElementById("movie_player");
      if (mp && typeof mp.getPlayerResponse === "function") {
        const pr = mp.getPlayerResponse();
        if (pr && pr.streamingData) {
          sendData(pr);
          return;
        }
      }

      // Check ytd-watch-flexy / ytd-watch-grid
      const watchEl = document.querySelector("ytd-watch-flexy") || document.querySelector("ytd-watch-grid");
      if (watchEl && watchEl.playerData && watchEl.playerData.streamingData) {
        sendData(watchEl.playerData);
        return;
      }

      // Check window.ytInitialPlayerResponse
      if (window.ytInitialPlayerResponse && window.ytInitialPlayerResponse.streamingData) {
        sendData(window.ytInitialPlayerResponse);
        return;
      }

      // Check ytplayer.config
      if (window.ytplayer && window.ytplayer.config && window.ytplayer.config.args) {
        const raw = window.ytplayer.config.args.raw_player_response;
        if (raw) {
          const pr = typeof raw === "string" ? JSON.parse(raw) : raw;
          if (pr && pr.streamingData) {
            sendData(pr);
            return;
          }
        }
      }
    } catch (e) {}
  }

  harvest();
  document.addEventListener("yt-navigate-finish", () => setTimeout(harvest, 150));
  document.addEventListener("yt-player-updated", () => setTimeout(harvest, 150));
  document.addEventListener("yt-page-data-updated", () => setTimeout(harvest, 150));
  document.addEventListener("spfdone", () => setTimeout(harvest, 150));
  setInterval(harvest, 1000);
})();
