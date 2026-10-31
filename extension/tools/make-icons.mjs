// Draws TDM's extension icons: node extension/tools/make-icons.mjs
//
// The TDM mark: a bold orange T with three forward chevrons on a charcoal
// rounded tile. Same geometry as packaging/gen_art.py and src/ui/assets.rs;
// change all three together. Sampled 4x4 per pixel and encoded as PNG with
// node built-ins only (zlib), so the output is byte-for-byte reproducible.
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const OUT = fileURLToPath(new URL("../icons/", import.meta.url));
const SIZES = [16, 32, 48, 128];
const SUPERSAMPLE = 4; // 4x4 samples per pixel for anti-aliased edges

const TILE_TOP = [46, 40, 35];
const TILE_BOTTOM = [22, 19, 17];
const MARK_TOP = [255, 184, 77];
const MARK_BOTTOM = [255, 106, 26];

const CHEVRON_TIPS = [0.7, 0.79, 0.88];
const CHEVRON_ARM = 0.1; // horizontal run of each arm
const CHEVRON_RISE = 0.17; // vertical reach of each arm from the centre line
const CHEVRON_HALF = 0.034; // half the stroke width

// ---------------------------------------------------------------- shapes
// All in unit coordinates: (0,0) top-left, (1,1) bottom-right.

function inRoundedSquare(x, y, r = 0.22) {
  const dx = Math.max(Math.abs(x - 0.5) - (0.5 - r), 0);
  const dy = Math.max(Math.abs(y - 0.5) - (0.5 - r), 0);
  return dx * dx + dy * dy <= r * r;
}

function segDist(px, py, ax, ay, bx, by) {
  const dx = bx - ax, dy = by - ay;
  const t = Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)));
  return Math.hypot(px - ax - t * dx, py - ay - t * dy);
}

function inChevron(x, y, tip) {
  const back = tip - CHEVRON_ARM;
  const dUp = segDist(x, y, back, 0.5 - CHEVRON_RISE, tip, 0.5);
  const dDown = segDist(x, y, tip, 0.5, back, 0.5 + CHEVRON_RISE);
  return Math.min(dUp, dDown) <= CHEVRON_HALF;
}

function inT(x, y) {
  const bar = x >= 0.12 && x <= 0.56 && y >= 0.3 && y <= 0.41;
  const stem = x >= 0.29 && x <= 0.39 && y >= 0.3 && y <= 0.72;
  return bar || stem;
}

function sample(x, y) {
  if (!inRoundedSquare(x, y)) return null;
  if (inT(x, y) || CHEVRON_TIPS.some((tip) => inChevron(x, y, tip))) {
    const t = Math.min(1, Math.max(0, (y - 0.3) / 0.42));
    return MARK_TOP.map((a, i) => a + (MARK_BOTTOM[i] - a) * t);
  }
  return TILE_TOP.map((a, i) => a + (TILE_BOTTOM[i] - a) * y);
}

function render(size) {
  const px = Buffer.alloc(size * size * 4);
  const n = SUPERSAMPLE * SUPERSAMPLE;
  for (let py = 0; py < size; py++) {
    for (let pxl = 0; pxl < size; pxl++) {
      let r = 0, g = 0, b = 0, a = 0;
      for (let s = 0; s < n; s++) {
        const x = (pxl + ((s % SUPERSAMPLE) + 0.5) / SUPERSAMPLE) / size;
        const y = (py + (Math.floor(s / SUPERSAMPLE) + 0.5) / SUPERSAMPLE) / size;
        const c = sample(x, y);
        if (!c) continue;
        r += c[0]; g += c[1]; b += c[2]; a++;
      }
      const o = (py * size + pxl) * 4;
      // Average only covered samples for colour, coverage for alpha (straight alpha).
      if (a) px.set([r / a, g / a, b / a, (255 * a) / n].map(Math.round), o);
    }
  }
  return px;
}

// ------------------------------------------------------------------ PNG

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  for (let k = 0; k < 8; k++) n = n & 1 ? 0xedb88320 ^ (n >>> 1) : n >>> 1;
  return n >>> 0;
});
function crc32(buf) {
  let c = 0xffffffff;
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

function png(size, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr.set([8, 6, 0, 0, 0], 8); // 8-bit RGBA, no interlace
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4); // filter 0
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// Self-check: the CRC of "IEND" with no data is a well-known constant.
if (crc32(Buffer.from("IEND")) !== 0xae426082) throw new Error("crc32 is broken");

for (const size of SIZES) {
  const file = `${OUT}icon${size}.png`;
  writeFileSync(file, png(size, render(size)));
  console.log(file);
}
