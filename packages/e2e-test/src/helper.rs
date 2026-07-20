use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use cu::pre::*;
use sha2::{Digest, Sha256};

use crate::log;

pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

pub fn invoke_gradlew(
    root: &Path,
    gradlew: &Path,
    proj: &Path,
    java_home: &Path,
    args: &[&str],
) -> cu::Result<Output> {
    invoke_gradlew_with_wrapper_home(
        root,
        gradlew,
        proj,
        java_home,
        &root.join(".gradle-wrapper"),
        args,
    )
}

pub fn invoke_gradlew_capture_failure(
    root: &Path,
    gradlew: &Path,
    proj: &Path,
    java_home: &Path,
    args: &[&str],
) -> cu::Result<(cu::Result<()>, Output)> {
    invoke_gradlew_capture_failure_with_wrapper_home(
        root,
        gradlew,
        proj,
        java_home,
        &root.join(".gradle-wrapper"),
        args,
    )
}

pub fn invoke_gradlew_with_wrapper_home(
    root: &Path,
    gradlew: &Path,
    proj: &Path,
    java_home: &Path,
    gradle_wrapper_home: &Path,
    args: &[&str],
) -> cu::Result<Output> {
    let (ok, output) = invoke_gradlew_capture_failure_with_wrapper_home(
        root,
        gradlew,
        proj,
        java_home,
        gradle_wrapper_home,
        args,
    )?;
    ok?;
    Ok(output)
}

pub fn invoke_gradlew_capture_failure_with_wrapper_home(
    root: &Path,
    gradlew: &Path,
    proj: &Path,
    java_home: &Path,
    gradle_wrapper_home: &Path,
    args: &[&str],
) -> cu::Result<(cu::Result<()>, Output)> {
    let (child, stdout, stderr) = gradlew
        .command()
        .current_dir(proj)
        .args(args)
        .arg("--no-daemon")
        .env("JAVA_HOME", java_home)
        .env("GRADLE_WRAPPER_HOME", gradle_wrapper_home)
        // Keep Gradle's own distribution cache project-local and persistent, so
        // repeated runs don't re-download every distribution.
        .env("GRADLE_USER_HOME", root.join(".gradle-test-home"))
        .stdout(cu::pio::string())
        .stderr(cu::pio::string())
        .stdin_null()
        .spawn()?;
    let result = child.wait_nz();
    let stdout = stdout.join().flatten().unwrap_or_default();
    let stderr = stderr.join().flatten().unwrap_or_default();
    for l in stdout.lines() {
        log!("[stdout] {l}");
    }
    for l in stderr.lines() {
        log!("[stderr] {l}");
    }
    Ok((result, Output { stdout, stderr }))
}

/// Resolve a JDK, installing it project-local if it isn't there yet, return the JAVA_HOME for that
/// jdk
pub fn ensure_jdk(version: &str) -> cu::Result<PathBuf> {
    if let Ok(dir) = jabba_which(version) {
        return Ok(dir);
    }

    let (child, bar, _) = cu::which("jabba")?
        .command()
        .args(["install", version])
        .stdoe(
            cu::pio::spinner(format!("installing {version}")).configure_spinner(|x| x.keep(false)),
        )
        .stdin_null()
        .spawn()?;
    child.wait_nz()?;
    bar.done();

    log!("installed {version}");
    jabba_which(version)
}

fn jabba_which(version: &str) -> cu::Result<PathBuf> {
    let (child, stdout) = cu::which("jabba")?
        .command()
        .args(["which", version, "--home"])
        .stdout(cu::pio::string())
        .stderr(cu::lv::D)
        .stdin_null()
        .spawn()?;
    child.wait_nz()?;
    let stdout = stdout.join()??;
    if stdout.is_empty() {
        cu::bail!("cannot find installed {version}");
    }
    log!("resolved {version}: {stdout}");
    Ok(stdout.into())
}

pub fn fetch_optional(url: &str) -> cu::Result<Option<String>> {
    match ureq::get(url).call() {
        Ok(mut resp) => Ok(Some(resp.body_mut().read_to_string()?.trim().to_string())),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => cu::rethrow!(e, "GET {url} failed"),
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub static VERBOSE: AtomicBool = AtomicBool::new(false);
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        if $crate::helper::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            cu::print!($($arg)*);
        } else {
            cu::debug!($($arg)*);
        }
    }
}
