// Self-check for media detection. Run: node --test extension/
import assert from "node:assert/strict";
import "./media-core.js";

const { filenameFromTitle, isStreamed, mediaKind, pickStream, scan, sourcesOf } = globalThis.TdmMedia;

// Kind by extension, then by Content-Type; never for non-http schemes
assert.equal(mediaKind("https://e.test/v/master.m3u8?token=1"), "hls");
assert.equal(mediaKind("https://e.test/manifest.mpd"), "dash");
assert.equal(mediaKind("https://e.test/clip.MP4"), "video");
assert.equal(mediaKind("https://e.test/song.mp3#t=3"), "audio");
assert.equal(mediaKind("https://e.test/play?id=9", "application/vnd.apple.mpegURL"), "hls");
assert.equal(mediaKind("https://e.test/play?id=9", "video/webm; codecs=vp9"), "video");
assert.equal(mediaKind("https://e.test/play?id=9", "audio/mpeg"), "audio");
assert.equal(mediaKind("https://e.test/page", "text/html"), null);
assert.equal(mediaKind("https://e.test/page"), null);
assert.equal(mediaKind("blob:https://e.test/1234"), null);
assert.equal(mediaKind("data:video/mp4;base64,AAAA"), null);
assert.equal(mediaKind("file:///C:/v.mp4"), null);
// Segments are not offered, even when served as video/mp4
assert.equal(mediaKind("https://e.test/seg-1.ts", "video/mp2t"), null);
assert.equal(mediaKind("https://e.test/chunk-7.m4s", "video/mp4"), null);
assert.equal(mediaKind("https://e.test/chunk", "video/iso.segment"), null);

// Element sources: http(s) only, <source> children included, deduplicated
const el = (currentSrc, src, children = []) => ({
  currentSrc,
  src,
  tagName: "VIDEO",
  querySelectorAll: () => children.map((s) => ({ src: s })),
});
assert.deepEqual(sourcesOf(el("https://e.test/a.mp4", "https://e.test/a.mp4", ["https://e.test/a.webm"])), [
  "https://e.test/a.mp4",
  "https://e.test/a.webm",
]);
assert.deepEqual(sourcesOf(el("blob:https://e.test/x", "blob:https://e.test/x")), []);
assert.equal(isStreamed(el("blob:https://e.test/x", "")), true);
assert.equal(isStreamed(el("https://e.test/a.mp4", "")), false);

// Stream pick: newest manifest, else newest whole file
assert.equal(pickStream([]), null);
assert.equal(
  pickStream([
    { url: "a", kind: "video" },
    { url: "b", kind: "hls" },
    { url: "c", kind: "video" },
  ]).url,
  "b",
);
assert.equal(
  pickStream([
    { url: "a", kind: "audio" },
    { url: "b", kind: "video" },
  ]).url,
  "b",
);

// Filenames from page titles
assert.equal(filenameFromTitle("My clip", "https://e.test/v.mp4?x=1"), "My clip.mp4");
assert.equal(filenameFromTitle('a/b\\c:d*e?"f<g>|h', "https://e.test/v.webm"), "a b c d e f g h.webm");
assert.equal(filenameFromTitle("  ..dots..  ", "https://e.test/v.mp4"), "dots.mp4");
assert.equal(filenameFromTitle("evil\u202Efdp.exe", "https://e.test/v.mp4"), "evil fdp.exe.mp4");
assert.equal(filenameFromTitle("tab\there\nnewline", "https://e.test/v.mp4"), "tab here newline.mp4");
assert.equal(filenameFromTitle("", "https://e.test/v.mp4"), undefined);
assert.equal(filenameFromTitle("   ", "https://e.test/v.mp4"), undefined);
assert.equal(filenameFromTitle("Title", "https://e.test/videoplayback?id=1"), undefined); // no trusted extension
assert.equal(filenameFromTitle("Title", "blob:https://e.test/1"), undefined);
const long = filenameFromTitle("😀".repeat(200), "https://e.test/v.mp4");
assert.equal(Array.from(long).length, 120 + ".mp4".length);
assert.ok(!/[\ud800-\udbff](?![\udc00-\udfff])/.test(long), "no lone high surrogate");

// Document scan
const doc = {
  title: "Page",
  querySelectorAll: () => [
    el("https://e.test/a.mp4", ""),
    { ...el("blob:https://e.test/s", ""), tagName: "VIDEO" },
    { ...el("https://e.test/live.m3u8", ""), tagName: "VIDEO" },
    { ...el("https://e.test/s.mp3", ""), tagName: "AUDIO" },
  ],
};
assert.deepEqual(scan(doc), {
  title: "Page",
  streamed: 1,
  items: [
    { url: "https://e.test/a.mp4", kind: "video" },
    { url: "https://e.test/live.m3u8", kind: "hls" },
    { url: "https://e.test/s.mp3", kind: "audio" },
  ],
});
