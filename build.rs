//! Embeds the TDM logo in `tdm.exe`, so Explorer and the taskbar show it.
//!
//! The GNU resource compiler (`windres`) splits its arguments on spaces, and
//! `winres` passes it `-I<project folder>`. A project under
//! `C:\Users\Some Name\...` therefore breaks the build. On the GNU toolchain
//! this script runs `windres` itself, from an 8.3 short-path folder with no
//! spaces, on a resource file that only contains the icon. The MSVC toolchain
//! has no such problem, so it keeps using `winres`.
//!
//! If the icon cannot be embedded, the build still succeeds and prints a
//! warning, so a missing icon never stops a build.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=packaging/icons/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    build_extension();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if let Err(reason) = embed_icon() {
        println!("cargo:warning=TDM icon not embedded: {reason}");
    }
}

/// Build the browser extension folders (`extension/dist/chrome` and
/// `extension/dist/firefox`) by running `extension/build.mjs` with Node.
///
/// Only the extension's source files are watched. The script writes into
/// `extension/dist`, and watching that folder would make every build rerun
/// the script. If Node is missing the build still succeeds, with a warning.
fn build_extension() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let ext = manifest.join("extension");
    if !ext.join("build.mjs").is_file() {
        return;
    }
    const SOURCES: [&str; 17] = [
        "build.mjs",
        "manifest.json",
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
        "tools/make-update-xml.mjs",
        "tools/pack-crx.mjs",
        "tools/tools.test.mjs",
        "README.md",
    ];
    for source in SOURCES {
        println!("cargo:rerun-if-changed=extension/{source}");
    }
    match std::process::Command::new("node").arg(ext.join("build.mjs")).current_dir(&ext).output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => println!(
            "cargo:warning=extension build failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => println!(
            "cargo:warning=extension not built ({e}). Install Node.js, then run: node extension/build.mjs"
        ),
    }
}

fn embed_icon() -> Result<(), String> {
    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").map_err(|e| format!("no manifest dir: {e}"))?,
    );
    let icon = manifest.join("packaging").join("icons").join("icon.ico");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        gnu_resource(&icon)
    } else {
        msvc_resource(&icon)
    }
}

/// GNU toolchain: run `windres` directly, from a folder without spaces.
fn gnu_resource(icon: &Path) -> Result<(), String> {
    let work = short_dir(&std::env::temp_dir().join("tdm-icon-build"))?;
    let icon = short_path(icon).unwrap_or_else(|| icon.to_path_buf());
    let icon_text = icon.to_string_lossy().replace('\\', "/");
    if icon_text.contains(' ') {
        return Err(format!("the icon path has a space ({icon_text}); enable 8.3 short names on this drive"));
    }

    let rc = work.join("icon.rc");
    std::fs::write(&rc, format!("1 ICON \"{icon_text}\"\n")).map_err(|e| format!("write rc: {e}"))?;
    let obj = work.join("icon.o");

    let status = std::process::Command::new("windres")
        .current_dir(&work)
        .arg("icon.rc")
        .arg("icon.o")
        .status()
        .map_err(|e| format!("could not run windres: {e}"))?;
    if !status.success() {
        return Err("windres failed on the icon resource".to_owned());
    }

    // Link the compiled object straight into the executable.
    println!("cargo:rustc-link-arg-bins={}", obj.to_string_lossy());
    Ok(())
}

/// MSVC toolchain: `winres` drives `rc.exe`, which handles spaces correctly.
fn msvc_resource(icon: &Path) -> Result<(), String> {
    let mut res = winres::WindowsResource::new();
    res.set_icon(icon.to_str().ok_or("icon path is not valid UTF-8")?);
    res.compile().map_err(|e| e.to_string())
}

/// Create `dir` and return its 8.3 short form, which has no spaces.
fn short_dir(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    short_path(dir).ok_or_else(|| format!("no short path for {} (8.3 names are off)", dir.display()))
}

/// The 8.3 short form of an existing path, or `None` if Windows has none.
#[cfg(windows)]
fn short_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    unsafe extern "system" {
        fn GetShortPathNameW(long_path: *const u16, short_path: *mut u16, buffer_len: u32) -> u32;
    }

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut buffer = vec![0u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `buffer` has the length we pass.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) };
    if written == 0 || written as usize >= buffer.len() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..written as usize])))
}

#[cfg(not(windows))]
fn short_path(_path: &Path) -> Option<PathBuf> {
    None
}
