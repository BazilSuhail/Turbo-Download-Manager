# What changed: Turbo Download Manager

This file summarises everything done in this pass. The work was split across
three implementation agents (UI, platform/installer, extension) with separate
file ownership, then checked by two senior review agents (Rust app;
extension/installer/licensing), and finished with fixes from their findings.

## 1. Made it legally distributable
- **Names**: crate `tdm`, product "Turbo Download Manager" (short name TDM), native host `com.tdm.host`.
- **Deleted all IDM artwork**: `assets/toolbar/`, the old `assets/icons/`, `assets/manifest.json` and `assets/NOTICE.md`.
- **Rewrote the menu by hand**, replacing the copy generated from IDMan.exe (File, Downloads, View, Help).
- **Deleted `src/ui/layout.rs`**, which held IDM's dialog measurements.
- **Removed IDM references** from code comments, registry value names and test data across `src/`. The anonymous FTP password is now `tdm@`, the User-Agent is `TDM/<version>`, and the resume file is `.tdmstate`.
- **New original artwork**:
  - 29 hand-written SVG icons in `assets/icons/`
  - the app icon, drawn in code (`src/ui/assets.rs`)
  - installer icons and bitmaps from `packaging/gen_art.py`
  - extension icons from `extension/tools/make-icons.mjs`
- **Licensing**: added `LICENSE-MIT` and `LICENSE-APACHE`, and set `license = "MIT OR Apache-2.0"`.

## 2. New UI
- **Theme**: TDM Dark (warm charcoal with an orange `#FF7A1A`→`#FFB347` accent) and TDM Light, both with contrast tests. The toggle cross-fades the whole UI and spins the sun/moon icon.
- **Look**: card-style rows, gradient progress bars with a moving highlight, status pills, sidebar icons with count badges, an empty state, and a speed sparkline in the status bar.
- **Motion**: toasts slide in and fade out, and dialogs fade and zoom in. Per-frame updates only run while something is moving.
- **New features**:
  - search box
  - batch add (`src/batch.rs`)
  - clipboard watcher
  - Open / Show in folder
  - Verify SHA-256 (`src/checksum.rs`)
  - right-click row menu
  - keyboard shortcuts
  - a launch-at-login switch that actually works

## 3. Browser integration
- **Pinned extension id** (`glkodjlchifmdbokmofigopbgdbabmge`, via `key` in the manifest). The app registers the native host at every start, and the installers do it too. No more manual id copying.
- **Browsers**: Chrome, Edge, Chromium, Brave, Vivaldi and Opera, plus **Firefox** (separate host manifest; `extension/build.mjs` makes `dist/firefox`).
- **Auto-open**: a captured download starts TDM if it's closed, and the host waits up to 10 s for it. If TDM is already running, its window is raised and focused.
  - The browser's startup handshake is now answered from saved settings while the app is closed, so capture works from the first download.
- **Single instance**: launching TDM again brings the running window forward.
- **Extension**:
  - a video-grabber button (closed Shadow DOM)
  - a popup listing the media on the current tab
  - restyled options page
  - three context menus
  - the app's capture on/off switch is respected

## 4. Cross-platform
- Native-host registration for Linux and macOS: manifests go into each installed browser's directory.
- Config and download folders come from the `dirs` crate.
- Local time via `localtime_r` on Unix.
- Proxy read from environment variables off Windows.
- Launch at login: Run key on Windows, XDG autostart on Linux, LaunchAgent on macOS.

## 5. Installers
- `cargo packager` configs for NSIS, MSI, `.app`/`.dmg`, `.deb` and AppImage, plus `cargo-generate-rpm`.
- A **custom NSIS installer** (`packaging/windows/installer.nsi`):
  - dark orange welcome and finish pages
  - license page
  - options for desktop shortcut, Start menu and launch at login
  - keeps the user's launch-at-login choice on upgrade
  - registers the host on install and removes it on uninstall
- **MSI** registers and unregisters the host too (`host-registration.wxs`).
- **CI**: `.github/workflows/release.yml` builds on Windows, macOS and Ubuntu 22.04 when a `v*` tag is pushed.
- Built here:
  - `target/release/tdm_0.1.0_x64-setup.exe`
  - `target/release/tdm_0.1.0_x64_en-US.msi`

## 6. Bugs fixed by the senior reviews
- A browser capture or batch add could **overwrite an existing file** or share one file between entries. Clashing names now get numbered: `name (2).ext`.
- A new download over an older, longer file **kept the old tail** (corrupt file). The file is now truncated unless resuming.
- A corrupt `downloads.json` or settings file was **silently overwritten**. It is now backed up to `*.corrupt` first.
- The download folder on Linux/macOS was relative to the working directory. It now uses the real Downloads folder.
- The startup registration wiped custom extension ids. They are now kept.
- The IPC token file was briefly world-readable on Unix. It is now created owner-only.
- The extension's timeout (8 s) was shorter than the app's cold start, which caused **double downloads**. It is now 20 s.
- Downloads arriving during the extension's handshake slipped past capture.
- The installer turned off launch at login on upgrade.
- The CI Linux job failed on `ubuntu-latest` because `libfuse2` is missing.

## Tests
- Rust: 171 library tests, 9 for the app and batch expansion, and 19 integration tests: all pass. `cargo clippy --all-targets` is clean.
- Extension: `node --test extension/` passes.

## Not done or still to do
- **Not tested by hand**:
  - running the installers
  - loading the extension in a real browser
  - TDM Light, populated lists and the animations on screen
- **Linux/macOS builds**: they compile only in CI. The Unix code paths were type-checked in a scratch crate, but the full crate can't cross-compile here because `aws-lc-sys` needs a C cross-compiler.
- **Store publishing**: the Chrome Web Store rejects a manifest with `key` and assigns its own id. Drop `key` for store uploads and add the store ids to `CHROME_EXTENSION_ID` in `src/register.rs`.
- **Code signing**: none yet, so expect SmartScreen and Gatekeeper warnings.
- **Placeholder URLs**: `repository` and `homepage` in `Cargo.toml`.
- **Known limitations**:
  - Two GUIs started at the exact same instant can both open.
  - In Firefox, a tiny file can finish before TDM answers and end up downloaded twice.
  - App-side capture settings reach the extension only when it reconnects.
  - Partial downloads from earlier pre-release builds restart from zero.
- **Before publishing** (needs a human decision):
  - Don't ship the parent folder (`IDM-SPEC.md`, `reports/`, `rea-workspace/`, `prev-idm/`), and keep it out of the public repo's history.
  - The toolbar set, category names and `.lng` language format resemble IDM's. They aren't copied code or art, but get a quick legal check before selling.
