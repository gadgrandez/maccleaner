//! Running external tools (Homebrew, Docker) with fixed arguments and a time limit.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The first of `paths` that is a file.
pub fn first_existing(paths: &[&str]) -> Option<PathBuf> {
    paths.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Runs with a time limit; returns (success, stdout, stderr). The output is read while the
/// program runs, so a large one (Homebrew's JSON) can't fill the pipe and block it.
pub fn run_split(program: &Path, args: &[&str], timeout: Duration) -> Result<(bool, String, String), String> {
    let mut child = Command::new(program)
        .args(args)
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .env("HOMEBREW_NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut out = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut out);
            }
            out
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("timed out".into());
        }
        thread::sleep(Duration::from_millis(100));
    };
    Ok((status.success(), stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default()))
}

/// Like `run_split`, with stdout and stderr together.
pub fn run(program: &Path, args: &[&str], timeout: Duration) -> Result<(bool, String), String> {
    run_split(program, args, timeout).map(|(ok, out, err)| (ok, out + &err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_large_output_and_status() {
        // 1 MB of output would fill the pipe if it were read only at the end.
        let (ok, out, err) = run_split(Path::new("/bin/sh"), &["-c", "head -c 1000000 /dev/zero | tr '\\0' x; echo e >&2; exit 3"], Duration::from_secs(10)).unwrap();
        assert!(!ok);
        assert_eq!(out.len(), 1_000_000);
        assert_eq!(err, "e\n");
        assert!(run(Path::new("/bin/sleep"), &["5"], Duration::from_millis(200)).is_err());
    }
}
