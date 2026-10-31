// Writes browser-specific packages: node extension/build.mjs
//
//   extension/dist/chrome/   manifest.json as-is (Chrome, Edge, Brave, Opera, Vivaldi)
//   extension/dist/firefox/  manifest rewritten for Firefox 128+
//   extension/dist/tdm-<browser>-<version>.zip  when `zip` or bsdtar is available
//
// Why two folders instead of one manifest for both: Chromium before 121
// rejects `background.scripts` in MV3, and Firefox does not run MV3 service
// workers, so no single `background` key loads everywhere we support. The
// source folder stays loadable unpacked in Chromium for day-to-day work.
import { cpSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = dirname(fileURLToPath(import.meta.url));
const DIST = join(SRC, "dist");
// Runtime files only: tests, tools and docs stay out of the packages.
const FILES = [
  "background.js",
  "filter.js",
  "media-core.js",
  "media.js",
  "popup.html",
  "popup.js",
  "popup.css",
  "options.html",
  "options.js",
  "ui.css",
  "icons",
];
const FIREFOX_ID = "tdm@tdm.app";

const manifest = JSON.parse(readFileSync(join(SRC, "manifest.json"), "utf8"));

function firefoxManifest(m) {
  const ff = structuredClone(m);
  delete ff.key; // Chromium-only id pinning; Firefox's id comes from gecko.id
  delete ff.minimum_chrome_version;
  delete ff.incognito; // Chromium-only; Firefox has its own "Run in Private Windows" switch
  // An event page instead of a service worker; same module file.
  ff.background = { scripts: [m.background.service_worker], type: "module" };
  ff.browser_specific_settings = {
    gecko: {
      id: FIREFOX_ID,
      // 128 ESR: optional_host_permissions, storage.session and
      // scripting.registerContentScripts are all present.
      strict_min_version: "128.0",
      // Required by addons.mozilla.org for new submissions; we collect nothing.
      data_collection_permissions: { required: ["none"] },
    },
  };
  return ff;
}

function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    return statSync(p).isDirectory() ? walk(p) : [p];
  });
}

const has = (cmd, args) => {
  const r = spawnSync(cmd, args, { encoding: "utf8" });
  return r.status === 0 ? r.stdout + r.stderr : null;
};

// Entries are listed explicitly (no "./" prefix) so manifest.json sits at the
// archive root, which addons.mozilla.org requires.
function zip(dir, out) {
  const files = walk(dir).map((p) => relative(dir, p).split("\\").join("/"));
  // GNU tar cannot write zip, so only bsdtar counts (Windows ships it in System32).
  const bsdtar = ["tar", join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe")].find((t) =>
    (has(t, ["--version"]) || "").includes("bsdtar"),
  );
  let r;
  if (has("zip", ["-v"])) r = spawnSync("zip", ["-q", "-X", out, ...files], { cwd: dir });
  else if (bsdtar) r = spawnSync(bsdtar, ["-a", "-cf", out, ...files], { cwd: dir });
  else return console.log(`  (no zip or bsdtar found; skipped ${relative(SRC, out)})`);
  if (r.status !== 0) throw new Error(`zipping ${dir} failed: ${r.stderr}`);
  console.log(`  ${relative(SRC, out)}`);
}

rmSync(DIST, { recursive: true, force: true });
for (const [browser, m] of [
  ["chrome", manifest],
  ["firefox", firefoxManifest(manifest)],
]) {
  const out = join(DIST, browser);
  mkdirSync(out, { recursive: true });
  for (const f of FILES) cpSync(join(SRC, f), join(out, f), { recursive: true });
  writeFileSync(join(out, "manifest.json"), JSON.stringify(m, null, 2) + "\n");
  console.log(`dist/${browser}/`);
  zip(out, join(DIST, `tdm-${browser}-${m.version}.zip`));
}
