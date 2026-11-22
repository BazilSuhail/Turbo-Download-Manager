"use strict";
//
// Link grabber helpers, pure so `grab.test.mjs` can run them under node.
// The page-side collector lives in popup.js; this file only sorts what it found.
//
import { extensionOf, httpUrl } from "./filter.js";

export const GRAB_CAP = 2000;

const TYPES = {
  video: "3gp avi flv m4v mkv mov mp4 mpeg mpg ogv ts webm wmv",
  audio: "aac aif aiff flac m4a mid mka mp3 oga ogg opus wav wma",
  archives: "7z bz2 cab gz iso rar tar tgz xz zip zst",
  documents: "csv doc docx epub md odt pdf ppt pptx rtf txt xls xlsx",
  programs: "apk appimage bat deb dmg exe jar msi pkg rpm sh",
  images: "avif bmp gif ico jpeg jpg png svg tif tiff webp",
};
const BY_EXT = new Map();
for (const [type, list] of Object.entries(TYPES)) for (const e of list.split(" ")) BY_EXT.set(e, type);

/// "video", "audio", "archives", "documents", "programs", "images" or "other".
export function typeOf(url) {
  return BY_EXT.get(extensionOf(url)) || "other";
}

/// Raw `{ url, text }` finds from the page to a deduped, http(s)-only list of
/// `{ url, name, text, type }`, at most `cap` long. `truncated` says whether links were dropped.
export function normalizeLinks(raw, cap = GRAB_CAP) {
  const seen = new Set();
  const items = [];
  let truncated = false;
  for (const r of raw || []) {
    const u = r && httpUrl(r.url);
    if (!u) continue;
    u.hash = ""; // #section anchors are the same download
    const key = u.href;
    if (seen.has(key)) continue;
    seen.add(key);
    if (items.length >= cap) {
      truncated = true;
      break;
    }
    let name = u.pathname.split("/").filter(Boolean).pop() || u.hostname;
    try {
      name = decodeURIComponent(name);
    } catch (_) {}
    items.push({ url: key, name, text: String(r.text || "").trim().slice(0, 120), type: typeOf(u) });
  }
  return { items, truncated };
}

/// Items matching a type chip ("all" or a type) and a case-insensitive text query.
export function filterLinks(items, type, query) {
  const q = String(query || "").trim().toLowerCase();
  return items.filter(
    (i) => (type === "all" || i.type === type) && (!q || i.url.toLowerCase().includes(q) || i.text.toLowerCase().includes(q)),
  );
}
