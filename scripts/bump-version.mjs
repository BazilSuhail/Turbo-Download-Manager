// Sets the release version everywhere it is written down.
//
//   node scripts/bump-version.mjs 1.1.0
//
// Updates: Cargo.toml (package version), Cargo.lock (the tdm entry),
// extension/manifest.json, and packaging/linux/app.tdm.metainfo.xml (adds a
// <release> entry for the new version if it is missing). Then run
// `cargo build` once so Cargo.lock and extension/dist are refreshed.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2];

if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version ?? "")) {
  console.error("Usage: node scripts/bump-version.mjs <major.minor.patch>   e.g. 1.1.0");
  process.exit(1);
}

const read = (p) => readFileSync(join(ROOT, p), "utf8");
const write = (p, text) => writeFileSync(join(ROOT, p), text);
const replaceOnce = (text, pattern, replacement, label) => {
  if (!pattern.test(text)) throw new Error(`could not find ${label}`);
  return text.replace(pattern, replacement);
};

// Cargo.toml: the first `version = "..."` line is the [package] version.
{
  const p = "Cargo.toml";
  write(p, replaceOnce(read(p), /^version = "[^"]+"/m, `version = "${version}"`, "package version in Cargo.toml"));
}

// Cargo.lock: only the tdm package entry.
{
  const p = "Cargo.lock";
  write(
    p,
    replaceOnce(read(p), /(name = "tdm"\nversion = ")[^"]+(")/, `$1${version}$2`, "tdm entry in Cargo.lock"),
  );
}

// extension/manifest.json
{
  const p = "extension/manifest.json";
  const manifest = JSON.parse(read(p));
  manifest.version = version;
  write(p, JSON.stringify(manifest, null, 2) + "\n");
}

// AppStream release history: add an entry for this version if it is missing.
{
  const p = "packaging/linux/app.tdm.metainfo.xml";
  let xml = read(p);
  if (!xml.includes(`<release version="${version}"`)) {
    const today = new Date().toISOString().slice(0, 10);
    xml = replaceOnce(
      xml,
      /(<releases>\s*)/,
      `$1<release version="${version}" date="${today}"/>\n    `,
      "<releases> block in the AppStream file",
    );
    write(p, xml);
  }
}

console.log(`Version set to ${version} in Cargo.toml, Cargo.lock, extension/manifest.json and the AppStream file.`);
console.log("Next: cargo build, commit, then tag v" + version + " and push.");
