//! Virus scan of finished downloads, using a command-line scanner.
//!
//! Windows Defender (`MpCmdRun.exe`) and ClamAV (`clamscan`) are supported.
//! A scan only reports; it never deletes or moves a file.

use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    Threat,
    Error(String),
}

/// The scanner's program name, lowercased, without its folder or `.exe`.
fn kind(scanner: &str) -> String {
    Path::new(scanner)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Arguments that scan one file, or `None` for a scanner we do not know.
pub fn args_for(scanner: &str, file: &Path) -> Option<Vec<String>> {
    let file = file.display().to_string();
    match kind(scanner).as_str() {
        "mpcmdrun" => Some(["-Scan", "-ScanType", "3", "-File"].map(str::to_owned).into_iter().chain([file]).collect()),
        "clamscan" => Some(vec!["--no-summary".to_owned(), file]),
        _ => None,
    }
}

/// What the scanner's exit code means. Defender: 0 clean, 2 threat.
/// ClamAV: 0 clean, 1 infected. Anything else is a failure to scan.
pub fn verdict(scanner: &str, code: Option<i32>) -> Verdict {
    match (kind(scanner).as_str(), code) {
        ("mpcmdrun", Some(0)) | ("clamscan", Some(0)) => Verdict::Clean,
        ("mpcmdrun", Some(2)) | ("clamscan", Some(1)) => Verdict::Threat,
        (_, code) => Verdict::Error(format!("scanner exited with {code:?}")),
    }
}

/// Scan `file` with `scanner`. Blocking: run it off the UI thread.
pub fn scan(scanner: &str, file: &Path) -> Verdict {
    let Some(args) = args_for(scanner, file) else {
        return Verdict::Error("unknown scanner".to_owned());
    };
    let mut command = Command::new(scanner);
    command.args(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // Without this a console window flashes up for every scan.
        command.creation_flags(0x0800_0000);
    }
    match command.status() {
        Ok(status) => verdict(scanner, status.code()),
        Err(e) => Verdict::Error(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defender_and_clamav_exit_codes_are_read_correctly() {
        let defender = r"C:\Program Files\Windows Defender\MpCmdRun.exe";
        assert_eq!(verdict(defender, Some(0)), Verdict::Clean);
        assert_eq!(verdict(defender, Some(2)), Verdict::Threat);
        assert!(matches!(verdict(defender, Some(1)), Verdict::Error(_)));
        assert_eq!(verdict("/usr/bin/clamscan", Some(0)), Verdict::Clean);
        assert_eq!(verdict("/usr/bin/clamscan", Some(1)), Verdict::Threat);
        assert!(matches!(verdict("/usr/bin/clamscan", None), Verdict::Error(_)));
    }

    #[test]
    fn arguments_are_a_list_so_file_names_cannot_inject_flags() {
        let file = Path::new("a b; rm -rf.zip");
        let args = args_for("clamscan", file).expect("known scanner");
        assert_eq!(args, vec!["--no-summary".to_owned(), "a b; rm -rf.zip".to_owned()]);
        assert!(args_for("notepad", file).is_none());
    }
}
