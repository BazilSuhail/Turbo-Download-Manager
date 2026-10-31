# TDM Download Capture — browser extension

The MV3 browser companion for **Turbo Download Manager**. It:

- watches the browser's own downloads, asks the TDM app whether it wants
  each one, and cancels the browser's copy when it does;
- adds right-click items: **Download with TDM** (links), **Download media
  with TDM** (images, video, audio) and **Download selected links with
  TDM**;
- has a toolbar popup that shows the connection, the auto-capture switch and
  the video/audio found on the current tab, each with a **Download** button;
- optionally shows a small **⬇ TDM** button over a hovered video and picks
  up streamed media (HLS `.m3u8`, DASH `.mpd`, MP4, WebM…) as pages load.

Everything it sends and receives is defined by `../src/native_host.rs`.
Licensed MIT OR Apache-2.0, like the rest of TDM. All code and artwork is
original (the icons are drawn by `tools/make-icons.mjs`).

## 1. Install the app first

The TDM app/installer registers the native host (`com.tdm.host`) for
you. To redo it by hand, run `tdm --register-host` with no arguments: that
registers the defaults for Chrome/Edge/Chromium (extension id
`glkodjlchifmdbokmofigopbgdbabmge`, pinned by the `key` in `manifest.json`)
and Firefox (`tdm@tdm.app`). You never need to copy an id: it is the same
on every machine and from every folder.

## 2. Load the extension

### Chrome, Edge, Brave, Opera, Vivaldi

1. Open the extensions page: `chrome://extensions`, `edge://extensions`,
   `brave://extensions`, `opera://extensions` or `vivaldi://extensions`.
2. Turn on **Developer mode**.
3. **Load unpacked** → pick this `extension` folder (or `dist/chrome` after a
   build). The id shown must be `glkodjlchifmdbokmofigopbgdbabmge`.
4. Pin the TDM icon to the toolbar and click it: a green dot means the app
   answered.

The service-worker console is under **Details → Inspect views: service
worker**; connection errors show up there.

### Firefox (128 or newer)

Firefox needs its own manifest (an event page instead of a service worker and
a `gecko` id), so build first:

```sh
node extension/build.mjs
```

That writes `extension/dist/chrome/` and `extension/dist/firefox/`, plus
`tdm-<browser>-<version>.zip` for each when `zip` or bsdtar (Windows'
built-in `tar.exe`) is available. Then:

- **Try it:** `about:debugging#/runtime/this-firefox` → **Load Temporary
  Add-on…** → pick `dist/firefox/manifest.json`. It stays until Firefox
  restarts.
- **Keep it:** release Firefox only installs signed add-ons; submit the
  Firefox zip to addons.mozilla.org (it can be signed as unlisted). Developer
  Edition/Nightly can install it unsigned with `xpinstall.signatures.required`
  set to `false`.

Why a build step instead of one manifest: Chromium before 121 rejects
`background.scripts` in MV3 and Firefox does not run MV3 service workers, so no
single `background` entry loads in both. The source folder stays loadable as is
in Chromium.

## 3. What it captures automatically

Nothing, until the app says what it wants. The app's `ready` reply carries
`captureExtensions`, `exceptionHosts` and `enabled` (capture switched off in
the app empties the list); a download is taken over only when

- automatic capture is on (popup or options page), and
- the URL is `http:`/`https:` — never `blob:`, `data:`, `file:` or an
  extension URL, and
- its host is not in `exceptionHosts` (suffix match, so `example.com` also
  covers `cdn.example.com`), and
- its file extension is in `captureExtensions`.

An empty `captureExtensions` list captures nothing.

Chromium holds a download in `onDeterminingFilename` until the app answers.
Firefox has no such event, so there the download is paused in `onCreated`,
and cancelled and erased once the app accepts it (resumed if it declines).

The right-click items, the popup's **Download** buttons and the video button
are explicit requests: they ignore the lists and the on/off switch and always
hand the URL over.

## 4. Media detection (optional)

Off until you press **Grant media detection** (options page) or **Enable
media detection** (popup). That asks for `webRequest` plus access to all
sites, and then:

- a content script is registered for all pages (frames included). Over a
  hovered or focused `<video>`/`<audio>` it shows a **⬇ TDM** button in a
  closed Shadow DOM, styled for light and dark. **×** hides it for that
  element on that page. Keyboard: focus the player, **Tab** moves into the
  button, **Esc** goes back. It never appears for `blob:`/`data:` players
  unless a real stream URL for that tab has been seen on the network, and it
  is not shown in fullscreen.
- responses whose URL or `Content-Type` look like a whole media file or a
  stream manifest are remembered per tab (stream segments such as `.ts` and
  `.m4s` are ignored) and listed in the popup.

The download name comes from the page title plus the URL's media extension
(sanitised: no path characters, controls or bidi/zero-width characters,
≤120 characters). The app sanitises it again. **Turn off** on the options page
removes the permissions and unregisters the script.

Without the grant the popup still lists the `<video>`/`<audio>` sources of the
current page. Opening the popup grants `activeTab`, which is enough to read
the page once.

## 5. Permissions

| Permission | Used for |
|---|---|
| `downloads` | `onCreated`, `onDeterminingFilename` (Chromium), `pause`, `resume`, `cancel`, `erase` — the whole interception path |
| `nativeMessaging` | `runtime.connectNative` to the app |
| `storage` | `storage.session` for connection state, capture lists, the de-duplication list and per-tab media; `storage.local` for the on/off switch |
| `contextMenus` | the right-click items |
| `activeTab` + `scripting` | read the current page's media when the popup opens, and the links in a selection after the menu click; register the media content script once granted. No install-time warning and no access to any page you have not acted on |
| `cookies` + `<all_urls>` | **optional, off by default.** Only to fill the protocol's `cookie` field so a handed-over download keeps your session |
| `webRequest` + `<all_urls>` | **optional, off by default.** Media detection (section 4). Read-only observation, never blocking |

No content scripts and no host access unless you grant them.

## 6. Development

```sh
node --test extension/            # filter, media detection, API shim
node extension/build.mjs          # dist/chrome, dist/firefox (+ zips)
node extension/tools/make-icons.mjs   # regenerate icons/*.png
```

Manual checks: `selftest.md`.

## 7. Known limitations

- The capture lists and the app's on/off switch are read once per connection
  (`hello`/`ready`); changes made in the app apply after **Reconnect** or a
  browser restart.

- Firefox: the download has already started when `onCreated` fires, so a very
  small file can finish before the app answers and end up in both places.
- HLS/DASH manifests are handed over as URLs; whether they are fetched as a
  video depends on the app's stream support. Signed or DRM streams will not
  play outside the page.
- Tabs that were already open when media detection was granted need a reload
  before the video button appears.
- Streams that are not plain HTTP files or manifests (WebSocket, WebRTC,
  per-segment URLs with no manifest) are not detected.
