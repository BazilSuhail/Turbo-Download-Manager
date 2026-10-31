"use strict";
//
// Media detection shared by the content script (media.js), the popup's
// on-demand page scan and the background's network observer.
//
// A classic script, not a module: registered content scripts cannot be
// modules. It publishes `globalThis.TdmMedia`, which the background imports
// for its side effect and media-core.test.mjs exercises under node.
//
(function (root) {
  // Whole files and stream manifests only.
  const BY_EXTENSION = new Map(
    Object.entries({
      m3u8: "hls",
      mpd: "dash",
      mp4: "video",
      m4v: "video",
      webm: "video",
      mov: "video",
      mkv: "video",
      ogv: "video",
      mp3: "audio",
      m4a: "audio",
      aac: "audio",
      oga: "audio",
      ogg: "audio",
      opus: "audio",
      flac: "audio",
      wav: "audio",
    }),
  );
  // One stream segment is useless on its own, and segments are often served
  // as plain video/mp4, so they are excluded by name before the type is read.
  const SEGMENT_EXTENSIONS = new Set(["ts", "m4s", "m4f", "cmfv", "cmfa"]);
  const BY_TYPE = new Map(
    Object.entries({
      "application/vnd.apple.mpegurl": "hls",
      "application/x-mpegurl": "hls",
      "audio/mpegurl": "hls",
      "audio/x-mpegurl": "hls",
      "application/dash+xml": "dash",
    }),
  );
  const SEGMENT_TYPES = new Set(["video/mp2t", "video/iso.segment", "audio/iso.segment"]);

  // Same gate as filter.js's httpUrl: blob:, data:, file: have nothing to fetch.
  function http(raw) {
    let u;
    try {
      u = new URL(raw);
    } catch (_) {
      return null;
    }
    return u.protocol === "http:" || u.protocol === "https:" ? u : null;
  }

  function extension(u) {
    const m = /\.([a-z0-9]{2,5})$/i.exec(u.pathname);
    return m ? m[1].toLowerCase() : "";
  }

  /// "hls" | "dash" | "video" | "audio" | null for a URL and optional
  /// Content-Type. The URL's extension wins over the header.
  function mediaKind(raw, contentType) {
    const u = http(raw);
    if (!u) return null;
    const ext = extension(u);
    if (SEGMENT_EXTENSIONS.has(ext)) return null;
    if (BY_EXTENSION.has(ext)) return BY_EXTENSION.get(ext);
    const type = String(contentType || "").split(";")[0].trim().toLowerCase();
    if (BY_TYPE.has(type)) return BY_TYPE.get(type);
    if (SEGMENT_TYPES.has(type)) return null;
    if (type.startsWith("video/")) return "video";
    if (type.startsWith("audio/")) return "audio";
    return null;
  }

  /// The fetchable (http/https) sources of a <video>/<audio>, deduplicated.
  function sourcesOf(el) {
    const raw = [el.currentSrc, el.src];
    const children = el.querySelectorAll ? el.querySelectorAll("source[src]") : [];
    for (const s of children) raw.push(s.src);
    return [...new Set(raw.filter((r) => r && http(r)))];
  }

  /// Playing from MediaSource or an inline buffer: the element's own URL is
  /// useless and only a network-observed URL can stand in for it.
  function isStreamed(el) {
    return /^(blob|data|mediastream):/i.test(el.currentSrc || el.src || "");
  }

  /// Best network-observed candidate for a streamed element. Newest first: a
  /// player that switched quality last fetched what it is playing now.
  function pickStream(list) {
    const recent = [...(list || [])].reverse();
    return (
      recent.find((m) => m.kind === "hls" || m.kind === "dash") ||
      recent.find((m) => m.kind === "video") ||
      recent.find((m) => m.kind === "audio") ||
      null
    );
  }

  // Characters Windows forbids, controls, and the invisible bidi/zero-width
  // ones that can disguise an extension. The host sanitises again; this just
  // keeps the suggestion readable.
  const UNSAFE = /[<>:"/\\|?*\u0000-\u001f\u007f​-‏‪-‮⁦-⁩﻿]/g;
  const MAX_STEM = 120;

  /// "Page title.ext" for a media URL, or undefined when the URL has no media
  /// extension we trust (the host then names it from the response instead of
  /// us guessing a wrong extension).
  function filenameFromTitle(title, raw) {
    const u = http(raw);
    if (!u) return undefined;
    const ext = extension(u);
    if (!BY_EXTENSION.has(ext)) return undefined;
    const cleaned = String(title || "")
      .replace(UNSAFE, " ")
      .replace(/\s+/g, " ")
      .trim();
    // Array.from so a cut never splits a surrogate pair.
    const stem = Array.from(cleaned)
      .slice(0, MAX_STEM)
      .join("")
      .replace(/^[.\s]+|[.\s]+$/g, "");
    return stem ? `${stem}.${ext}` : undefined;
  }

  /// Every directly fetchable media source in a document, plus a count of
  /// elements that stream from blob:/data: and so need network detection.
  function scan(doc) {
    const items = [];
    let streamed = 0;
    for (const el of doc.querySelectorAll("video, audio")) {
      const urls = sourcesOf(el);
      if (!urls.length && isStreamed(el)) streamed++;
      const tag = String(el.tagName).toLowerCase();
      for (const url of urls) {
        const k = mediaKind(url);
        items.push({ url, kind: k === "hls" || k === "dash" ? k : tag });
      }
    }
    return { title: doc.title || "", items, streamed };
  }

  root.TdmMedia = { filenameFromTitle, isStreamed, mediaKind, pickStream, scan, sourcesOf };
})(globalThis);
