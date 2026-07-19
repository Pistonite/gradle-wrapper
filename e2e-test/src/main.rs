//! End-to-end fixture suite for `gradlew`, across the Gradle versions and JDKs
//! listed in `TEST_PLAN.md`.
//!
//! This is a binary rather than `#[test]` functions: the suite runs for minutes,
//! downloads gigabytes, and needs ordered progress output and a summary table —
//! all of which fight the test harness. `cargo test` stays fast and offline.
//!
//! Usage:
//!   cargo run -p e2e-test              # everything
//!   cargo run -p e2e-test -- 2.0 9.6.1 # only these Gradle versions
//!   cargo run -p e2e-test -- behaviour # only the behaviour tests

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

/// (Gradle version, JDK). Java 8 covers Gradle 2.0–8.14.x, so one JDK carries
/// almost the whole range; see GRADLE_COMPATIBILITY.md.
const MATRIX: &[(&str, &str)] = &[
    ("2.0", "openjdk@8"),    // the floor
    ("2.14.1", "openjdk@8"), // last 2.x
    ("3.5.1", "openjdk@8"),  // last 3.x — no published wrapper checksum
    ("4.10.3", "openjdk@8"), // last 4.x
    ("5.6.4", "openjdk@8"),  // last 5.x
    ("6.9.4", "openjdk@8"),  // last 6.x
    ("7.6.4", "openjdk@8"),  // last 7.x
    ("8.14.3", "openjdk@8"), // last 8.x — Java 8's upper bound
    ("9.0.0", "openjdk@21"), // first 9.x, gradle-gradle-cli-main layout
    ("9.6.1", "openjdk@25"), // current release on the latest LTS
];

/// A version that is cheap to reuse for the behaviour tests.
const BEHAVIOUR_VERSION: &str = "9.6.1";
const BEHAVIOUR_JDK: &str = "openjdk@25";

