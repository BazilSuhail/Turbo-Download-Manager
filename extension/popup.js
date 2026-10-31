import "./media-core.js"; // defines globalThis.TdmMedia

const { filenameFromTitle } = globalThis.TdmMedia;
const api = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);
const MEDIA_ACCESS = { permissions: ["webRequest"], origins: ["<all_urls>"] };
// Where the user flips the Incognito / private-window switch for this extension.
const FIREFOX = /Firefox\//.test(navigator.userAgent);
const SETTINGS_PAGE = FIREFOX ? "about:addons" : "chrome://extensions";
const SETTINGS_URL = FIREFOX ? SETTINGS_PAGE : `${SETTINGS_PAGE}/?id=${api.runtime.id}`;
// Firefox's host-not-registered error; Chromium says "... host not found."
const NOT_INSTALLED = /not found|no such native application/i;

// The background may be mid-restart; treat that as "no answer yet".
const send = (msg) => api.runtime.sendMessage(msg).catch(() => null);

// ------------------------------------------------------------------ status

function renderStatus(s) {
  if (!s) {
    $("state").textContent = "Extension is starting…";
    $("dot").className = "dot";
    return;
  }
  const tone = s.connected ? "up" : NOT_INSTALLED.test(s.lastError || "") ? "down" : "wait";
  $("dot").className = "dot " + tone;
  $("state").textContent = { up: "Connected", down: "Not installed", wait: "Waiting for the app" }[tone];
  $("state").parentElement.title = s.lastError || (s.retryInMs > 0 ? `Retrying in ${Math.ceil(s.retryInMs / 1000)}s` : "");
  $("reconnect").hidden = s.connected;
  $("enabled").checked = s.enabled;
  $("hostVersion").textContent = s.hostVersion ? `TDM ${s.hostVersion}` : "";
  $("grant").hidden = s.hasMediaAccess;
  renderIncognito(s.incognitoAllowed);
}

function renderIncognito(allowed) {
  $("incogDot").className = "dot " + (allowed ? "up" : "");
  const text = $("incogText");
  text.classList.toggle("on", !!allowed);
  text.textContent = allowed
    ? "Private windows: on"
    : FIREFOX
      ? `Private windows: off — allow TDM in private windows in ${SETTINGS_PAGE} (one time)`
      : `Private windows: off — allow TDM in Incognito in ${SETTINGS_PAGE} (one time)`;
  $("incogBtn").hidden = !!allowed;
}

const refresh = async (cmd = "status", extra = {}) => renderStatus(await send({ cmd, ...extra }));

$("reconnect").addEventListener("click", () => refresh("reconnect"));
$("enabled").addEventListener("change", (e) => refresh("setEnabled", { value: e.target.checked }));
$("incogBtn").addEventListener("click", () => {
  api.tabs.create({ url: SETTINGS_URL }).catch(() => null);
  window.close();
});
$("options").addEventListener("click", (e) => {
  e.preventDefault();
  api.runtime.openOptionsPage();
  window.close();
});
// permissions.request must be the first await in the gesture (Firefox). The
// background reacts to the grant itself, so this works even if the browser
// closes the popup while its prompt is showing.
$("grantMedia").addEventListener("click", async () => {
  const granted = await api.permissions.request(MEDIA_ACCESS).catch(() => false);
  await refresh();
  if (granted) showEmpty("Media detection is on. Reload the page so TDM can watch it.");
});

// ------------------------------------------------------------------- media

const [tab] = await api.tabs.query({ active: true, currentWindow: true });

// Finds <video>/<audio> in the page right now. Opening the popup grants
// activeTab, so this needs no host permission.
async function scanPage() {
  if (!tab || !api.scripting) return null;
  const run = async (target) => {
    await api.scripting.executeScript({ target, files: ["media-core.js"] });
    const results = await api.scripting.executeScript({ target, func: () => globalThis.TdmMedia.scan(document) });
    return results.map((r) => r.result).filter(Boolean);
  };
  try {
    return await run({ tabId: tab.id, allFrames: true });
  } catch (_) {
    try {
      return await run({ tabId: tab.id }); // a cross-origin frame refused; the top frame may not
    } catch (_) {
      return null; // browser-internal page, web store, PDF viewer…
    }
  }
}

function label(url, title) {
  const fromTitle = filenameFromTitle(title, url);
  if (fromTitle) return fromTitle;
  const u = new URL(url);
  const last = u.pathname.split("/").filter(Boolean).pop() || u.hostname;
  try {
    return decodeURIComponent(last);
  } catch (_) {
    return last;
  }
}

function showEmpty(text) {
  $("empty").textContent = text;
  $("empty").hidden = false;
}

async function download(button, item, title) {
  button.disabled = true;
  button.textContent = "Sending…";
  const reply = await send({ cmd: "downloadMedia", url: item.url, pageUrl: tab && tab.url, title });
  const result = reply && reply.result;
  button.disabled = false;
  button.className = "btn " + (result === "accepted" ? "ok" : "err");
  button.textContent =
    result === "accepted" ? "Sent ✓" : result === "offline" ? "App offline" : result === "declined" ? "Declined" : "Failed";
  setTimeout(() => {
    button.className = "btn primary";
    button.textContent = "Download";
  }, 2500);
}

async function loadMedia() {
  const [frames, network] = await Promise.all([
    scanPage(),
    tab ? send({ cmd: "media", tabId: tab.id }) : Promise.resolve(null),
  ]);
  const title = (tab && tab.title) || (frames && frames[0] ? frames[0].title : "");
  // Page elements first, then network finds newest first; first sighting wins.
  const unique = new Map();
  const found = [...(frames || []).flatMap((f) => f.items), ...[...(network || [])].reverse()];
  for (const m of found) if (!unique.has(m.url)) unique.set(m.url, m);
  const items = [...unique.values()];

  $("count").textContent = items.length ? String(items.length) : "";
  $("count").hidden = !items.length;
  const list = $("media");
  list.replaceChildren();
  for (const item of items) {
    const li = $("item").content.firstElementChild.cloneNode(true);
    li.querySelector(".badge").textContent = item.kind;
    li.querySelector(".badge").classList.add(item.kind);
    li.querySelector(".title").textContent = label(item.url, title);
    li.querySelector(".title").title = item.url;
    li.querySelector(".where").textContent = new URL(item.url).hostname;
    const button = li.querySelector("button");
    button.setAttribute("aria-label", `Download ${label(item.url, title)} with TDM`);
    button.addEventListener("click", () => download(button, item, title));
    list.append(li);
  }
  if (items.length) return;
  const streamed = (frames || []).reduce((n, f) => n + f.streamed, 0);
  if (!frames) showEmpty("TDM can't look inside this page.");
  else if (streamed) showEmpty("This page streams its video. With media detection on, start playback and reopen this popup.");
  else showEmpty("No video or audio found on this page.");
}

refresh();
setInterval(refresh, 2000);
loadMedia();
