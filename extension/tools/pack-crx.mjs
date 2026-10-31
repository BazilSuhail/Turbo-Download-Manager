// Packs a directory into a CRX3 file signed with the extension's private key.
//
//   node extension/tools/pack-crx.mjs <srcDir> <outFile> <keyPem> <publicKeyBase64>
//
// The public key is the manifest's `key` field, which pins the extension id.
// Node built-ins only. Keep the PEM outside the repository.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";

/// The extension id a CRX3 public key yields: the first 16 bytes of its
/// SHA-256, each nibble written as a letter a-p.
export function extensionIdFromPublicKey(pubKey) {
  const hex = crypto.createHash("sha256").update(pubKey).digest().subarray(0, 16).toString("hex");
  return [...hex].map((c) => String.fromCharCode(97 + parseInt(c, 16))).join("");
}

function zip(dir) {
  const files = [];
  (function walk(d, rel) {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const r = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory()) walk(path.join(d, e.name), r);
      else files.push(r);
    }
  })(dir, "");
  const local = [], central = [];
  let offset = 0;
  for (const name of files) {
    const data = fs.readFileSync(path.join(dir, name));
    const comp = zlib.deflateRawSync(data);
    const crc = zlib.crc32(data);
    const nameBuf = Buffer.from(name);
    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0); lh.writeUInt16LE(20, 4); lh.writeUInt16LE(0, 6);
    lh.writeUInt16LE(8, 8); lh.writeUInt16LE(0, 10); lh.writeUInt16LE(0x21, 12);
    lh.writeUInt32LE(crc, 14); lh.writeUInt32LE(comp.length, 18); lh.writeUInt32LE(data.length, 22);
    lh.writeUInt16LE(nameBuf.length, 26); lh.writeUInt16LE(0, 28);
    local.push(lh, nameBuf, comp);
    const ch = Buffer.alloc(46);
    ch.writeUInt32LE(0x02014b50, 0); ch.writeUInt16LE(20, 4); ch.writeUInt16LE(20, 6);
    ch.writeUInt16LE(0, 8); ch.writeUInt16LE(8, 10); ch.writeUInt16LE(0, 12); ch.writeUInt16LE(0x21, 14);
    ch.writeUInt32LE(crc, 16); ch.writeUInt32LE(comp.length, 20); ch.writeUInt32LE(data.length, 24);
    ch.writeUInt16LE(nameBuf.length, 28); ch.writeUInt32LE(offset, 42);
    central.push(ch, nameBuf);
    offset += lh.length + nameBuf.length + comp.length;
  }
  const cdBuf = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(files.length, 8); end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(cdBuf.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, cdBuf, end]);
}

const varint = (n) => { const out = []; while (n >= 0x80) { out.push((n & 0x7f) | 0x80); n = Math.floor(n / 128); } out.push(n); return Buffer.from(out); };
const bytesField = (field, buf) => Buffer.concat([varint((field << 3) | 2), varint(buf.length), buf]);

export function pack(srcDir, outFile, keyPem, pubB64) {
  const pubKey = Buffer.from(pubB64, "base64");
  const crxId = crypto.createHash("sha256").update(pubKey).digest().subarray(0, 16);
  const signedData = bytesField(1, crxId);
  const archive = zip(srcDir);
  const key = crypto.createPrivateKey(fs.readFileSync(keyPem));
  const len = Buffer.alloc(4); len.writeUInt32LE(signedData.length);
  const toSign = Buffer.concat([Buffer.from("CRX3 SignedData\0"), len, signedData, archive]);
  const sig = crypto.sign("sha256", toSign, key);
  const proof = Buffer.concat([bytesField(1, pubKey), bytesField(2, sig)]);
  const header = Buffer.concat([bytesField(2, proof), bytesField(10000, signedData)]);
  const prefix = Buffer.alloc(12);
  prefix.write("Cr24", 0); prefix.writeUInt32LE(3, 4); prefix.writeUInt32LE(header.length, 8);
  fs.writeFileSync(outFile, Buffer.concat([prefix, header, archive]));
  console.log("wrote", outFile, archive.length, "bytes of archive");
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  const [srcDir, outFile, keyPem, pubB64] = process.argv.slice(2);
  if (!pubB64) {
    console.error("usage: pack-crx.mjs <srcDir> <outFile> <keyPem> <publicKeyBase64>");
    process.exit(2);
  }
  pack(srcDir, outFile, keyPem, pubB64);
}
