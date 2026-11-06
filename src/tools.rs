//! Helper programs (yt-dlp, ffmpeg and similar) run as supervised child
//! processes. No shell is involved. Stdin is closed, output is capped, and the
//! child is killed when the time limit passes or when the future is dropped.
//! A helper binary must match a SHA-256 pin before it runs.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Most output kept from each of stdout and stderr. Anything beyond is read and dropped.
pub const OUTPUT_CAP: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool not found at {0}")]
    Missing(String),
    #[error("tool does not match its pinned SHA-256")]
    HashMismatch,
    #[error("could not start the tool: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("tool pipe error: {0}")]
    Io(#[source] std::io::Error),
    #[error("tool timed out after {0:?}")]
    Timeout(Duration),
    #[error("could not put the tool in a job object: {0}")]
    Job(#[source] std::io::Error),
}

/// Keeps a helper and any processes it starts inside one job object, so they
/// die with the job. On Windows the job kills its processes when its handle
/// closes. On other platforms this does nothing.
#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::io;

    use tokio::process::Child;

    use super::ToolError;

    type Handle = *mut c_void;

    /// `JobObjectExtendedLimitInformation` from the `JOBOBJECTINFOCLASS` enum.
    const EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` from winnt.h.
    const LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    /// Mirrors `JOBOBJECT_BASIC_LIMIT_INFORMATION` (winnt.h, 64 bytes on x64).
    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)] // the kernel reads these fields; Rust never does

    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    /// Mirrors `IO_COUNTERS` (winnt.h, 48 bytes).
    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    /// Mirrors `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` (winnt.h, 144 bytes on x64).
    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)]
    struct ExtendedLimitInformation {
        basic: BasicLimitInformation,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, info_len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(object: Handle) -> i32;
    }

    /// An owned job handle. The handle is stored as an integer so the future
    /// that holds it stays `Send`. Dropping it closes the handle, which kills
    /// every process still in the job.
    pub struct Job(usize);

    impl Job {
        /// Creates a job that kills its processes when the handle closes.
        pub fn create() -> Result<Self, ToolError> {
            // SAFETY: null attributes and a null name are both allowed. The call only returns a handle.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(ToolError::Job(io::Error::last_os_error()));
            }
            // From here on, `job` owns the handle and closes it on every error path.
            let job = Job(handle as usize);
            let info = ExtendedLimitInformation {
                basic: BasicLimitInformation { limit_flags: LIMIT_KILL_ON_JOB_CLOSE, ..Default::default() },
                ..Default::default()
            };
            // SAFETY: `info` is a #[repr(C)] value laid out like JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            // and the length is its exact size. `handle` is a live job handle owned by `job`.
            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&info as *const ExtendedLimitInformation).cast(),
                    std::mem::size_of::<ExtendedLimitInformation>() as u32,
                )
            };
            if ok == 0 {
                return Err(ToolError::Job(io::Error::last_os_error()));
            }
            Ok(job)
        }

        /// Puts the running child into the job. A child that has already exited has nothing to contain.
        pub fn assign(&self, child: &Child) -> Result<(), ToolError> {
            let Some(process) = child.raw_handle() else { return Ok(()) };
            // SAFETY: `self.0` is a live job handle owned by `self`. `process` is the child's process
            // handle, which tokio keeps open while the child has not been reaped.
            let ok = unsafe { AssignProcessToJobObject(self.0 as Handle, process) };
            if ok == 0 {
                return Err(ToolError::Job(io::Error::last_os_error()));
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateJobObjectW and is closed only here, once.
            unsafe {
                CloseHandle(self.0 as Handle);
            }
        }
    }
}

#[cfg(not(windows))]
mod job {
    use tokio::process::Child;

    use super::ToolError;

    pub struct Job;

    impl Job {
        pub fn create() -> Result<Self, ToolError> {
            Ok(Self)
        }

        pub fn assign(&self, _child: &Child) -> Result<(), ToolError> {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Checks that `path` is a regular file whose SHA-256 equals `pin_hex`
/// (64 hex digits, any case). Call this before every run of a downloaded helper.
pub fn verify_pin(path: &Path, pin_hex: &str) -> Result<(), ToolError> {
    if !path.is_file() {
        return Err(ToolError::Missing(path.display().to_string()));
    }
    let mut file = std::fs::File::open(path).map_err(ToolError::Io)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(ToolError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if hex.eq_ignore_ascii_case(pin_hex.trim()) {
        Ok(())
    } else {
        Err(ToolError::HashMismatch)
    }
}

/// Reads up to `OUTPUT_CAP` bytes and drains the rest. Draining keeps the child from
/// blocking on a full pipe.
async fn read_capped<R: AsyncRead + Unpin>(pipe: Option<R>) -> std::io::Result<String> {
    let Some(mut pipe) = pipe else { return Ok(String::new()) };
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = pipe.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if kept.len() < OUTPUT_CAP {
            let room = OUTPUT_CAP - kept.len();
            kept.extend_from_slice(&chunk[..n.min(room)]);
        }
    }
    Ok(String::from_utf8_lossy(&kept).into_owned())
}

/// Runs `program` with `args` and waits for it, up to `limit`.
pub async fn run(program: &Path, args: &[String], limit: Duration) -> Result<Output, ToolError> {
    if !program.is_file() {
        return Err(ToolError::Missing(program.display().to_string()));
    }
    // The job is created before the spawn, so a failure here never leaves an
    // unsandboxed helper running.
    //
    // Known gap (Windows): the child is spawned running and only then assigned
    // to the job. A helper that starts a grandchild in that short window can
    // leave the grandchild outside the job, where closing the job will not end
    // it. Closing the gap needs a suspended spawn (CREATE_SUSPENDED, then
    // resume after assignment). That was not done here: it changes the spawn
    // path, and it cannot be tested without a build.
    let job = job::Job::create()?;
    let mut command = Command::new(program);
    command.args(args);
    // Helper programs are console tools; keep their windows hidden.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(ToolError::Spawn)?;
    if let Err(e) = job.assign(&child) {
        let _ = child.start_kill();
        return Err(e);
    }
    // `job` lives until this function returns. Closing it then ends any
    // helper-started processes that are still running.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let work = async {
        let (out, err, status) =
            tokio::join!(read_capped(stdout), read_capped(stderr), child.wait());
        let status = status.map_err(ToolError::Io)?;
        Ok::<_, ToolError>(Output {
            code: status.code(),
            stdout: out.map_err(ToolError::Io)?,
            stderr: err.map_err(ToolError::Io)?,
        })
    };
    // The result is bound first so the borrow of `child` ends before the kill below.
    let timed = tokio::time::timeout(limit, work).await;
    match timed {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            Err(ToolError::Timeout(limit))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tdm-tools-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn pin_matches_only_the_right_bytes() {
        let path = temp_file("helper.bin", b"helper-bytes");
        let good = sha256_hex(b"helper-bytes");
        assert!(verify_pin(&path, &good).is_ok());
        assert!(verify_pin(&path, &good.to_uppercase()).is_ok(), "case must not matter");
        assert!(matches!(
            verify_pin(&path, &sha256_hex(b"other")),
            Err(ToolError::HashMismatch)
        ));
    }

    #[test]
    fn pin_refuses_a_directory_and_a_missing_file() {
        let dir = std::env::temp_dir();
        assert!(matches!(verify_pin(&dir, &"0".repeat(64)), Err(ToolError::Missing(_))));
        let gone = dir.join("tdm-tools-definitely-missing.bin");
        assert!(matches!(verify_pin(&gone, &"0".repeat(64)), Err(ToolError::Missing(_))));
    }

    /// A tiny program that prints one word, through the platform shell.
    fn shell() -> (std::path::PathBuf, Vec<String>) {
        if cfg!(windows) {
            (std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"), vec!["/C".into(), "echo tdm-ok".into()])
        } else {
            (std::path::PathBuf::from("/bin/sh"), vec!["-c".into(), "echo tdm-ok".into()])
        }
    }

    #[tokio::test]
    async fn runs_a_helper_and_keeps_its_output() {
        let (program, args) = shell();
        let out = run(&program, &args, Duration::from_secs(30)).await.unwrap();
        assert_eq!(out.code, Some(0));
        assert!(out.stdout.contains("tdm-ok"), "stdout was {:?}", out.stdout);
    }

    #[tokio::test]
    async fn kills_a_helper_that_runs_past_its_limit() {
        let (program, args) = if cfg!(windows) {
            (
                std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"),
                vec!["/C".into(), "ping -n 30 127.0.0.1 >NUL".into()],
            )
        } else {
            (std::path::PathBuf::from("/bin/sh"), vec!["-c".into(), "sleep 30".into()])
        };
        let started = std::time::Instant::now();
        let result = run(&program, &args, Duration::from_millis(400)).await;
        assert!(matches!(result, Err(ToolError::Timeout(_))), "got {result:?}");
        assert!(started.elapsed() < Duration::from_secs(20), "the child was not stopped");
    }

    #[tokio::test]
    async fn refuses_a_missing_program_without_starting_anything() {
        let result = run(Path::new("no-such-helper-tdm"), &[], Duration::from_secs(1)).await;
        assert!(matches!(result, Err(ToolError::Missing(_))));
    }

    #[tokio::test]
    async fn caps_the_output_it_keeps() {
        let bytes = vec![b'x'; OUTPUT_CAP * 3];
        let kept = read_capped(Some(&bytes[..])).await.unwrap();
        assert_eq!(kept.len(), OUTPUT_CAP);
    }
}
