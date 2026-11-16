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
  const { buttonLabel, describeChoice, isDecorative, isStreamed, mediaKind, pickStream, sourcesOf } = globalThis.TdmMedia;

  const MIN_W = 120; // smaller elements are icons/previews, not something to save
  const MIN_H = 60;
  const MISS_RETRY_MS = 2000;
  const LABEL = "Download media with TDM";
  const DECORATIVE_HOVER_MS = 600; // muted looping background clips need a deliberate hover
  // Images are offered only when large enough to be a picture worth saving.
  const IMG_MIN_W = 160;
  const IMG_MIN_H = 120;
  // The TDM logo, shown on the button. Inlined as a data URI: an extension
  // resource would need web_accessible_resources, which lets every page detect
  // the extension.
  const ICON =
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAHsklEQVR4nM1XeXCVVxU/595veUvykpCwlkK1WJwyFKQWhrIlrqQwlDpsZYCGWpZ0htahQRBmjHEUik5UqJaq1dZAQZOZ0lZaBIEmQJhKpVKwiCxlpCyJeWR52/e+77v3Hue+LBMdkqDU0fPX9949557fPed3zrkX4H8s2Nsi9bF+C5sT/D8KdTsY9qZ4ctHAMPG0KW3WfpKWvje/4jI1zvbNa8IP7d3tXKsAUJ1r5QCs228GAMq4KcJyYFiB6u4hrbsCATFRuiCRI/c5gJ8gQG16MzsFYmQYQwow5ntmaQXAlfa9QL09DYyiOhBa78WJoc9cdNiVTScT0ZsC6BSTiwLDVAWGBAADweQAkhMAx0xykSgTQn0kRQB2CCHhwNkUw7lVf2QN1RPtz9Wcces6QIiaSeYoUvwRBbRv48lEdKPettd4EkgQQEKCBEDuSwKlOtiVSWTmQzIEHrQY3kjy7Yf+lvNkOhYbNG2IdzElcM3MGjikLfZMtjZIwAlg0NMPH0lfmg+A2rqHYHaJzHhBQIaEDLURakOdBR0GGTbRAMbd6ynjyYIqb8m8uqZEfha9JAhfmHnU/dkbkwKTa4usUxzx/oePurOaJHOrHwzMz0AvB9ZrBAgwu4O0HeWkXVNnbcmwDUbSY2euuubSkbuc43umWOtcgceb/PCCE1GI1xaqbSFDrmz2YFPxEW/97gfNsdmmrEsI+KbeoLYWWC8RIPDIrALGEZFQUTsISaC/VNhG3uIau+qv5o/Xzg8VWs+HOG1oQXa2vxWfNn9o7HLYgJVNLs6Zfthfv318v4jJcI2vaNvsem9LeTkwTcoey7CTva1rg6tyAv5WpZR0owiQImZbiFGXrxtY5W3WGw8Mxn89xIbiCyl8McBo8IgwzbjiYNM13ypaWP/pc7WFp15NCLb/um9UL/tDsrG7H9YTgFoAduFrkc/mbnaeiybMUjQYtw3iyJhocNgi7fzsIvsT2UbsnjyDihvSys9m6vFBFs34yGH734ubYywUg+uL3r/MEIuEov3a+U/vB7NPAESAhRUgB9jutraywOr+lc4LsSRf5TN2tTHFi+/Y4b/y90VWVT5Xy043Go2eBMkBeIARNPnsvRuCHRyb5b/yyYD8vQAMfOiZE2Yf8/66fYJdmDAjmlcdhIIeIvCtDNvJROFG8v3KG2tCT+R+z/3x+83BMXf9xj0Yfcx6vn8/uZghtRkGcUDgRBncjAONG2KJzRGTiuISGy779qSS+uQHr0+xK4MG3L36nVhL9Vzg7eSGnlOQqQFgAlxF/ULez5vLgqsmbE/ciJbYO/NtWQoOKVKMGQSoCGTEQp5ScMkjOEAEwlcITS5buPho4vyhaWYdBzVsTr37CygHnFej+0ofHIDOHkcAypcqy/a2ti7nJ/MN/1HHU25Hf2BSMT/HQp6Q7NSfGnPGSIK/ZJtgtAk6lwRMHysy/gwAd73L/Ec186Hinyck6xlAZjmscSgFiiNBTkCNSbqkJAEDRmgiBe2gDEc99tvCWm/s6AHxFRGDSts8EkiQNcwUewHxnkuOXVxRB2LUmfbu96/H7BLSubkX8Hwz8E/1A9mSCi7Py/J/Ij0l9AQQzUgiQWCZwCSg0yrM6aev55y45sWzh6L/3J0BmNPitc8HTwGEDYRLDntCh777MOouCH1IS1lgdW62qFS+Em4UGUsTKsZEU5o/NHyne0DrlA7LyZs60JkkgcVyLMyXXLXaQJuA6HrxEf+Rnpxr6WrFWmn0uMBCMygNJTIM5AJ4Om9z+getzwTsnDyx0TBICmQQ9fisMn/W2zdK3ljhCkWAKWdwFqavOdB2x8vO7jenWsWKKLfZzf4SlTfrvHeR7qYASIe+BmTrA1AQyaFKcPTwQwBTQWxdYHzk2fRT8fV2xDTU15uSOHt4tfe7lpLXjuUG5UTwATJFJRgIn019bbJ1LyPY7SNNWXy8OWYPBz6v26UEemu9AACxtfYP6duM/HXoiPXo0Xc4tZUFv6vXzpeExumstS4xDtAKRonHMJ0qQcf/KqePFlpf0Tr7ppgNeybbyzuj2pdf7AKgv6uBzauZCy+PeP1AKCgKRYqEHlcswIy2ON+Q+31/Y/MSvjcvKKen0uBrm3AAjetx/uyQnf439k81jwqicw8dEY/rlrviRCY+twagKwoVQKefCg8YGXHftZi6U/gkWaboGCab8HREyNFJ0X4JCttoRFP8rf47/BlvTbG3INKo4sPeFzpIp/Pe562Ydf+hpx/MBXbf1mRjsxOc6xPzGEdQmhKgIGKr0UmvPZ8hE4yYxy4WbPdmvjnZXApII4sPe1/UzabwFp1nfN7sTyoHQ9/hos/YS/NzxC+FowQy5H4zkEwAmAaBAJaOQ9bod857/RnQymyeXtbh+N96D7CboqoAoUEUVLovxZPmj4wQajJJPaEYI2WajMWIL9hRL2MS6POXsoau7Kzzj+0xQvryVa0LjDCx1jqgq8Ep5Q4t59SwJFCudX41MfTlLSPA7tT/T/ywnhYyJ/kAiAjhip+/wEuzC4FsCDQl2auDqtIVGx/Izv/QSh18+gK42vl/7RlG7bMbGssCk9pWWUf23TcwrAd/t+Pe1vsRbglE+3zRj6tMxG730XpbID5u5/8AtA61EomITJkAAAAASUVORK5CYII=";


  const CSS = `
    :host { all: initial; }
    .pill {
      display: inline-flex; align-items: center; gap: 2px; padding: 3px;
      border-radius: 999px; font: 600 12px/1 system-ui, -apple-system, "Segoe UI", sans-serif;
      background: rgba(0, 0, 0, .9); color: #f5f5f5;
      box-shadow: 0 6px 22px rgba(0,0,0,.35), inset 0 0 0 1px rgba(255,255,255,.08);
      backdrop-filter: blur(8px); -webkit-backdrop-filter: blur(8px);
      animation: in .18s cubic-bezier(.2,.8,.2,1);
    }
    button {
      all: unset; box-sizing: border-box; cursor: pointer; border-radius: 999px;
      padding: 7px 11px; white-space: nowrap; transition: filter .15s, background .15s, opacity .15s;
    }
    .go { background: #C2410C; color: #fff; }
    .go img { width: 14px; height: 14px; margin-right: 6px; vertical-align: -3px; border-radius: 3px; }
    .go:hover { filter: brightness(1.08); }
    .go[aria-busy="true"] { opacity: .75; cursor: progress; }
    .go.ok { background: #15803d; color: #fff; }
    .go.err { background: #b91c1c; color: #fff; }
    .more { padding: 7px 7px; opacity: .8; }
    .more:hover { opacity: 1; background: rgba(255,255,255,.08); }
    .menu { position: absolute; top: calc(100% + 6px); right: 0; min-width: 220px; max-width: 320px;
      padding: 4px; border-radius: 12px; background: rgba(0,0,0,.94); color: #f5f5f5;
      font: 500 12px/1.3 system-ui, -apple-system, "Segoe UI", sans-serif;
      box-shadow: 0 8px 26px rgba(0,0,0,.4), inset 0 0 0 1px rgba(255,255,255,.1); }
    .menu[hidden] { display: none; }
    .menu button { display: block; width: 100%; padding: 7px 9px; border-radius: 8px; white-space: normal; }
    .menu button:hover, .menu button:focus-visible { background: rgba(255,255,255,.12); }
    .menu small { display: block; opacity: .65; font-weight: 400; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .menu .best b::after { content: " · best"; color: #FF9A4D; font-weight: 500; }
    .x { padding: 7px 9px; opacity: .7; }
    .x:hover { opacity: 1; background: rgba(255,255,255,.08); }
    button:focus-visible { outline: 2px solid #FF7A1A; outline-offset: 2px; }
    @media (prefers-color-scheme: light) {
      .go { background: #C2410C; color: #fff; }
      button:focus-visible { outline-color: #F06A10; }
      .pill { background: rgba(255,255,255,.94); color: #111111;
              box-shadow: 0 6px 22px rgba(0,0,0,.18), inset 0 0 0 1px rgba(0,0,0,.08); }
      .x:hover, .more:hover { background: rgba(0,0,0,.06); }
      .menu { background: rgba(255,255,255,.97); color: #111; box-shadow: 0 8px 26px rgba(0,0,0,.2), inset 0 0 0 1px rgba(0,0,0,.1); }
      .menu button:hover, .menu button:focus-visible { background: rgba(0,0,0,.07); }
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
  go.setAttribute("aria-live", "polite");
  const close = document.createElement("button");
  close.type = "button";
  close.className = "x";
  close.textContent = "×";
  close.title = "Hide the TDM button for this media";
  close.setAttribute("aria-label", close.title);
  const more = document.createElement("button");
  more.type = "button";
  more.className = "more";
  more.textContent = "▾";
  more.hidden = true;
  more.title = "Choose which stream or file to send";
  more.setAttribute("aria-label", more.title);
  more.setAttribute("aria-haspopup", "menu");
  more.setAttribute("aria-expanded", "false");
  const menu = document.createElement("div");
  menu.className = "menu";
  menu.setAttribute("role", "menu");
  menu.hidden = true;
  pill.append(go, more, close);
  root.append(pill, menu);

  // The button text, with the TDM logo in front of it.
  function setGo(text) {
    const icon = document.createElement("img");
    icon.src = ICON;
    icon.alt = "";
    icon.width = 14;
    icon.height = 14;
    const label = document.createElement("span");
    label.textContent = text;
    go.replaceChildren(icon, label);
  }
  setGo(LABEL);

  // ------------------------------------------------------------------ state

  const dismissed = new WeakSet();
  const tabbedPast = new WeakSet();
  let current = null; // element the pill belongs to
  let currentUrl = "";
  let choices = []; // [{url, kind}] the background recorded for this tab, newest last
  let decoSince = { el: null, at: 0 };
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

  function closeMenu(refocus) {
    if (menu.hidden) return;
    menu.hidden = true;
    more.setAttribute("aria-expanded", "false");
    if (refocus) more.focus();
  }

  function labelFor(url) {
    return url && !/^https?:\/\/(www\.)?youtube\.com\//i.test(url) ? buttonLabel(url, mediaKind(url) || undefined) : LABEL;
  }

  function show(el, url, list) {
    current = el;
    currentUrl = url;
    choices = list;
    closeMenu(false);
    more.hidden = list.length < 2;
    setGo(labelFor(url));
    if (!host.isConnected) document.documentElement.append(host);
    place();
    pin("display", "block");
  }

  function hide() {
    closeMenu(false);
    pin("display", "none");
    current = null;
    viaKeyboard = false;
  }

  // An element's own http(s) URL, else (for blob:/data: players) the stream
  // the background saw on the network for this tab, else nothing.
  // A YouTube video page link as https://www.youtube.com/watch?v=ID (or a
  // Shorts link), or "" when `raw` is not one.
  function youtubeVideoLink(raw) {
    try {
      const u = new URL(raw, location.href);
      if (!/(^|\.)youtube\.com$/i.test(u.hostname)) return "";
      const id = u.pathname === "/watch" ? u.searchParams.get("v") : (u.pathname.match(/^\/shorts\/([^/]+)/) || [])[1];
      if (!id || !/^[A-Za-z0-9_-]{11}$/.test(id)) return "";
      return u.pathname === "/watch" ? `https://www.youtube.com/watch?v=${id}` : `https://www.youtube.com/shorts/${id}`;
    } catch (_) {
      return "";
    }
  }

  // On YouTube the media URLs (i.ytimg.com pictures, googlevideo.com chunks)
  // are not the video, so the button hands over the video's page link: the
  // open page for the main player, otherwise the link of the card the
  // thumbnail or hover preview belongs to.
  function youtubeLinkFor(el) {
    if (el.closest("ytd-watch-flexy #movie_player, ytd-shorts #shorts-player, #player #movie_player")) {
      const own = youtubeVideoLink(location.href);
      if (own) return own;
    }
    const sel = 'a[href*="/watch?v="], a[href*="/shorts/"]';
    let node = el;
    for (let depth = 0; node && depth < 10; depth++, node = node.parentElement) {
      const a = node.matches && node.matches(sel) ? node : node.querySelector && node.querySelector(sel);
      const link = a && youtubeVideoLink(a.getAttribute("href"));
      if (link) return link;
    }
    return youtubeVideoLink(location.href);
  }

  async function urlFor(el) {
    if (/(^|\.)youtube\.com$/i.test(location.hostname)) return { url: youtubeLinkFor(el), list: [] };
    if (el.tagName === "IMG") {
      // A picture is saved from its own address; only web links qualify.
      const src = el.currentSrc || el.src || "";
      return { url: /^https?:/i.test(src) ? src : "", list: [] };
    }
    const direct = sourcesOf(el)[0];
    if (direct) return { url: direct, list: [] };
    if (!isStreamed(el)) return { url: "", list: [] };
    try {
      const all = await api.runtime.sendMessage({ cmd: "mediaForTab" });
      const seen = new Set();
      const list = (Array.isArray(all) ? all : []).filter(
        (m) => m && typeof m.url === "string" && !seen.has(m.url) && seen.add(m.url),
      );
      const best = pickStream(list);
      return { url: best ? best.url : "", list };
    } catch (_) {
      return { url: "", list: [] }; // extension reloaded underneath us
    }
  }

  async function attach(el) {
    if (el === current) return place();
    if (dismissed.has(el) || document.fullscreenElement) return;
    if (miss.el === el && Date.now() - miss.at < MISS_RETRY_MS) return;
    if (
      el.tagName === "VIDEO" &&
      isDecorative({ muted: el.muted, loop: el.loop, controls: el.controls, autoplay: el.autoplay, width: el.clientWidth, height: el.clientHeight })
    ) {
      // A muted looping background clip: only after a deliberate hover.
      if (decoSince.el !== el) decoSince = { el, at: Date.now() };
      const wait = DECORATIVE_HOVER_MS - (Date.now() - decoSince.at);
      if (wait > 0) return void setTimeout(schedule, wait + 20);
    }
    const { url, list } = await urlFor(el);
    if (!url) {
      miss = { el, at: Date.now() };
      return;
    }
    show(el, url, list);
  }

  function mediaAt(x, y) {
    for (const el of document.querySelectorAll("video, audio, img")) {
      const r = el.getBoundingClientRect();
      const [minW, minH] = el.tagName === "IMG" ? [IMG_MIN_W, IMG_MIN_H] : [MIN_W, MIN_H];
      if (r.width < minW || r.height < minH) continue;
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
    if (!menu.hidden || (e.target === more && (e.key === "ArrowDown" || e.key === "ArrowUp"))) {
      const items = [...menu.querySelectorAll("button")];
      const i = items.indexOf(e.target);
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        return closeMenu(true);
      }
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        if (menu.hidden) openMenu();
        else items[(i + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length].focus();
        return;
      }
      if (e.key === "Home" || e.key === "End") {
        e.preventDefault();
        return items[e.key === "Home" ? 0 : items.length - 1].focus();
      }
      if (e.key === "Tab") closeMenu(false);
    }
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
    setGo(text);
    go.removeAttribute("aria-busy");
    resetTimer = setTimeout(() => {
      go.className = "go";
      setGo(labelFor(currentUrl));
    }, 2500);
  }

  async function send(url) {
    if (!url || go.getAttribute("aria-busy")) return;
    go.setAttribute("aria-busy", "true");
    setGo("Sending…");
    let reply = null;
    try {
      reply = await api.runtime.sendMessage({ cmd: "downloadMedia", url });
    } catch (_) {
      /* extension reloaded; reported as a failure below */
    }
    const result = reply && reply.result;
    if (result === "accepted") {
      // Avoid double sends: once sent, the button stays away from this element.
      const sent = current;
      if (sent) dismissed.add(sent);
      feedback("ok", "✓ Sent to TDM");
      clearTimeout(resetTimer);
      resetTimer = setTimeout(() => {
        go.className = "go";
        if (current === sent) hide();
      }, 1800);
    } else if (result === "offline") feedback("err", "TDM isn't running");
    else feedback("err", "Couldn't send");
  }

  function openMenu() {
    if (more.hidden) return;
    const best = pickStream(choices);
    const rows = [...choices].reverse().map((item) => {
      const d = describeChoice(item);
      const b = document.createElement("button");
      b.type = "button";
      b.setAttribute("role", "menuitem");
      if (best && item.url === best.url) b.className = "best";
      const t = document.createElement("b");
      t.textContent = d.label;
      t.style.fontWeight = "600";
      const sm = document.createElement("small");
      sm.textContent = d.detail;
      b.title = item.url;
      b.append(t, sm);
      b.addEventListener("click", () => {
        closeMenu(false);
        void send(item.url);
      });
      return b;
    });
    menu.replaceChildren(...rows);
    menu.hidden = false;
    more.setAttribute("aria-expanded", "true");
    (rows.find((b) => b.className === "best") || rows[0]).focus();
  }

  more.addEventListener("click", () => (menu.hidden ? openMenu() : closeMenu(true)));
  go.addEventListener("click", () => void send(currentUrl));

  close.addEventListener("click", () => {
    const back = current;
    const keyboard = viaKeyboard;
    if (back) dismissed.add(back);
    hide();
    if (keyboard && back && back.focus) back.focus();
  });
})();
