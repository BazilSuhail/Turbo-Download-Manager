# Turbo Download Manager (TDM)

A fast, segmented download manager for Windows, macOS and Linux, with browser capture for Chrome, Edge, Brave, Opera, Vivaldi and Firefox.

[![Licence: MIT OR Apache-2.0](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-orange.svg)](LICENSE-MIT)
[![Rust](https://img.shields.io/badge/Rust-2024%20edition-orange.svg)](https://www.rust-lang.org)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](#install)

TDM keeps the layout people already know (menu, toolbar, link bar, category sidebar, download list, status bar) in its own orange-on-charcoal look, with a light theme one click away.

## Features

### Downloading

- Multi-connection HTTP/HTTPS downloads with pause and resume (resume state is kept in a `<file>.tdmstate` sidecar)
- FTP, plus HLS/DASH media manifests saved as a playable file
- A queue with a max-active limit, a scheduler with start and stop times, and a global speed limiter
- Proxy support: the Windows system proxy, or `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` elsewhere
- Automatic categories (Programs, Music, Video, Documents, Compressed), each with its own folder
- A file is never overwritten: a name clash becomes `name (2).ext`

### Interface

- TDM Dark (default) and TDM Light, switched with an animated toggle
- Download cards with a gradient progress bar, status pills, speed and time left, and a live speed sparkline in the status bar
- Toast notifications for finished downloads, captures and errors
- Search box, batch add (`https://site/img[001-120].jpg`, `[a-z]`) and an opt-in clipboard watcher
- Per-download actions: Open, Show in folder, Verify SHA-256, Copy link, Delete
- Keyboard shortcuts: Ctrl+N new link, Ctrl+F search, Ctrl+Shift+V paste link, Space pause/resume, Delete, Esc
- No redraws when nothing is moving, so the CPU stays idle

### Browser integration

- The extension hands matching downloads to TDM and cancels the browser's own copy
- TDM starts by itself when a download is captured while it is closed; if it is already running, its window comes to the front
- A floating **⬇ TDM** button on videos, and a popup listing the media on the current tab (media detection is an opt-in permission)
- Right-click: Download link, Download media, or Download selected links with TDM
- The native host is registered by the installer and again at every app start, so you never copy an extension id by hand

## Install

Run the installer for your OS (see [Building](#building) for how the packages are made):

| OS | Package |
|---|---|
| Windows | `tdm_<ver>_x64-setup.exe` (per-user installer with shortcuts and launch at login) or `.msi` |
| macOS | `.dmg` |
| Linux | `.deb`, `.rpm` or `.AppImage` |

Then add the browser extension from inside TDM:

1. Open TDM and click **Add** in the **BROWSERS** section of the sidebar. This opens "Add TDM to your browser".
2. Click **Add to** your browser. TDM copies the extension folder path to your clipboard and opens the browser's extensions page.
3. Turn on **Developer mode**, click **Load unpacked**, press Ctrl+V and choose **Select Folder**.
4. The page in step 1 turns green once TDM is connected.

Firefox needs a build step first (`node extension/build.mjs`), then **Load Temporary Add-on** on `dist/firefox/manifest.json`. See `extension/README.md` for details and `extension/selftest.md` for a step-by-step check.

> **Private windows:** the browser does not let an extension run in Incognito until you allow it. After the steps above, open TDM's Details page in the browser and switch on **Allow in Incognito** once. TDM cannot do this for you.

## How browser capture works

When you start a download in your browser, the TDM extension sees it and asks the TDM app whether it wants it. If the app says yes, the extension cancels the browser's copy, so the file is downloaded by TDM instead. If TDM is closed, the extension starts it first.

## Building

```sh
cargo build --release          # target/release/tdm(.exe)
cargo test                     # Rust tests
node --test extension/         # extension tests
node extension/build.mjs       # extension/dist/{chrome,firefox} plus zips

cargo install cargo-packager --locked
cargo packager --release --formats nsis,wix      # Windows
cargo packager --release --formats app,dmg       # macOS
cargo packager --release --formats deb,appimage  # Linux
```

Linux needs the iced and wgpu system headers. `packaging/README.md` lists them, along with the `.rpm` build and the GitHub Actions release workflow.

## Command line

```
tdm                              start the app (or bring the running one to the front)
tdm --register-host [ids...]     register the browser native host (defaults to the bundled extension)
tdm --unregister-host            remove the registration (the uninstallers use this)
```

## Project layout

| Path | What |
|---|---|
| `src/engine.rs`, `ftp.rs`, `media.rs` | download engines |
| `src/main.rs`, `src/ui/` | the iced interface: theme, icons, menu, options |
| `src/ipc.rs`, `native_host.rs`, `register.rs` | browser bridge, single instance, host registration |
| `src/scheduler.rs`, `store.rs`, `model.rs` | schedule, persistence, settings |
| `assets/icons/` | the app's own SVG icons |
| `extension/` | Manifest V3 browser extension |
| `packaging/` | installers, icons (`gen_art.py`), desktop and metadata files |

## Maintainers

- Bazil Suhail
- Abdullah Masood

## Licence

TDM is dual-licensed under either of

- the [MIT licence](LICENSE-MIT), or
- the [Apache Licence, Version 2.0](LICENSE-APACHE),

at your option. Unless you state otherwise, any contribution you intentionally submit for inclusion in TDM is dual-licensed in the same way, without any additional terms or conditions.

Third-party crates are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
