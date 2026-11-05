"use strict";
//
// Turbo Download Manager — MV3 capture background (a service worker in
// Chromium, an event page in Firefox; see build.mjs).
//
// Protocol: see src/native_host.rs. Standard native messaging framing
// (4-byte LE length + UTF-8 JSON); the JSON is a plain object with a camelCase
// `type` discriminator. The browser does the framing for us, so this file only
// ever deals in objects.
//
// The background can be killed between any two events, so nothing that must
// survive lives only in a variable: configuration, connection state, the
// backoff clock and detected media all go through storage. Callbacks (the held
// `suggest`) cannot be persisted, so every path that holds one also guarantees
// it is answered — on error, on timeout and on host disconnect.
//

import {
  basename,
  belowMinSize,
  checkIncognitoAccess,
  httpUrl,
  isTorrentLink,
  normalizeExtension,
  normalizeHost,
  pickApi,
  shouldCapture,
} from "./filter.js";
import "./media-core.js"; // classic script shared with the content script; defines globalThis.TdmMedia

const { filenameFromTitle, mediaKind, pickStream } = globalThis.TdmMedia;
const api = pickApi(globalThis);

const HOST_NAME = "com.tdm.host";
const EXT_VERSION = "1.0"; // protocol version sent in `hello`, not the manifest version
// Longer than the host's own worst case for a download while the app is
// closed (2 s connect + 10 s LAUNCH_WAIT + a retry, src/ipc.rs): giving up
// sooner resumes the browser's copy while the app still receives it.
const VERDICT_TIMEOUT_MS = 20000;
const BACKOFF_MIN_MS = 2000;
const BACKOFF_MAX_MS = 5 * 60 * 1000;
const DECIDED_KEEP = 256;
const MEDIA_KEEP = 40; // per tab
const MAX_SELECTED_LINKS = 200;
const MENU_LINK = "tdm-link";
const MENU_MEDIA = "tdm-media";
const MENU_SELECTION = "tdm-selection";
const MEDIA_SCRIPT_ID = "tdm-media";
// Media detection is on exactly when both are granted. Asking for webRequest
// too keeps the cookie grant (which also needs <all_urls>) from silently
// turning on an overlay the user never asked for.
const MEDIA_ACCESS = { permissions: ["webRequest"], origins: ["<all_urls>"] };

// Chromium lets us hold a download before it starts writing; Firefox does not
// have this event, so there we intercept in onCreated instead.
const HAS_DETERMINING_FILENAME = !!api.downloads.onDeterminingFilename;

const TIMED_OUT = Symbol("timeout");
const quiet = () => {};

let port = null; // live native port, or null when not connected
let handshake = null; // in-flight ensureHost() promise, so we connect once
const waiting = []; // FIFO of reply resolvers — the host answers in request order
const inFlight = new Set(); // download ids being decided right now

// ---------------------------------------------------------------- connection

function dropPort(why) {
  const dead = port;
  port = null;
  if (dead) {
    try {
      dead.disconnect();
    } catch (_) {
      /* already gone */
    }
  }
  // Unblock every held download rather than leaving it paused forever.
  while (waiting.length) waiting.shift()(null);
  void recordFailure(why);
}

async function recordFailure(message) {
  const { backoffMs = BACKOFF_MIN_MS } = await api.storage.session.get("backoffMs");
  await api.storage.session.set({
    connected: false,
    lastError: message,
    nextAttemptAt: Date.now() + backoffMs,
    backoffMs: Math.min(backoffMs * 2, BACKOFF_MAX_MS),
  });
}

function request(message) {
  return new Promise((resolve) => {
    if (!port) return resolve(null);
    waiting.push(resolve);
    try {
      port.postMessage(message);
    } catch (e) {
      waiting.pop(); // nothing async ran since the push, so this is ours
      dropPort(String((e && e.message) || e));
      resolve(null);
    }
  });
}