const SERVICES: &str = "https://services.gradle.org/distributions";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(e) => {
            eprintln!("\nharness error: {e:?}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool> {
    let root = project_root();

    // JABBA_HOME must be set before any jabba call, or JDKs are installed
    // system-wide instead of project-local. Set once, here, so no call site can
    // forget it.
    let jabba_home = root.join(".jabba");
    unsafe { std::env::set_var("JABBA_HOME", &jabba_home) };
    println!("JABBA_HOME = {}", jabba_home.display());

    let filters: Vec<String> = std::env::args().skip(1).collect();
    let want = |name: &str| filters.is_empty() || filters.iter().any(|f| f == name);

    let gradlew = build_gradlew(&root)?;
    println!("binary     = {}\n", gradlew.display());

    let mut results: Vec<(String, Result<String>)> = Vec::new();

    for (version, jdk) in MATRIX {
        if !want(version) {
            continue;
        }
        println!("=== Gradle {version} on {jdk} ===");
        let outcome = fixture(&root, &gradlew, version, jdk);
        match &outcome {
            Ok(note) => println!("    PASS {note}\n"),
            Err(e) => println!("    FAIL {e:#}\n"),
        }
        results.push((format!("gradle {version}"), outcome));
    }

    if want("behaviour") {
        for (name, outcome) in behaviour_tests(&root, &gradlew) {
            match &outcome {
                Ok(note) => println!("    PASS {name} {note}"),
                Err(e) => println!("    FAIL {name}: {e:#}"),
            }
            results.push((name, outcome));
        }
        println!();
    }

    // Summary
    println!("{:-<64}", "");
    let mut failed = 0;
    for (name, outcome) in &results {
        match outcome {
            Ok(note) => println!("  PASS  {name:<28} {note}"),
            Err(e) => {
                failed += 1;
                println!("  FAIL  {name:<28} {e:#}");
            }
        }
    }
    println!("{:-<64}", "");
    println!("{} passed, {failed} failed", results.len() - failed);

    Ok(failed == 0)
}

/// One matrix entry: plant a bad jar, run, verify, then run again for idempotence.
fn fixture(root: &Path, gradlew: &Path, version: &str, jdk: &str) -> Result<String> {
    let java_home = ensure_jdk(jdk)?;

    let proj = root.join("target/e2e").join(version);
    let _ = std::fs::remove_dir_all(&proj);
    std::fs::create_dir_all(proj.join("gradle/wrapper"))?;
    std::fs::write(proj.join("build.gradle"), b"")?;
    std::fs::write(
        proj.join("gradle/wrapper/gradle-wrapper.properties"),
        format!(
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-{version}-bin.zip\n"
        ),
    )?;

    // The untrusted blob this tool exists to neutralise.
    const PLANTED: &[u8] = b"NOT A JAR - PLANTED BY THE TEST SUITE\n";
    let jar = proj.join("gradle/wrapper/gradle-wrapper.jar");
    std::fs::write(&jar, PLANTED)?;

    // A `gradlew` script in the project, to prove we never overwrite it.
    let script = proj.join("gradlew");
    std::fs::write(&script, b"#!/bin/sh\n# sentinel\n")?;

    let out = invoke(gradlew, &proj, &java_home, root, &["--version"])?;
    if !out.status.success() {
        bail!(
            "exit {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status.code(),
            tail(&out.stdout, 40),
            tail(&out.stderr, 40)
        );
    }

    // Ran the right Gradle?
    let marker = format!("Gradle {version}");
    if !out.stdout.contains(&marker) {
        bail!(
            "stdout does not contain {marker:?}\n--- stdout ---\n{}",
            tail(&out.stdout, 40)
        );
    }

    // Jar replaced?
    let jar_bytes = std::fs::read(&jar)?;
    if jar_bytes == PLANTED {
        bail!("planted jar was NOT replaced");
    }

    // Matches Gradle's published wrapper checksum, where one exists.
    let jar_hash = sha256(&jar_bytes);
    let note = match fetch_optional(&format!("{SERVICES}/gradle-{version}-wrapper.jar.sha256"))? {
        Some(published) => {
            if !jar_hash.eq_ignore_ascii_case(&published) {
                bail!("jar sha256 {jar_hash} != published {published}");
            }
            "(checksum verified)".to_string()
        }
        None => {
            // The warning is only emitted while generating. On a repeat suite
            // run the version is already in the known-good cache, so nothing is
            // generated and nothing warns — only assert it when this run
            // actually did the work.
            let generated = out.stderr.contains("generating a trusted");
            if generated && !out.stderr.contains("publishes no wrapper checksum") {
                bail!("no published checksum for {version}, but no warning was emitted");
            }
            if generated {
                "(no published checksum - warned, dist-verified)".to_string()
            } else {
                "(no published checksum - cached from an earlier run)".to_string()
            }
        }
    };

    // Cached as known-good.
    let cached = root
        .join(".gradle-wrapper/known-good")
        .join(format!("gradle-wrapper-{version}.jar"));
    if !cached.is_file() {
        bail!("{} was not created", cached.display());
    }

    // Work directory cleaned up.
    let work = root.join(".gradle-wrapper/work");
    if work.is_dir() {
        let leftovers: Vec<_> = std::fs::read_dir(&work)?.filter_map(|e| e.ok()).collect();
        if !leftovers.is_empty() {
            bail!("{} left {} entries behind", work.display(), leftovers.len());
        }
    }

    // Our own gradlew script must be untouched — the `wrapper` task emits one,
    // and copying it would overwrite the binary with a shell script.
    if std::fs::read(&script)? != b"#!/bin/sh\n# sentinel\n" {
        bail!("the project's gradlew script was overwritten");
    }

    // Second run: no work, byte-identical result.
    let props_before = std::fs::read(proj.join("gradle/wrapper/gradle-wrapper.properties"))?;
    let out2 = invoke(gradlew, &proj, &java_home, root, &["--version"])?;
    if !out2.status.success() {
        bail!("second run failed: {}", tail(&out2.stderr, 20));
    }
    for noise in ["downloading", "generating", "replacing"] {
        if out2.stderr.contains(noise) {
            bail!("second run was not cached: stderr mentions {noise:?}");
        }
    }
    if std::fs::read(&jar)? != jar_bytes
        || std::fs::read(proj.join("gradle/wrapper/gradle-wrapper.properties"))? != props_before
    {
        bail!("second run changed the project files");
    }

    Ok(note)
}

fn behaviour_tests(root: &Path, gradlew: &Path) -> Vec<(String, Result<String>)> {
    let mut out = Vec::new();
    println!("=== behaviour ===");
    for (name, f) in [
        ("tamper", tamper as fn(&Path, &Path) -> Result<String>),
        ("all-downgrade", all_downgrade),
        ("bad-version", bad_version),
        ("exit-code", exit_code),
        ("java-home-invalid", java_home_invalid),
        ("java-home-missing", java_home_missing),
        ("empty-wrapper-home", empty_wrapper_home),
    ] {
        out.push((name.to_string(), f(root, gradlew)));
    }
    out
}

/// A project set up for the behaviour tests, using the already-cached version.
fn behaviour_project(root: &Path, name: &str, url_version: &str, kind: &str) -> Result<PathBuf> {
    let proj = root.join("target/e2e/behaviour").join(name);
    let _ = std::fs::remove_dir_all(&proj);
    std::fs::create_dir_all(proj.join("gradle/wrapper"))?;
    std::fs::write(proj.join("build.gradle"), b"")?;
    std::fs::write(
        proj.join("gradle/wrapper/gradle-wrapper.properties"),
        format!(
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-{url_version}-{kind}.zip\n"
        ),
    )?;
    std::fs::write(proj.join("gradle/wrapper/gradle-wrapper.jar"), b"GARBAGE\n")?;
    Ok(proj)
}

fn tamper(root: &Path, gradlew: &Path) -> Result<String> {
    let java = ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "tamper", BEHAVIOUR_VERSION, "bin")?;
    let jar = proj.join("gradle/wrapper/gradle-wrapper.jar");

    invoke(gradlew, &proj, &java, root, &["--version"])?;
    let good = std::fs::read(&jar)?;

    std::fs::write(&jar, b"TAMPERED\n")?;
    let out = invoke(gradlew, &proj, &java, root, &["--version"])?;
    if !out.status.success() {
        bail!("run after tampering failed");
    }
    if std::fs::read(&jar)? != good {
        bail!("tampered jar was not restored");
    }
    Ok("(restored)".to_string())
}

