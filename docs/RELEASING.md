# Releasing TDM

This guide covers how to publish a new version. Every release is built and published by GitHub Actions. You only need to set the version, commit, and push a tag.

## Version numbers

TDM uses semantic versions: `MAJOR.MINOR.PATCH`.

| Change | Example | When |
|---|---|---|
| PATCH | 1.0.0 → 1.0.1 | Bug fixes only |
| MINOR | 1.0.1 → 1.1.0 | New features, nothing breaks |
| MAJOR | 1.1.0 → 2.0.0 | Breaking changes |

A pre-release has a hyphen, for example `1.1.0-beta.1`. Pre-releases are published, but not marked as the latest release.

## What a release does

1. The **check-version** job confirms that the tag matches the version in `Cargo.toml`, `extension/manifest.json`, and `packaging/linux/app.tdm.metainfo.xml`. If any of them differ, the release stops before anything is built.
2. The **build** job builds the installers on three systems:
   - Windows: `*-setup.exe` (NSIS) and `*.msi` (WiX)
   - Linux: `*.deb`, `*.AppImage`, and `*.rpm`
3. The **publish** job collects the installers, writes `SHA256SUMS.txt`, creates the release notes, and publishes a GitHub release titled `Turbo Download Manager vX.Y.Z`. The notes list the commits since the previous tag.

Each platform is built on its own runner, so the release contains one installer per platform.

## Step by step

### 1. Make sure `main` is ready

```sh
git checkout main
git pull
```

Check that the tests pass locally (`cargo test`) and that CI is green on `main`.

### 2. Choose the new version

Decide the number using the table above. For example, the next feature release after 1.0.0 is `1.1.0`.

### 3. Set the version everywhere

```sh
node scripts/bump-version.mjs 1.1.0
```

This updates `Cargo.toml`, `Cargo.lock`, `extension/manifest.json`, and adds a `<release>` entry to the AppStream file.

Then refresh the build outputs once:

```sh
cargo build --release
```

### 4. Write what changed

Edit `packaging/linux/app.tdm.metainfo.xml` if you want to add a short description under the new `<release>` entry. The GitHub release notes are generated from the commit messages, so write clear commit messages.

### 5. Commit the release

```sh
git add -A
git commit -m "chore: release v1.1.0"
```

### 6. Tag and push

```sh
git tag v1.1.0
git push origin main v1.1.0
```

Pushing the tag starts the release workflow. The tag must start with `v` and match the version exactly.

### 7. Watch the build

Open the **Actions** tab on GitHub and find the **Release** run for the tag. The run takes a while because it builds three platforms.

If the **check-version** job fails, one of the three version files does not match the tag. Run `node scripts/bump-version.mjs <version>` again with the right number, commit, then move the tag (see below).

### 8. Check the release

When the run finishes, open the **Releases** page. Check that:
- The title is `Turbo Download Manager vX.Y.Z`.
- All installers are attached, and `SHA256SUMS.txt` is included.
- The release notes list the expected changes.

## Fixing a failed or wrong release

If a release is wrong and has not been used yet:

1. On GitHub, open the release and choose **Delete**.
2. Delete the tag locally and on GitHub:
   ```sh
   git tag -d v1.1.0
   git push origin :refs/tags/v1.1.0
   ```
3. Fix the problem, commit, and tag again.

Do not reuse a version number that users have already downloaded. Use the next number instead, for example `1.1.1`.

## Pre-releases

```sh
node scripts/bump-version.mjs 1.2.0-beta.1
git commit -am "chore: release v1.2.0-beta.1"
git tag v1.2.0-beta.1
git push origin main v1.2.0-beta.1
```

The release is published with the **Pre-release** label.

## Where versions are written

| File | Field |
|---|---|
| `Cargo.toml` | `version` under `[package]` |
| `Cargo.lock` | the `tdm` entry (updated by `cargo build`) |
| `extension/manifest.json` | `version` |
| `packaging/linux/app.tdm.metainfo.xml` | `<release version="...">` |

The browser protocol version (`EXT_VERSION` in `extension/background.js`) is separate and is not changed on release.

## Platform coverage

The release currently builds for:
- Windows (64-bit)
- Linux (64-bit, Ubuntu 22.04)


## Requirements for the build machine

GitHub Actions installs everything automatically. For a local build you need:
- Rust (stable)
- Node.js 20 or newer (the browser extension is built during `cargo build`)
- On Linux, the GUI libraries listed in the README