// A reply that never arrives would desynchronise the FIFO for every later
// request, so a timeout is fatal to the connection, not just to this request.
async function requestWithTimeout(message, ms) {
  let timer;
  const reply = await Promise.race([
    request(message),
    new Promise((r) => {
      timer = setTimeout(() => r(TIMED_OUT), ms);
    }),
  ]);
  clearTimeout(timer);
  if (reply === TIMED_OUT) {
    dropPort(`host did not reply within ${ms} ms`);
    return null;
  }
  return reply;
}

async function cachedLists() {
  const s = await api.storage.session.get(["captureExtensions", "exceptionHosts", "captureMinSize"]);
  return {
    captureExtensions: s.captureExtensions || [],
    exceptionHosts: s.exceptionHosts || [],
    captureMinSize: s.captureMinSize || 0,
  };
}

// Resolves to the capture lists from the handshake, or null if the host is not
// reachable. `force` ignores the backoff clock (used for explicit user actions).
function ensureHost(force = false) {
  // `port` is set before `ready` arrives, so check the handshake first.
  if (handshake) return handshake;
  if (port) return cachedLists();
  const p = (async () => {
    const { nextAttemptAt = 0 } = await api.storage.session.get("nextAttemptAt");
    if (!force && Date.now() < nextAttemptAt) return null;
    return openAndHandshake();
  })();
  handshake = p;
  p.catch(() => null).then(() => {
    if (handshake === p) handshake = null;
  });
  return p;
}

async function openAndHandshake() {
  let opened;
  try {
    opened = api.runtime.connectNative(HOST_NAME);
  } catch (e) {
    await recordFailure(String((e && e.message) || e));
    return null;
  }
  if (api.runtime.lastError) {
    await recordFailure(api.runtime.lastError.message);
    return null;
  }
  opened.onMessage.addListener((msg) => {
    const resolve = waiting.shift();
    if (resolve) resolve(msg); // unsolicited pushes are ignored; we have none yet
  });
  opened.onDisconnect.addListener((p) => {
    // Chromium reports the reason through runtime.lastError, Firefox through port.error.
    const err = (p && p.error) || api.runtime.lastError;
    const why = (err && err.message) || "host disconnected";
    if (port === opened) dropPort(why);
  });
  port = opened;

  const ready = await requestWithTimeout({ type: "hello", version: EXT_VERSION }, VERDICT_TIMEOUT_MS);
  if (!ready || ready.type !== "ready") {
    const why = ready ? `handshake answered with "${ready.type}"` : "host closed during handshake";
    if (port === opened) dropPort(why);
    return null;
  }
  // Capture switched off in the app: capture nothing (an empty list), but stay connected.
  const lists = {
    captureExtensions: ready.enabled === false ? [] : (ready.captureExtensions || []).map(normalizeExtension),
    exceptionHosts: (ready.exceptionHosts || []).map(normalizeHost),
    // Bytes, from the app's "skip downloads smaller than" setting; 0 is off.
    captureMinSize: ready.captureMinSize > 0 ? ready.captureMinSize : 0,
  };
  await api.storage.session.set({
    connected: true,
    hostVersion: String(ready.version || ""),
    lastError: "",
    backoffMs: BACKOFF_MIN_MS,
    nextAttemptAt: 0,
    ...lists,
  });
  return lists;
}

// ------------------------------------------------------------------ decision
// (the rule itself is in filter.js, checked by filter.test.mjs)

// The same download id can be delivered more than once; decide it once.
async function claim(id) {
  if (inFlight.has(id)) return false;
  const { decided = [] } = await api.storage.session.get("decided");
  if (decided.includes(id)) return false;
  await api.storage.session.set({ decided: [...decided, id].slice(-DECIDED_KEEP) });
  inFlight.add(id);
  return true;
}