fn all_downgrade(root: &Path, gradlew: &Path) -> Result<String> {
    let java = ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "all", BEHAVIOUR_VERSION, "all")?;
    let out = invoke(gradlew, &proj, &java, root, &["--version"])?;
    if !out.status.success() {
        bail!("run failed: {}", tail(&out.stderr, 20));
    }
    let props = std::fs::read_to_string(proj.join("gradle/wrapper/gradle-wrapper.properties"))?;
    if !props.contains("-bin.zip") {
        bail!("properties were not replaced with Gradle's own -bin file: {props}");
    }
    Ok("(-all downgraded to -bin)".to_string())
}

fn bad_version(root: &Path, gradlew: &Path) -> Result<String> {
    let java = ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "badversion", "99.99.99", "bin")?;
    // Use a throwaway cache so a failure can't be confused with real state.
    let home = root.join("target/e2e/behaviour/badversion-home");
    let _ = std::fs::remove_dir_all(&home);

    let out = invoke_with_home(gradlew, &proj, &java, &home, &["--version"])?;
    if out.status.success() {
        bail!("expected failure for a nonexistent version");
    }
    let kg = home.join("known-good");
    if kg.is_dir() && std::fs::read_dir(&kg)?.next().is_some() {
        bail!("a failed run left files in known-good/");
    }
    Ok("(failed cleanly, nothing cached)".to_string())
}

fn exit_code(root: &Path, gradlew: &Path) -> Result<String> {
    let java = ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "exitcode", BEHAVIOUR_VERSION, "bin")?;
    let out = invoke(gradlew, &proj, &java, root, &["definitelyNotATask"])?;
    if out.status.success() {
        bail!("a failing task should not exit 0");
    }
    Ok(format!("(exit {:?})", out.status.code()))
}

fn java_home_invalid(root: &Path, gradlew: &Path) -> Result<String> {
    let proj = behaviour_project(root, "badjava", BEHAVIOUR_VERSION, "bin")?;
    let out = invoke(
        gradlew,
        &proj,
        Path::new("/nonexistent"),
        root,
        &["--version"],
    )?;
    if out.status.success() {
        bail!("expected failure");
    }
    if !out
        .stderr
        .contains("JAVA_HOME is set to an invalid directory")
    {
        bail!("wrong message: {}", tail(&out.stderr, 10));
    }
    Ok("(script message reproduced)".to_string())
}

fn java_home_missing(root: &Path, gradlew: &Path) -> Result<String> {
    let proj = behaviour_project(root, "nojava", BEHAVIOUR_VERSION, "bin")?;
    let out = Command::new(gradlew)
        .arg("--version")
        .current_dir(&proj)
        .env_remove("JAVA_HOME")
        .env("PATH", "/nonexistent")
        .env("GRADLE_WRAPPER_HOME", root.join(".gradle-wrapper"))
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        bail!("expected failure");
    }
    if !stderr.contains("JAVA_HOME is not set and no 'java' command could be found") {
        bail!("wrong message: {stderr}");
    }
    Ok("(script message reproduced)".to_string())
}

