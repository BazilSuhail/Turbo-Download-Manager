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

// Optional override of the app's minimum size, in KiB. Empty removes it, so the
// app's own setting applies again.
const MIN_KIB_KEY = "captureMinKib";
api.storage.local
  .get(MIN_KIB_KEY)
  .then((v) => {
    if (typeof v[MIN_KIB_KEY] === "number") $("captureMinKib").value = v[MIN_KIB_KEY];
  })
  .catch(() => null);
$("captureMinKib").addEventListener("change", (e) => {
  const text = e.target.value.trim();
  const kib = Number(text);
  if (text === "" || !Number.isFinite(kib)) {
    e.target.value = "";
    api.storage.local.remove(MIN_KIB_KEY).catch(() => null);
  } else {
    const floored = Math.max(0, Math.floor(kib));
    e.target.value = floored;
    api.storage.local.set({ [MIN_KIB_KEY]: floored }).catch(() => null);
  }
});

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

// Sites where media detection runs, stored as origins under "mediaSites".
// Empty by default (no detection). Unchecking the box clears the list.
const MEDIA_SITES_KEY = "mediaSites";
const mediaSitesOn = $("mediaSitesOn");
const mediaSitesText = $("mediaSites");

// One site per line; a bare host gets https://. Anything that is not a web
// address is dropped. The result is origins, which is what the background matches.
function parseOrigins(text) {
  const out = new Set();
  for (const line of text.split(/\r?\n/)) {
    const v = line.trim();
    if (!v) continue;
    try {
      const u = new URL(/^https?:\/\//i.test(v) ? v : `https://${v}`);
      if (u.protocol === "http:" || u.protocol === "https:") out.add(u.origin);
    } catch (_) {
      /* not a site; skipped */
    }
  }
  return [...out];
}

api.storage.local
  .get(MEDIA_SITES_KEY)
  .then((v) => {
    const list = Array.isArray(v[MEDIA_SITES_KEY]) ? v[MEDIA_SITES_KEY] : [];
    mediaSitesText.value = list.join("\n");
    mediaSitesOn.checked = list.length > 0;
    mediaSitesText.disabled = !mediaSitesOn.checked;
  })
  .catch(() => null);

function saveMediaSites() {
  mediaSitesText.disabled = !mediaSitesOn.checked;
  if (!mediaSitesOn.checked) mediaSitesText.value = "";
  const list = mediaSitesOn.checked ? parseOrigins(mediaSitesText.value) : [];
  if (mediaSitesOn.checked) mediaSitesText.value = list.join("\n");
  api.storage.local.set({ [MEDIA_SITES_KEY]: list }).catch(() => null);
}
mediaSitesOn.addEventListener("change", saveMediaSites);
mediaSitesText.addEventListener("change", saveMediaSites);

refresh();
setInterval(refresh, 2000);