async function cookieHeader(url) {
  if (!api.cookies) return undefined; // optional "cookies" permission not granted
  try {
    const all = await api.cookies.getAll({ url });
    return all.length ? all.map((c) => `${c.name}=${c.value}`).join("; ") : undefined;
  } catch (_) {
    return undefined; // no host permission for this origin
  }
}

// JSON.stringify drops undefined, which is exactly what the Rust side's
// Option<_> fields expect.
async function downloadMessage(url, extra) {
  return {
    type: "download",
    url,
    cookie: await cookieHeader(url),
    userAgent: navigator.userAgent,
    ...extra,
  };
}

const setLast = (text) => api.storage.session.set({ lastAction: text });

// Magnet and .torrent links are handed to the app only when the user turned
// torrents on in the extension options. Read on every use, so a change applies
// at once. Missing or non-true means off.
async function torrentsEnabled() {
  const { torrentsEnabled: on = false } = await api.storage.local.get("torrentsEnabled");
  return on === true;
}

// Chromium only delivers private-window events to an extension the user allowed
// in Incognito; this re-reads that grant (it can change at any time) and keeps
// the popup's copy in storage.session.
async function refreshIncognito() {
  const allowed = await checkIncognitoAccess(api.extension);
  await api.storage.session.set({ incognitoAllowed: allowed });
  return allowed;
}

async function decide(item) {
  const raw = item.finalUrl || item.url;
  const url = httpUrl(raw);
  if (!url) return false;
  const { enabled = true } = await api.storage.local.get("enabled");
  if (!enabled) return false;
  if (isTorrentLink(raw) && !(await torrentsEnabled())) {
    console.info("torrent download ignored: torrents are off in the extension options");
    return false;
  }
  // A private download is ours only if the user allowed TDM in Incognito; else the browser keeps it.
  if (item.incognito === true && !(await refreshIncognito())) return false;
  if (!(await claim(item.id))) return false;
  try {
    const filename = basename(item.filename) || undefined;
    // Pre-filter on the last known lists so a page full of uninteresting
    // downloads never tries to wake a host that is not installed.
    const cached = await cachedLists();
    if (cached.captureExtensions.length && !shouldCapture(url, filename, cached)) return false;

    const lists = await ensureHost();
    if (!shouldCapture(url, filename, lists)) return false;
    // The options page may override the app's floor; a number there wins, KiB.
    const { captureMinKib } = await api.storage.local.get("captureMinKib");
    const minBytes = typeof captureMinKib === "number" ? captureMinKib * 1024 : lists.captureMinSize;
    if (belowMinSize(item.totalBytes, minBytes)) return false;

    const reply = await requestWithTimeout(
      await downloadMessage(raw, {
        referer: item.referrer || undefined,
        filename,
        size: item.totalBytes > 0 ? item.totalBytes : undefined,
      }),
      VERDICT_TIMEOUT_MS,
    );
    await setLast(`${reply ? reply.type : "no reply"}: ${raw}`);
    return !!reply && reply.type === "accepted";
  } finally {
    inFlight.delete(item.id);
  }
}

// An explicit user action (menu, popup, media pill): bypasses the capture
// lists, the on/off toggle and the backoff clock. Resolves to the reply type,
// "offline" or "unsupported" so the caller can show feedback.
async function handOver(raw, extra = {}) {
  const torrent = isTorrentLink(raw);
  if (torrent && !(await torrentsEnabled())) {
    // The link itself is left out of the log on purpose.
    console.info("torrent link ignored: torrents are off in the extension options");
    await setLast("torrent link ignored: torrents are off in the extension options");
    return "disabled";
  }
  if (!torrent && !httpUrl(raw)) {
    await setLast(`unsupported URL: ${raw}`);
    return "unsupported";
  }
  if (!(await ensureHost(true))) return "offline";
  const reply = await requestWithTimeout(await downloadMessage(raw, extra), VERDICT_TIMEOUT_MS);
  await setLast(`${reply ? reply.type : "no reply"}: ${raw}`);
  return reply ? reply.type : "no reply";
}

