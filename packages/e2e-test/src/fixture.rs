use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::{helper, log};

static SERVICE_BASE: &str = "https://services.gradle.org/distributions";

/// One matrix entry: plant a bad jar, run, verify, then run again for idempotence.
pub fn run_test(root: &Path, gradlew: &Path, version: &str, jdk: &str) -> cu::Result<()> {
    let java_home = helper::ensure_jdk(jdk)?;

    let proj = cu::path!(&root / "target" / "e2e" / version);
    cu::fs::make_dir_empty(&proj)?;
    cu::fs::write(proj.join("build.gradle"), b"")?;
    cu::fs::write(
        proj.join("gradle/wrapper/gradle-wrapper.properties"),
        format!(
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-{version}-bin.zip\n"
        ),
    )?;
    log!("created fixture test project '{}'", proj.display());

    // The untrusted blob this tool exists to neutralise.
    let jar = proj.join("gradle/wrapper/gradle-wrapper.jar");
    cu::fs::write(&jar, "NOT A JAR - PLANTED BY THE TEST SUITE")?;

    // A `gradlew` script in the project, to prove we never overwrite it.
    let script = proj.join("gradlew");
    cu::fs::write(&script, b"#!/bin/sh\n# sentinel\n")?;

    let out = helper::invoke_gradlew(root, gradlew, &proj, &java_home, &["--version"])?;

    // Ran the right Gradle?
    let marker = format!("Gradle {version}");
    if !out.stdout.contains(&marker) {
        cu::bail!("expected stdout to contain {marker:?}");
    }
    log!("ok - gradle version is correct");

    // Jar replaced?
    let jar_bytes = std::fs::read(&jar)?;
    if str::from_utf8(&jar_bytes).is_ok() {
        cu::bail!("expected planted jar to be replaced");
    }
    log!("ok - jar replaced");

    // Matches Gradle's published wrapper checksum, where one exists.
    let jar_hash = helper::sha256(&jar_bytes);
    match helper::fetch_optional(&format!(
        "{SERVICE_BASE}/gradle-{version}-wrapper.jar.sha256"
    ))? {
        Some(published) => {
            if !jar_hash.eq_ignore_ascii_case(&published) {
                cu::bail!("jar sha256 {jar_hash} != published {published}");
            }
            log!("ok - jar checksum verified");
        }
        None => {
            log!("ok - checksum 404");
        }
    }

    // Cached as known-good.
    let cached = root
        .join(".gradle-wrapper/known-good")
        .join(format!("gradle-wrapper-{version}.jar"));
    if !cached.is_file() {
        cu::bail!("expected '{}' to be created but not", cached.display());
    }
    log!("ok - known-good created");

    // Work directory cleaned up.
    let work = root.join(".gradle-wrapper/work");
    match cu::fs::read_dir(&work) {
        Err(_) => {
            log!("ok - work dir doesn't exist")
        }
        Ok(mut leftovers) => {
            if leftovers.next().is_none() {
                log!("ok - work dir is empty");
            } else {
                cu::bail!("entries left behind in '{}'", work.display());
            }
        }
    }

    // Our own gradlew script must be untouched — the `wrapper` task emits one,
    // and copying it would overwrite the binary with a shell script.
    if cu::fs::read(&script)? != b"#!/bin/sh\n# sentinel\n" {
        cu::bail!("the project's gradlew script was overwritten");
    }
    log!("ok - project gradlew script untouched");

    // wait for one second to make sure if the properties were overwritten, it will be a different
    // timestamp
    thread::sleep(Duration::from_secs(1));

    // Second run: no work, byte-identical result.
    let props_before = cu::fs::read(proj.join("gradle/wrapper/gradle-wrapper.properties"))?;
    let out2 = helper::invoke_gradlew(root, gradlew, &proj, &java_home, &["--version"])?;
    for noise in ["downloading", "generating", "replacing"] {
        if out2.stderr.contains(noise) || out2.stdout.contains(noise) {
            cu::bail!("second run was not cached: stderr mentions {noise:?}");
        }
    }
    if cu::fs::read(&jar)? != jar_bytes
        || cu::fs::read(proj.join("gradle/wrapper/gradle-wrapper.properties"))? != props_before
    {
        cu::bail!("second run changed the project files");
    }
    log!("ok - second run did not change project");

    Ok(())
}
