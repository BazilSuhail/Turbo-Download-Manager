// Writes the update manifest Chrome and Edge poll for the packed extension.
//
//   node extension/tools/make-update-xml.mjs <codebaseUrl> <outFile> [manifest.json]
//
// The appid comes from the manifest's `key`, and the version from its
// `version`, so the file cannot drift from the package it points at.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { extensionIdFromPublicKey } from "./pack-crx.mjs";

const xmlEscape = (s) => s.replace(/[&<>'"]/g, (c) => `&#${c.charCodeAt(0)};`);

export function updateXml(manifest, codebaseUrl) {
  const appid = extensionIdFromPublicKey(Buffer.from(manifest.key, "base64"));
  return `<?xml version='1.0' encoding='UTF-8'?>
<gupdate xmlns='http://www.google.com/update2/response' protocol='2.0'>
  <app appid='${appid}'>
    <updatecheck codebase='${xmlEscape(codebaseUrl)}' version='${xmlEscape(manifest.version)}' />
  </app>
</gupdate>
`;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  const [codebaseUrl, outFile, manifestPath] = process.argv.slice(2);
  if (!codebaseUrl || !outFile) {
    console.error("usage: make-update-xml.mjs <codebaseUrl> <outFile> [manifest.json]");
    process.exit(2);
  }
  const defaultManifest = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "manifest.json");
  const manifest = JSON.parse(fs.readFileSync(manifestPath ?? defaultManifest, "utf8"));
  fs.writeFileSync(outFile, updateXml(manifest, codebaseUrl));
  console.log("wrote", outFile);
}
