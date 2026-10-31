"use strict";
//
// TDM media pill — a small "Download" button over the <video>/<audio> under
// the pointer (or keyboard focus). Registered by the background only while
// the optional media-detection permission is granted; media-core.js runs
// first and provides globalThis.TdmMedia.
//
// The pill lives in a closed Shadow DOM on a custom element so page CSS cannot
// reach it, and it is built with DOM calls rather than innerHTML so pages that
// enforce Trusted Types do not break it.
//
(() => {
  if (globalThis.__tdmMediaPill) return; // one instance per frame
  globalThis.__tdmMediaPill = true;

  const api = globalThis.browser ?? globalThis.chrome;
  const { isStreamed, pickStream, sourcesOf } = globalThis.TdmMedia;

  const MIN_W = 120; // smaller elements are icons/previews, not something to save
  const MIN_H = 60;
  const MISS_RETRY_MS = 2000;
  const LABEL = "⬇ TDM";

  const CSS = `
    :host { all: initial; }
    .pill {
      display: inline-flex; align-items: center; gap: 2px; padding: 3px;
      border-radius: 999px; font: 600 12px/1 system-ui, -apple-system, "Segoe UI", sans-serif;
      background: rgba(28, 26, 25, .9); color: #f5efe9;
      box-shadow: 0 6px 22px rgba(0,0,0,.35), inset 0 0 0 1px rgba(255,255,255,.08);
      backdrop-filter: blur(8px); -webkit-backdrop-filter: blur(8px);
      animation: in .18s cubic-bezier(.2,.8,.2,1);
    }
    button {
      all: unset; box-sizing: border-box; cursor: pointer; border-radius: 999px;
      padding: 7px 11px; white-space: nowrap; transition: filter .15s, background .15s, opacity .15s;
    }
    .go { background: linear-gradient(135deg, #FF7A1A, #FFB347); color: #1c1a19; }
    .go:hover { filter: brightness(1.08); }
    .go[aria-busy="true"] { opacity: .75; cursor: progress; }
    .go.ok { background: #2fbf71; color: #fff; }
    .go.err { background: #e5484d; color: #fff; }
    .x { padding: 7px 9px; opacity: .7; }
    .x:hover { opacity: 1; background: rgba(255,255,255,.08); }
    button:focus-visible { outline: 2px solid #FFB347; outline-offset: 2px; }
    @media (prefers-color-scheme: light) {
      .pill { background: rgba(255,255,255,.94); color: #1c1a19;
              box-shadow: 0 6px 22px rgba(0,0,0,.18), inset 0 0 0 1px rgba(0,0,0,.08); }
      .x:hover { background: rgba(0,0,0,.06); }
    }
    @keyframes in { from { opacity: 0; transform: translateY(-4px) scale(.96); } }
    @media (prefers-reduced-motion: reduce) { .pill { animation: none; } button { transition: none; } }
  `;

  // ------------------------------------------------------------------ build

  const host = document.createElement("tdm-media-pill");
  const pin = (k, v) => host.style.setProperty(k, v, "important");
  pin("all", "initial");
  pin("position", "fixed");
  pin("z-index", "2147483647");
  pin("display", "none");
  const root = host.attachShadow({ mode: "closed" });
  try {
    // Constructable sheets are exempt from the page's style-src CSP.
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(CSS);
    root.adoptedStyleSheets = [sheet];
  } catch (_) {
    const style = document.createElement("style");
    style.textContent = CSS;
    root.append(style);
  }
  const pill = document.createElement("div");
  pill.className = "pill";
  pill.setAttribute("role", "group");
  pill.setAttribute("aria-label", "Turbo Download Manager");
  const go = document.createElement("button");
  go.type = "button";
  go.className = "go";
  go.textContent = LABEL;
  go.setAttribute("aria-live", "polite");
  const close = document.createElement("button");
  close.type = "button";
  close.className = "x";
  close.textContent = "×";
  close.title = "Hide the TDM button for this media";
  close.setAttribute("aria-label", close.title);
  pill.append(go, close);
  root.append(pill);

  // ------------------------------------------------------------------ state

  const dismissed = new WeakSet();
  const tabbedPast = new WeakSet();
  let current = null; // element the pill belongs to
  let currentUrl = "";
  let miss = { el: null, at: 0 }; // last element with no usable URL, to avoid asking every frame
  let pointer = { x: -1, y: -1 };
  let raf = 0;
  let resetTimer = 0;
  let viaKeyboard = false;

  const visible = () => host.isConnected && host.style.getPropertyValue("display") !== "none";

  function place() {
    if (!current || !current.isConnected) return hide();
    const r = current.getBoundingClientRect();
    const right = Math.min(r.right, innerWidth) - 10;
    const top = Math.max(r.top, 0) + 10;
    pin("left", `${right}px`);
    pin("top", `${top}px`);
    pin("transform", "translateX(-100%)");
  }

  function show(el, url) {
    current = el;
    currentUrl = url;
    if (!host.isConnected) document.documentElement.append(host);
    place();
    pin("display", "block");
  }

  function hide() {
    pin("display", "none");
    current = null;
    viaKeyboard = false;
  }

  // An element's own http(s) URL, else (for blob:/data: players) the stream
  // the background saw on the network for this tab, else nothing.
  async function urlFor(el) {
    const direct = sourcesOf(el)[0];
    if (direct) return direct;
    if (!isStreamed(el)) return "";
    try {
      const best = pickStream(await api.runtime.sendMessage({ cmd: "mediaForTab" }));
      return best ? best.url : "";
    } catch (_) {
      return ""; // extension reloaded underneath us
    }
  }

  async function attach(el) {
    if (el === current) return place();
    if (dismissed.has(el) || document.fullscreenElement) return;
    if (miss.el === el && Date.now() - miss.at < MISS_RETRY_MS) return;
    const url = await urlFor(el);
    if (!url) {
      miss = { el, at: Date.now() };
      return;
    }
    show(el, url);
  }

  function mediaAt(x, y) {
    for (const el of document.querySelectorAll("video, audio")) {
      const r = el.getBoundingClientRect();
      if (r.width < MIN_W || r.height < MIN_H) continue;
      if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) return el;
    }
    return null;
  }

  // Pointer tracking by hit-testing rectangles rather than event targets:
  // players cover their <video> with control overlays, so the target is
  // almost never the media element itself.
  function check() {
    raf = 0;
    const el = mediaAt(pointer.x, pointer.y);
    if (el) return void attach(el);
    if (current && !viaKeyboard && !host.matches(":hover")) hide();
  }
  const schedule = () => {
    if (!raf) raf = requestAnimationFrame(check);
  };

  addEventListener(
    "pointermove",
    (e) => {
      pointer = { x: e.clientX, y: e.clientY };
      schedule();
    },
    { passive: true, capture: true },
  );
  addEventListener("scroll", () => current && schedule(), { passive: true, capture: true });
  addEventListener("resize", () => current && place(), { passive: true });
  document.addEventListener("fullscreenchange", () => document.fullscreenElement && hide());

  // Keyboard: focusing a player shows the pill, Tab from the player moves
  // into it, Escape (anywhere in the pill) puts focus back.
  document.addEventListener("focusin", (e) => {
    const el = e.target && e.target.closest ? e.target.closest("video, audio") : null;
    if (!el) return;
    void attach(el).then(() => {
      if (current === el) viaKeyboard = true;
    });
  });
  document.addEventListener(
    "keydown",
    (e) => {
      if (
        e.key === "Tab" &&
        !e.shiftKey &&
        visible() &&
        viaKeyboard &&
        document.activeElement === current &&
        !tabbedPast.has(current)
      ) {
        e.preventDefault();
        go.focus();
      }
    },
    true,
  );
  root.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" && !(e.key === "Tab" && (e.shiftKey ? e.target === go : e.target === close))) return;
    e.preventDefault();
    const back = current;
    // Leaving with Tab means "move on": the next Tab from the player must not
    // pull focus back into the pill.
    if (back && e.key === "Tab") tabbedPast.add(back);
    hide();
    if (back && back.focus) back.focus();
  });

  // ---------------------------------------------------------------- actions

  function feedback(cls, text) {
    clearTimeout(resetTimer);
    go.className = `go ${cls}`.trim();
    go.textContent = text;
    go.removeAttribute("aria-busy");
    resetTimer = setTimeout(() => {
      go.className = "go";
      go.textContent = LABEL;
    }, 2500);
  }

  go.addEventListener("click", async () => {
    if (!currentUrl || go.getAttribute("aria-busy")) return;
    go.setAttribute("aria-busy", "true");
    go.textContent = "Sending…";
    let reply = null;
    try {
      reply = await api.runtime.sendMessage({ cmd: "downloadMedia", url: currentUrl });
    } catch (_) {
      /* extension reloaded; reported as a failure below */
    }
    const result = reply && reply.result;
    if (result === "accepted") feedback("ok", "✓ Sent to TDM");
    else if (result === "offline") feedback("err", "TDM isn't running");
    else feedback("err", "Couldn't send");
  });

  close.addEventListener("click", () => {
    const back = current;
    const keyboard = viaKeyboard;
    if (back) dismissed.add(back);
    hide();
    if (keyboard && back && back.focus) back.focus();
  });
})();
