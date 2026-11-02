# Turbo Download Manager: building and packaging

TDM is a single binary, `tdm`. Licensed MIT OR Apache-2.0
(`LICENSE-MIT`, `LICENSE-APACHE`).

## Build and test

```sh
cargo build --release
cargo test
```

Linux needs the usual windowing/GPU headers for iced:

```sh
sudo apt-get install -y pkg-config libxkbcommon-dev libwayland-dev libx11-dev \
  libxcursor-dev libxi-dev libxrandr-dev libfontconfig1-dev
```

## Installers

Installers are made by [cargo-packager](https://github.com/crabnebula-dev/cargo-packager),
configured under `[package.metadata.packager]` in `Cargo.toml`. It downloads
NSIS, WiX and linuxdeploy itself on first use.

```sh
cargo install cargo-packager --locked
```

| OS      | Command                                               | Output (in `target/release/`) |
|---------|-------------------------------------------------------|-------------------------------|
| Windows | `cargo packager --release --formats nsis,wix`         | `*-setup.exe`, `*.msi`        |
| Linux   | `cargo packager --release --formats deb,appimage`     | `*.deb`, `*.AppImage`         |
| Linux   | `cargo install cargo-generate-rpm && cargo generate-rpm` (after `cargo build --release`) | `target/generate-rpm/*.rpm` |

`.github/workflows/release.yml` runs all of these on a `v*` tag and attaches
the results to a draft GitHub release.

### Windows (NSIS)

`windows/installer.nsi` is our own template (derived from cargo-packager's):
dark orange welcome/finish pages (`windows/sidebar.bmp`,
`windows/header.bmp`), the license, the install-directory page, and an options
page for a desktop shortcut, a Start menu entry and launch at login. It
installs per user (no UAC prompt), runs `tdm --register-host` after
installing and `tdm --unregister-host` before uninstalling.

The MSI (WiX) uses cargo-packager's stock template with our banner and dialog
art, plus `windows/host-registration.wxs`: deferred custom actions run
`tdm --register-host` after install and `tdm --unregister-host` on
uninstall, as the installing user. A per-machine MSI therefore only cleans up
that user's manifests; other accounts keep theirs until they run TDM again
or remove it themselves.

### Linux

There is nothing to run as root at install time: the app writes the
per-user native-messaging manifests itself on first start (for each browser
whose profile directory exists). `linux/tdm.desktop` and
`linux/app.tdm.metainfo.xml` (AppStream) ship in the packages.

### Art

`icons/`, `windows/*.bmp` and the extension icons are generated from `assets/tdm-logo.png` by `python packaging/make_logo.py` (needs `pip install pillow`)
(stdlib only, no third-party pixels). Rerun it after changing the design.

## Browser registration by hand

```sh
tdm --register-host                 # bundled extension ids
tdm --register-host <chrome-id>...  # additional unpacked/dev extension ids
tdm --unregister-host               # remove every manifest and registry key
```

Manifests: Windows keeps them in `%APPDATA%\tdm` and points
`HKCU\Software\{Google\Chrome, Microsoft\Edge, Chromium,
BraveSoftware\Brave-Browser, Vivaldi, Mozilla}\NativeMessagingHosts\com.tdm.host`
at them (Opera reads Chrome's key). Linux writes into
`~/.config/{google-chrome, chromium, microsoft-edge, BraveSoftware/Brave-Browser, vivaldi}/NativeMessagingHosts`
and `~/.mozilla/native-messaging-hosts`.
