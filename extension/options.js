"use strict";

const api = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);
const MEDIA_ACCESS = { permissions: ["webRequest"], origins: ["<all_urls>"] };
// Same copy as the popup: where the Incognito / private-window switch lives.
const FIREFOX = /Firefox\//.test(navigator.userAgent);
const SETTINGS_PAGE = FIREFOX ? "about:addons" : "chrome://extensions";
const SETTINGS_URL = FIREFOX ? SETTINGS_PAGE : `${SETTINGS_PAGE}/?id=${api.runtime.id}`;

function renderIncognito(allowed) {
  $("incogText").classList.toggle("on", !!allowed);
  $("incogText").textContent = allowed
    ? "Private windows: on"
    : FIREFOX
      ? `Private windows: off — allow TDM in private windows in ${SETTINGS_PAGE} (one time)`
      : `Private windows: off — allow TDM in Incognito in ${SETTINGS_PAGE} (one time)`;
  $("incogBtn").hidden = !!allowed;
}
$("incogBtn").addEventListener("click", () => api.tabs.create({ url: SETTINGS_URL }).catch(() => null));

function render(s) {
  renderIncognito(s.incognitoAllowed);
  $("dot").className = "dot " + (s.connected ? "up" : "down");
  $("state").textContent = s.connected
    ? "Connected to the desktop application"
    : s.retryInMs > 0
      ? `Not connected — retrying in ${Math.ceil(s.retryInMs / 1000)}s`
      : "Not connected";
  $("enabled").checked = s.enabled;
  $("hostName").textContent = s.hostName;
  $("hostVersion").textContent = s.hostVersion || "—";
  $("extensionId").textContent = s.extensionId;
  $("captureExtensions").textContent = s.captureExtensions.join(", ") || "none (nothing is captured)";
  $("exceptionHosts").textContent = s.exceptionHosts.join(", ") || "none";
  $("lastAction").textContent = s.lastAction || "—";
  $("lastError").textContent = s.lastError || "—";
  $("grantCookies").hidden = s.hasCookieAccess;
  $("cookieState").textContent = s.hasCookieAccess ? "Allowed" : "";
  $("grantMedia").hidden = s.hasMediaAccess;
  $("revokeMedia").hidden = !s.hasMediaAccess;
  $("mediaState").textContent = s.hasMediaAccess ? "On" : "";
  hasCookieAccess = s.hasCookieAccess;
}
let hasCookieAccess = false;

async function refresh(cmd = "status", extra = {}) {
  try {
    const s = await api.runtime.sendMessage({ cmd, ...extra });
    if (s && !s.error) render(s);
  } catch (e) {
    // The background can be mid-restart; the next poll picks it up.
    $("state").textContent = "Extension is starting…";
  }
}

$("reconnect").addEventListener("click", () => refresh("reconnect"));
$("enabled").addEventListener("change", (e) => refresh("setEnabled", { value: e.target.checked }));

// permissions.request needs a user gesture, so it lives here, not in the
// background, and must be the first await (Firefox drops the gesture after one).
$("grantCookies").addEventListener("click", async () => {
  await api.permissions.request({ permissions: ["cookies"], origins: ["<all_urls>"] }).catch(() => false);
  refresh();
});
$("grantMedia").addEventListener("click", async () => {
  await api.permissions.request(MEDIA_ACCESS).catch(() => false);
  refresh();
});
// Keep <all_urls> while the cookie grant still relies on it.
$("revokeMedia").addEventListener("click", async () => {
  await api.permissions
    .remove({ permissions: ["webRequest"], origins: hasCookieAccess ? [] : ["<all_urls>"] })
    .catch(() => false);
  refresh();
});

refresh();
setInterval(refresh, 2000);
