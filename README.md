<p align="center">
  <img src="assets/tdm-logo.png" alt="Turbo Download Manager logo" width="150">
</p>

<h1 align="center">Turbo Download Manager</h1>

<p align="center">
  <b>Split every download into parallel segments, resume what breaks, and capture files straight from your browser.</b>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform: Windows and Linux">
  <a href="https://github.com/BazilSuhail/Turbo-Download-Manager/blob/main/LICENSE-MIT"><img src="https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue" alt="Licence: MIT OR Apache-2.0"></a>
</p>

| [Why TDM](#why-tdm) | [How to download](#how-to-download) | [Browser buttons](#browser-buttons) | [Features](#what-it-does) | [Known limits](#known-limits) | [Verify](#how-to-verify) | [For developers](#for-developers) | [Roadmap](#roadmap) |
| --- | --- | --- | --- | --- | --- | --- | --- |

## Why TDM

TDM is a desktop download manager written in Rust. It has three main parts:

- **Segments.** A large file is split into byte ranges. The ranges download in parallel. Each segment is kept apart, so one failed connection does not stop the others.
- **Resume.** An interrupted download continues from where it stopped. Before it resumes, TDM checks the server's `ETag` and `Last-Modified` headers. A file that changed on the server is not joined to an old copy.
- **Capture.** A browser extension sends downloads and page media to the app. You do not copy links by hand.

TDM has no published speed comparison yet, so this README makes no speed claims. The `tdm-bench` crate only times a ranged-download loop against the local lab server. It does not run the TDM engine or another downloader.

| Features tracked | Works | Partial | Unverified or not built |
| --- | --- | --- | --- |
| 32 | 15 | 11 | 6 |

The counts come from the status table in [What it does](#what-it-does).

<p align="center">
  <img src="docs/TDM.png" alt="Turbo Download Manager's main window" width="880">
</p>

## How to download

There is no Windows installer yet. Run TDM from source for now. See [For developers](#for-developers).

| Platform | What to use | Notes |
| --- | --- | --- |
| Windows | Build from source. Run `target\release\tdm.exe`. | The installer and code signing are not built. |
| Linux | Build from source. Run `target/release/tdm`. | Linux packages (deb, AppImage, rpm) are set up in `Cargo.toml`, but they are not verified. |
| Browser | Load the extension folders from the build. Chrome, Edge and Firefox. | Browser capture is not verified yet. Test it in your own browser first. |

## Browser buttons

- **Automatic capture.** A normal file download in the browser is handed to TDM. This happens when capture is on and the file is above the minimum size.
- **Download with TDM.** Right-click a link and choose this.
- **Download media with TDM.** Right-click an image, video or audio item and choose this.
- **Download selected links with TDM.** Select text that contains links, right-click, and choose this.
- **Popup.** Lists the media found on the page. Each item has a Download button.

Media detection is off by default. To use it:

1. Turn on media detection in the popup or in Options. Chrome then asks for "all sites" access.
2. List each site you want under "Media sites".
3. Reload the page.

On a listed site, TDM records media manifests and media files (`.m3u8`, `.mpd`, mp4, webm) for each tab. It ignores segment files (`.ts`, `.m4s`) on purpose. The hover button appears over a playing video that has a direct URL or a recorded stream, and only on listed sites.

The popup lists the `<video>` and `<audio>` sources on the current page. It does not need the extra permission, because it uses the active tab.

Not detected: encrypted (DRM) streams, players that use only `blob:` sources with no manifest, and YouTube-style streams where video and audio are separate.

## What it does

| Feature | Status | Notes |
| --- | --- | --- |
| Segmented HTTP and HTTPS downloads | Works | Tested with the lab server only. No live internet run yet. |
| Pause and resume with ETag and Last-Modified checks | Works | If the server sends no validators, resume starts again from zero. |
| Retries with backoff, no retry on 404 | Works | Permanent errors such as 404 are not retried. |
| Per-host connection learning | Works | Remembers how many connections each host accepts. |
| Pause when the disk is full | Works | The download pauses and gives a reason. |
| Mirror lists and numbered ranges like `[001-120]` | Works | Batch links can use mirrors and numbered ranges. |
| Server hash check (Digest, Content-MD5, x-goog-hash) | Works | Checks the downloaded bytes against the hash the server sends. |
| FTP | Partial | Tested against a local fake server only. A real server login failed with the "anonymous" default. For a login, use `ftp://user:password@host/path`. |
| FTPS (explicit and implicit TLS) | Partial | Code exists. Not run against a real FTPS server. |
| Browser capture from Chrome, Edge, Firefox | Unverified | Needs a real browser run. |
| Hover buttons on images, video and audio | Partial | Skips `blob:` and `data:` sources. |
| Capture prompt with rename and folder choice | Works | |
| HLS and DASH streams | Works | Joining DASH audio and video needs ffmpeg. |
| Warning when the file type does not match the name | Works | |
| Scanner hook (Defender, ClamAV) | Partial | Not run on a real machine. |
| Duplicate name dialog | Works | |
| Duplicate URL detection | Partial | Checks URL, name and size. |
| Queues, schedules, time-of-day bandwidth cap | Works | |
| Site grabber (find links on a page) | Works | |
| Proxies: system, manual, PAC script, SOCKS | Works | |
| Keychain storage for passwords | Partial | The real OS keychain is not verified. |
| Local token-protected IPC | Works | |
| Archive extraction (zip, tar, gz, xz) | Partial | On by default. The result is shown only in the log. No 7z or RAR. |
| Minimum file size for browser capture | Partial | Wired in. End to end not verified. |
| RSS feeds | Partial | Text "contains" rules only. The seen-list is saved before the download starts. |
| Torrents (magnet and .torrent) | Unverified | A live swarm has never been tested. The worker runs in its own process. |
| Command-line mode (`tdm-cli add`, `tdm-cli rss`) | Partial | No tests yet. |
| MCP server (`tdm-mcp`, feature `mcp`) | Partial | One tool: download a URL. Off by default. |
| Debrid services | Not built | Waiting on an owner decision. |
| Quality picker for streaming sites | Not built | A yt-dlp button shows in the address bar only after a yt-dlp path is set in Preferences. It has no quality list. |
| Windows installer and signing | Not built | Needs a certificate. |
| Linux packages | Unverified | Tested on a Windows-only machine. |

## Known limits

These are known defects. They are not fixed yet.

1. Files saved from a URL with no extension (for example `/image?id=5`) are saved with no extension. Windows shows them as "File". TDM does not use the Content-Type to name the file, and it does not read Content-Disposition.
2. TDM does not check whether a link is a web page before it queues it. A "Download with TDM" link on a Google results page may save the HTML page as a file. This was not reproduced in this review.
3. Video detection on a normal website needs three steps: grant the permission, list each site under "Media sites", and reload. YouTube-style and encrypted (DRM) video are not detected correctly.
4. Torrent helper windows: the fix for the console window is in the code. It is not verified on a machine yet.
5. Import clears queue membership.
6. Some disk work runs on the UI thread. This includes large imports and quarantine copies. Large files may stall the window.
7. The right-click menu shows nothing on failure. The popup shows only a short label on the button (Failed, App offline or Declined) for a moment, with no error text.
8. The "Download" labels differ between the menu, the hover button and the popup.

## How to verify

These checks have not been run by hand yet. Run them on a real machine:

- [ ] Torrents: add a `.torrent` file and a magnet link. Check that no console window appears. Pause, resume, remove and restart.
- [ ] Browser capture in Chrome, Edge and Firefox: right-click a link, use the hover button, the capture prompt, and a Google Images page.
- [ ] FTP with anonymous login and with a password. FTPS on ports 21 and 990.
- [ ] A URL with no extension, from a server that sends Content-Disposition.
- [ ] RSS: add a feed with a rule, confirm auto-add works, restart, and confirm there are no duplicates.
- [ ] Export, then import, and check queue membership.
- [ ] Scanner, DASH join with and without ffmpeg, zip extraction, PAC proxy, and keychain entries.
- [ ] Import a large file and watch for UI stalls.

## For developers

<details>
<summary><b>Build, test and run</b></summary>

You need a recent stable Rust toolchain. The project uses edition 2024.

```sh
git clone https://github.com/BazilSuhail/Turbo-Download-Manager.git
cd Turbo-Download-Manager
cargo build
cargo build --release
```

Build the browser extension:

```sh
node extension/build.mjs
```

Run the tests:

```sh
cargo test --workspace
cargo test --workspace --features mcp
node --test extension/
```

Run the app:

```sh
cargo run --bin tdm
```

On Linux, install the windowing and GPU headers that iced needs:

```sh
sudo apt-get install -y pkg-config libxkbcommon-dev libwayland-dev libx11-dev \
  libxcursor-dev libxi-dev libxrandr-dev libfontconfig1-dev
```

Register the browser host by hand, if needed:

```sh
tdm --register-host                 # bundled extension ids
tdm --register-host <chrome-id>...  # extra unpacked or development extension ids
tdm --unregister-host               # remove every manifest and registry key
```

</details>

<details>
<summary><b>Crates and folders</b></summary>

| Path | What it is |
| --- | --- |
| `src/` (package `tdm`) | The main app: the engine, planner, proxy, scheduler, browser host, IPC and the iced user interface (`src/ui/`). |
| `crates/tdm-store` | Durable JSON state files: atomic replace, lifecycle backups and corrupt-file recovery. |
| `crates/tdm-labserver` | Deterministic HTTP lab server with fault injection, used by the engine tests. |
| `crates/tdm-bench` | Black-box ranged-download benchmark run against `tdm-labserver`. |
| `tests/` | Integration tests: local HTTP, FTP and media servers. |
| `extension/` | The browser extension. The build creates the Chrome and Firefox folders. |
| `packaging/` | Installer templates, icons and the packaging guide. |
| `docs/` | Feature guide and release notes. |

Read [CONTRIBUTING.md](CONTRIBUTING.md) before you open a pull request.

</details>

## Roadmap

Open items, planned work and feature gaps are kept in [TODO.md](../TODO.md). The list is in priority order.

## Licence and credits

Turbo Download Manager is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

Third-party components are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md). Turbo is published by Entity Studios.

### Clean-room statement

TDM was written without copying text, code, images or logos from MDM, a GPL-3.0 download manager. Ideas from other download managers are described here in our own words.