// ------------------------------------------------------------------- capture

const pause = (id) => api.downloads.pause(id).catch(quiet);
const resume = (id) => api.downloads.resume(id).catch(quiet);
// Our application is fetching it, so the browser must not also fetch it.
const takeOver = (id) =>
  api.downloads
    .cancel(id)
    .catch(quiet)
    .then(() => api.downloads.erase({ id }))
    .catch(quiet);

api.downloads.onCreated.addListener((item) => {
  if (!httpUrl(item.finalUrl || item.url)) return;
  // Pause first and ask later. In Chromium onDeterminingFilename always
  // follows and always resumes or cancels, so nothing stays paused on our
  // account; in Firefox we settle it here.
  void pause(item.id);
  if (HAS_DETERMINING_FILENAME) return;
  decide(item).then(
    (take) => (take ? takeOver(item.id) : resume(item.id)),
    () => resume(item.id),
  );
});

if (HAS_DETERMINING_FILENAME) {
  api.downloads.onDeterminingFilename.addListener((item, suggest) => {
    const letBrowserHaveIt = () => {
      void resume(item.id);
      suggest();
    };
    decide(item).then(
      (take) => {
        if (!take) return letBrowserHaveIt();
        void takeOver(item.id);
        suggest();
      },
      () => letBrowserHaveIt(),
    );
    return true; // we answer `suggest` asynchronously
  });
}

// ------------------------------------------------------------- media (tabs)
// Media URLs seen on the network, per tab, newest last. Kept in
// storage.session because the background may be torn down between the
// request and the user opening the popup.

const mediaKey = (tabId) => `media:${tabId}`;
let mediaChain = Promise.resolve(); // serialises read-modify-write so concurrent responses don't drop entries

async function tabMedia(tabId) {
  const key = mediaKey(tabId);
  const s = await api.storage.session.get(key);
  return s[key] || [];
}

// `change` returns the new list, or null for "unchanged".
function updateTabMedia(tabId, change) {
  const key = mediaKey(tabId);
  mediaChain = mediaChain
    .then(async () => {
      const next = change(await tabMedia(tabId));
      if (!next) return;
      if (next.length) await api.storage.session.set({ [key]: next });
      else await api.storage.session.remove(key);
    })
    .catch(quiet);
  return mediaChain;
}

// Origins the user turned media detection on for (options page). Empty by
// default, so nothing is kept until a site is listed. Cached; dropped whenever
// the list changes in storage.
let mediaSites = null;
async function mediaSiteEnabled(origin) {
  if (!origin) return false;
  if (!mediaSites) {
    const { mediaSites: list = [] } = await api.storage.local.get("mediaSites");
    mediaSites = new Set(list);
  }
  return mediaSites.has(origin);
}
api.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.mediaSites) mediaSites = null;
});

// The page a request was made from: Chromium's `initiator`, Firefox's `originUrl`.
function pageOrigin(d) {
  try {
    return new URL(d.initiator || d.originUrl || d.url).origin;
  } catch (_) {
    return "";
  }
}

function onMediaResponse(d) {
  if (d.tabId < 0) return; // service workers, prefetch, the extension itself
  if (d.type === "main_frame") return void updateTabMedia(d.tabId, (list) => (list.length ? [] : null));
  const header = (d.responseHeaders || []).find((h) => h.name.toLowerCase() === "content-type");
  const kind = mediaKind(d.url, header && header.value);
  if (!kind) return;
  // Only pages on sites the user enabled; nothing is recorded for any other site.
  void mediaSiteEnabled(pageOrigin(d)).then((on) => {
    if (!on) return;
    updateTabMedia(d.tabId, (list) =>
      list.some((m) => m.url === d.url) ? null : [...list, { url: d.url, kind }].slice(-MEDIA_KEEP),
    );
  }, quiet);
}

