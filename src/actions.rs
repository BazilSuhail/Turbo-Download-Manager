//! Shell-style actions on local paths: open a folder, reveal a file in its folder.
//!
//! Every entry point validates the path first: it must be absolute, must not
//! contain a line break or NUL character, and must exist. Nothing here goes
//! through a shell. On Windows, `explorer.exe` receives the path as a single
//! argument.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Command;

/// Returns the folder that contains `file`, or `None` when there is no parent
/// (a root, or an empty relative path).
pub fn folder_of(file: &Path) -> Option<&Path> {
    file.parent().filter(|p| !p.as_os_str().is_empty())
}

/// Checks the rules shared by every action: absolute, no line break or NUL,
/// and the target must exist.
fn validate(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be absolute",
        ));
    }
    let text = path.as_os_str().to_string_lossy();
    if text.contains('\n') || text.contains('\r') || text.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a line break or NUL character",
        ));
    }
    if !path.exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "path does not exist"));
    }
    Ok(())
}

/// Opens `path` (a folder) with the system handler.
pub fn open_folder(path: &Path) -> io::Result<()> {
    validate(path)?;
    if !path.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a folder"));
    }
    open::that_detached(path)
}

/// The Windows Explorer binary, by absolute path, so a same-named file earlier
/// on the search path is never run. Falls back to the usual install folder when
/// `SystemRoot` is not set.
#[cfg(windows)]
fn explorer_exe() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("C:\\Windows"));
    root.join("System32").join("explorer.exe")
}

/// Shows `file` selected in its containing folder.
///
/// On Windows this runs `explorer.exe /select,<path>` with the path as one
/// argument. On other platforms it opens the parent folder.
pub fn reveal_in_folder(file: &Path) -> io::Result<()> {
    validate(file)?;
    #[cfg(windows)]
    {
        let mut arg = OsString::from("/select,");
        arg.push(file.as_os_str());
        // spawn, not status: explorer.exe often exits non-zero even on success.
        Command::new(explorer_exe()).arg(arg).spawn().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let parent = folder_of(file).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path has no parent folder")
        })?;
        open::that_detached(parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn absolute_base() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn folder_of_returns_parent() {
        assert_eq!(folder_of(Path::new("a/b.txt")), Some(Path::new("a")));
        assert_eq!(folder_of(Path::new("b.txt")), None);
    }

    #[test]
    fn relative_path_is_refused() {
        let err = open_folder(Path::new("relative/dir")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let err = reveal_in_folder(Path::new("relative/file.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn newline_is_refused() {
        let p = absolute_base().join("tdm\nbad.txt");
        let err = reveal_in_folder(&p).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let p = absolute_base().join("tdm\rbad.txt");
        assert_eq!(validate(&p).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn open_folder_refuses_a_file() {
        // The running test binary is an existing absolute file, not a folder.
        let file = std::env::current_exe().expect("test binary path");
        assert_eq!(open_folder(&file).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn nul_is_refused() {
        let p = absolute_base().join("tdm\0bad.txt");
        assert_eq!(validate(&p).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn missing_path_is_not_found() {
        let p = absolute_base().join("tdm_actions_missing_9f3c2a7e_does_not_exist");
        assert_eq!(validate(&p).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(
            open_folder(&p).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