/// An empty GRADLE_WRAPPER_HOME must be treated as unset, not as a relative
/// path, or cache directories get scattered through the user's project.
fn empty_wrapper_home(root: &Path, gradlew: &Path) -> Result<String> {
    let java = ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "emptyhome", BEHAVIOUR_VERSION, "bin")?;
    let fake_home = root.join("target/e2e/behaviour/fake-home");
    std::fs::create_dir_all(&fake_home)?;

    let out = Command::new(gradlew)
        .arg("--version")
        .current_dir(&proj)
        .env("JAVA_HOME", &java)
        .env("GRADLE_WRAPPER_HOME", "") // the case under test
        .env("HOME", &fake_home)
        .env("GRADLE_USER_HOME", root.join(".gradle-test-home"))
        .output()?;
    let _ = out;

    if proj.join("known-good").exists() || proj.join("work").exists() {
        bail!("empty GRADLE_WRAPPER_HOME created cache dirs inside the project");
    }
    if !fake_home.join(".gradle-wrapper").exists() {
        bail!("did not fall back to $HOME/.gradle-wrapper");
    }
    Ok("(fell back to $HOME)".to_string())
}

// ---------------------------------------------------------------- helpers

struct Output {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn invoke(
    gradlew: &Path,
    proj: &Path,
    java_home: &Path,
    root: &Path,
    args: &[&str],
) -> Result<Output> {
    invoke_with_home(
        gradlew,
        proj,
        java_home,
        &root.join(".gradle-wrapper"),
        args,
    )
}

fn invoke_with_home(
    gradlew: &Path,
    proj: &Path,
    java_home: &Path,
    wrapper_home: &Path,
    args: &[&str],
) -> Result<Output> {
    let root = project_root();
    let out = Command::new(gradlew)
        .args(args)
        .current_dir(proj)
        .env("JAVA_HOME", java_home)
        .env("GRADLE_WRAPPER_HOME", wrapper_home)
        // Keep Gradle's own distribution cache project-local and persistent, so
        // repeated runs don't re-download every distribution.
        .env("GRADLE_USER_HOME", root.join(".gradle-test-home"))
        .output()
        .with_context(|| format!("cannot run {}", gradlew.display()))?;

    Ok(Output {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("e2e-test is a workspace member")
        .to_path_buf()
}

fn build_gradlew(root: &Path) -> Result<PathBuf> {
    println!("building gradlew...");
    let status = Command::new(env!("CARGO"))
        .args(["build", "--release", "-p", "gradle-wrapper"])
        .current_dir(root)
        .status()?;
    if !status.success() {
        bail!("cargo build failed");
    }
    let bin = root.join("target/release/gradlew");
    if !bin.is_file() {
        bail!("{} not found after build", bin.display());
    }
    Ok(bin)
}

/// Resolve a JDK, installing it project-local if it isn't there yet.
///
/// Never assumes the JDK is present: `jabba which` first, then `jabba install`
/// on failure, then `which` again.
fn ensure_jdk(version: &str) -> Result<PathBuf> {
    if let Some(dir) = jabba_which(version) {
        return Ok(dir);
    }

    println!("    installing JDK {version} (this takes a while)...");
    let status = Command::new("jabba")
        .args(["install", version])
        .status()
        .context("cannot run jabba - is it on PATH?")?;
    if !status.success() {
        bail!("jabba install {version} failed");
    }

    jabba_which(version).ok_or_else(|| anyhow!("jabba installed {version} but cannot locate it"))
}

fn jabba_which(version: &str) -> Option<PathBuf> {
    let out = Command::new("jabba")
        .args(["which", version])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    if path.as_os_str().is_empty() {
        return None;
    }
    // macOS bundles nest the real home; check both shapes.
    [path.clone(), path.join("Contents/Home")]
        .into_iter()
        .find(|c| c.join("bin/java").is_file() || c.join("bin/java.exe").is_file())
}

fn fetch_optional(url: &str) -> Result<Option<String>> {
    match ureq::get(url).call() {
        Ok(mut resp) => Ok(Some(resp.body_mut().read_to_string()?.trim().to_string())),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("GET {url}")),
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Last `n` lines, so a failure report stays readable.
fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}