// `api.webRequest` only exists once the optional permission is granted. Called
// at top level (so a restarted background is woken by events) and on grant.
function watchMedia() {
  const wr = api.webRequest;
  if (!wr || wr.onResponseStarted.hasListener(onMediaResponse)) return;
  try {
    wr.onResponseStarted.addListener(
      onMediaResponse,
      { urls: ["<all_urls>"], types: ["main_frame", "media", "xmlhttprequest", "other"] },
      ["responseHeaders"],
    );
  } catch (e) {
    console.warn("media observer:", e);
  }
}
watchMedia();

async function hasMediaAccess() {
  try {
    return await api.permissions.contains(MEDIA_ACCESS);
  } catch (_) {
    return false;
  }
}

// The hover pill is a dynamically registered content script, so it exists
// only while the optional host permission does — nothing is injected by
// default. Idempotent; re-run on startup because not every browser persists
// registrations across sessions.
async function syncMedia() {
  const on = await hasMediaAccess();
  if (on) watchMedia();
  if (!api.scripting || !api.scripting.registerContentScripts) return;
  try {
    const have = await api.scripting.getRegisteredContentScripts({ ids: [MEDIA_SCRIPT_ID] });
    if (on && !have.length) {
      await api.scripting.registerContentScripts([
        {
          id: MEDIA_SCRIPT_ID,
          matches: ["<all_urls>"],
          js: ["media-core.js", "media.js"],
          allFrames: true, // players are usually embedded in iframes
          runAt: "document_idle",
        },
      ]);
    } else if (!on && have.length) {
      await api.scripting.unregisterContentScripts({ ids: [MEDIA_SCRIPT_ID] });
    }
  } catch (e) {
    console.warn("media content script:", e);
  }
}

api.permissions.onAdded.addListener(() => void syncMedia());
api.permissions.onRemoved.addListener(() => void syncMedia());
api.tabs.onRemoved.addListener((tabId) => void updateTabMedia(tabId, () => []));

// -------------------------------------------------------------- context menu

function createMenus() {
  // removeAll first: drops ids from older versions and avoids duplicate-id errors.
  api.contextMenus.removeAll(() => {
    void api.runtime.lastError;
    const add = (props) => api.contextMenus.create(props, () => void api.runtime.lastError);
    add({ id: MENU_LINK, title: "Download with TDM", contexts: ["link"] });
    add({ id: MENU_MEDIA, title: "Download media with TDM", contexts: ["image", "video", "audio"] });
    add({ id: MENU_SELECTION, title: "Download selected links with TDM", contexts: ["selection"] });
  });
}

function onStart() {
  createMenus();
  void syncMedia();
  void refreshIncognito();
}
api.runtime.onInstalled.addListener(onStart);
api.runtime.onStartup.addListener(onStart);

// Reads the links inside the current selection. The menu click grants
// activeTab, which is all this needs.
async function selectedLinks(tab, frameId) {
  if (!tab || !api.scripting) return [];
  try {
    const [res] = await api.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [frameId || 0] },
      func: () => {
        const sel = getSelection();
        if (!sel || sel.isCollapsed) return [];
        return [...document.querySelectorAll("a[href]")].filter((a) => sel.containsNode(a, true)).map((a) => a.href);
      },
    });
    return (res && res.result) || [];
  } catch (e) {
    await setLast(`could not read the selection: ${(e && e.message) || e}`);
    return [];
  }
}

