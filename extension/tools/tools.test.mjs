// node --test extension/tools/
import { test } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { extensionIdFromPublicKey, pack } from "./pack-crx.mjs";
import { updateXml } from "./make-update-xml.mjs";

const manifest = JSON.parse(fs.readFileSync(new URL("../manifest.json", import.meta.url), "utf8"));
const PINNED_ID = "glkodjlchifmdbokmofigopbgdbabmge";

test("the manifest key derives the pinned extension id", () => {
  assert.equal(extensionIdFromPublicKey(Buffer.from(manifest.key, "base64")), PINNED_ID);
});

test("update.xml names the id, codebase and manifest version", () => {
  const xml = updateXml(manifest, "http://127.0.0.1:8765/tdm.crx?a=1&b=2");
  assert.match(xml, new RegExp(`appid='${PINNED_ID}'`));
  assert.match(xml, new RegExp(`version='${manifest.version}'`));
  assert.match(xml, /codebase='http:\/\/127\.0\.0\.1:8765\/tdm.crx\?a=1&#38;b=2'/);
});

test("a packed CRX3 has the Cr24 header and version 3", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "tdm-pack-test-"));
  try {
    const src = path.join(dir, "src");
    fs.mkdirSync(src);
    fs.writeFileSync(path.join(src, "manifest.json"), "{}");
    // A throwaway key: the test must not depend on the real signing key.
    const { privateKey, publicKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
    const keyPem = path.join(dir, "key.pem");
    fs.writeFileSync(keyPem, privateKey.export({ type: "pkcs1", format: "pem" }));
    const pubB64 = publicKey.export({ type: "spki", format: "der" }).toString("base64");
    const out = path.join(dir, "out.crx");
    const log = console.log;
    console.log = () => {};
    try {
      pack(src, out, keyPem, pubB64);
    } finally {
      console.log = log;
    }
    const bytes = fs.readFileSync(out);
    assert.equal(bytes.subarray(0, 4).toString("ascii"), "Cr24");
    assert.equal(bytes.readUInt32LE(4), 3);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

