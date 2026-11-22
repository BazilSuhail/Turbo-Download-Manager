// Self-check for the link grabber helpers. Run: bun extension/grab.test.mjs
import assert from "node:assert/strict";
import { filterLinks, normalizeLinks, typeOf } from "./grab.js";

assert.equal(typeOf(new URL("https://e.test/a.MP4?x=1")), "video");
assert.equal(typeOf(new URL("https://e.test/a.zip")), "archives");
assert.equal(typeOf(new URL("https://e.test/page")), "other");

const { items, truncated } = normalizeLinks([
  { url: "https://e.test/a.zip#top", text: " A " },
  { url: "https://e.test/a.zip", text: "dup" },
  { url: "javascript:void(0)" },
  { url: "mailto:a@e.test" },
  { url: "blob:https://e.test/x" },
  { url: "http://e.test/song%20one.mp3" },
  null,
]);
assert.equal(items.length, 2);
assert.equal(truncated, false);
assert.equal(items[0].text, "A");
assert.equal(items[1].name, "song one.mp3");
assert.equal(items[1].type, "audio");

// Cap: stops at the limit and says so; an exact fit is not truncated.
const many = Array.from({ length: 10 }, (_, i) => ({ url: `https://e.test/${i}.zip` }));
const capped = normalizeLinks(many, 3);
assert.deepEqual([capped.items.length, capped.truncated], [3, true]);
assert.equal(normalizeLinks(many, 10).truncated, false);

assert.equal(filterLinks(items, "audio", "").length, 1);
assert.equal(filterLinks(items, "all", "SONG").length, 1);
assert.equal(filterLinks(items, "all", "").length, 2);
assert.equal(filterLinks(items, "video", "").length, 0);