api.contextMenus.onClicked.addListener((info, tab) => {
  const referer = info.pageUrl || (tab && tab.url) || undefined;
  void (async () => {
    if (info.menuItemId === MENU_SELECTION) {
      const links = [...new Set(await selectedLinks(tab, info.frameId))].filter((u) => httpUrl(u));
      // Sequential: the host answers in order and one slow reply must not time out the rest.
      for (const link of links.slice(0, MAX_SELECTED_LINKS)) await handOver(link, { referer });
      if (!links.length) await setLast("no links in the selection");
      return;
    }
    if (info.menuItemId === MENU_LINK) return void (await handOver(info.linkUrl, { referer }));
    if (info.menuItemId !== MENU_MEDIA) return;
    let raw = info.srcUrl;
    // A streamed <video> has a blob: src; the stream seen on the network is what the user means.
    if (!httpUrl(raw) && info.mediaType !== "image" && tab) {
      const best = pickStream(await tabMedia(tab.id));
      if (best) raw = best.url;
    }
    const filename = info.mediaType === "image" ? undefined : filenameFromTitle(tab && tab.title, raw);
    await handOver(raw, { referer: info.frameUrl || referer, filename });
  })();
});

// ------------------------------------------------------ popup, options, pill

async function status() {
  const s = await api.storage.session.get([
    "connected",
    "hostVersion",
    "captureExtensions",
    "exceptionHosts",
    "lastError",
    "lastAction",
    "nextAttemptAt",
  ]);
  const { enabled = true } = await api.storage.local.get("enabled");
  return {
    hostName: HOST_NAME,
    extensionId: api.runtime.id,
    enabled,
    connected: !!port && !!s.connected,
    hostVersion: s.hostVersion || "",
    captureExtensions: s.captureExtensions || [],
    exceptionHosts: s.exceptionHosts || [],
    lastError: s.lastError || "",
    lastAction: s.lastAction || "",
    retryInMs: Math.max(0, (s.nextAttemptAt || 0) - Date.now()),
    hasCookieAccess: !!api.cookies,
    hasMediaAccess: await hasMediaAccess(),
    incognitoAllowed: await refreshIncognito(),
  };
}

// Our own pages (popup, options) may change settings; content scripts run in
// arbitrary pages and may only ask for media and hand over a media URL.
function fromExtensionPage(sender) {
  return sender.id === api.runtime.id && typeof sender.url === "string" && sender.url.startsWith(api.runtime.getURL(""));
}

// A content script may start a download only for a URL on the origin of the
// tab it runs in. Anything else is refused by the caller.
function sameOriginAsTab(raw, sender) {
  const target = httpUrl(raw);
  const tabUrl = sender.tab && sender.tab.url;
  if (!target || !tabUrl) return false;
  try {
    return new URL(tabUrl).origin === target.origin;
  } catch (_) {
    return false;
  }
}

api.runtime.onMessage.addListener((msg, sender, reply) => {
  if (!msg || typeof msg.cmd !== "string") return;
  const page = fromExtensionPage(sender);
  const answer = (p) => {
    p.then(reply, (e) => reply({ error: String((e && e.message) || e) }));
    return true; // reply asynchronously
  };

  if (msg.cmd === "downloadMedia" && typeof msg.url === "string") {
    if (!page && !sameOriginAsTab(msg.url, sender)) {
      // The reason only; the URL is not logged.
      console.warn("downloadMedia refused: URL is not on the sender's tab origin");
      return answer(Promise.resolve({ result: "rejected" }));
    }
    // From the popup the page details come in the message; from the content
    // script they come from the browser, which the page cannot forge.
    const referer = page ? msg.pageUrl : sender.url;
    const title = page ? msg.title : sender.tab && sender.tab.title;
    return answer(
      handOver(msg.url, { referer: referer || undefined, filename: filenameFromTitle(title, msg.url) }).then(
        (result) => ({ result }),
      ),
    );
  }
  if (msg.cmd === "mediaForTab" && !page && sender.tab) return answer(tabMedia(sender.tab.id));
  if (!page) return;

  if (msg.cmd === "status") return answer(status());
  if (msg.cmd === "media" && Number.isInteger(msg.tabId)) return answer(tabMedia(msg.tabId));
  if (msg.cmd === "setEnabled") return answer(api.storage.local.set({ enabled: !!msg.value }).then(status));
  if (msg.cmd === "reconnect") return answer(ensureHost(true).then(status));
});
