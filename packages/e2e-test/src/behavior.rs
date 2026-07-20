use std::path::{Path, PathBuf};

/// A version that is cheap to reuse for the behaviour tests.
static BEHAVIOUR_VERSION: &str = "9.6.1";
static BEHAVIOUR_JDK: &str = "openjdk@25";

use crate::{helper, log};

pub fn tamper(root: &Path, gradlew: &Path) -> cu::Result<()> {
    let java = helper::ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "tamper", BEHAVIOUR_VERSION, "bin")?;
    let jar = proj.join("gradle/wrapper/gradle-wrapper.jar");

    helper::invoke_gradlew(root, gradlew, &proj, &java, &["--version"])?;
    let good = cu::fs::read(&jar)?;

    cu::fs::write(&jar, b"TAMPERED\n")?;
    helper::invoke_gradlew(root, gradlew, &proj, &java, &["--version"])?;
    if cu::fs::read(&jar)? != good {
        cu::bail!("tampered jar was not restored");
    }
    log!("ok - tampered jar restored");
    Ok(())
}
pub fn all_downgrade(root: &Path, gradlew: &Path) -> cu::Result<()> {
    let java = helper::ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "all", BEHAVIOUR_VERSION, "all")?;
    helper::invoke_gradlew(root, gradlew, &proj, &java, &["--version"])?;
    let props = cu::fs::read_string(proj.join("gradle/wrapper/gradle-wrapper.properties"))?;
    if !props.contains("-bin.zip") {
        cu::bail!("properties were not replaced with Gradle's own -bin file: {props}");
    }
    log!("ok - properties was replaced with -bin url");
    Ok(())
}

pub fn bad_version(root: &Path, gradlew: &Path) -> cu::Result<()> {
    let java = helper::ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "badversion", "99.99.99", "bin")?;
    // Use a throwaway cache so a failure can't be confused with real state.
    let home = root.join("target/e2e/temp/badversion-home");
    let _ = std::fs::remove_dir_all(&home);

    let (ok, out) = helper::invoke_gradlew_capture_failure_with_wrapper_home(
        root,
        gradlew,
        &proj,
        &java,
        &home,
        &["--version"],
    )?;
    if ok.is_ok() {
        cu::bail!("expected failure when running with bad gradle version");
    }
    cu::ensure!(out.stdout.contains("failed to download gradle"))?;
    let kg = home.join("known-good");
    if kg.is_dir() && cu::fs::read_dir(&kg)?.next().is_some() {
        cu::bail!("a failed run left files in known-good/");
    }
    log!("ok - failed run did not write to known-good");
    Ok(())
}

pub fn exit_code(root: &Path, gradlew: &Path) -> cu::Result<()> {
    let java = helper::ensure_jdk(BEHAVIOUR_JDK)?;
    let proj = behaviour_project(root, "exitcode", BEHAVIOUR_VERSION, "bin")?;
    let result = helper::invoke_gradlew(root, gradlew, &proj, &java, &["definitelyNotATask"]);
    if result.is_ok() {
        cu::bail!("a failing task should not exit 0");
    }
    log!("ok - failed run exited with failure status");
    Ok(())
}

pub fn java_home_invalid(root: &Path, gradlew: &Path) -> cu::Result<()> {
    let proj = behaviour_project(root, "badjava", BEHAVIOUR_VERSION, "bin")?;
    let (ok, out) = helper::invoke_gradlew_capture_failure(
        root,
        gradlew,
        &proj,
        &root.join(".jabba/not-jdk/not-jdk-version"),
        &["--version"],
    )?;
    if ok.is_ok() {
        cu::bail!("expected failure");
    }
    log!("ok - invalid JAVA_HOME failed run");
    cu::ensure!(
        out.stdout
            .contains("JAVA_HOME is set to an invalid directory")
    )?;
    log!("ok - invalid JAVA_HOME failed with correct message");
    Ok(())
}

/// A project set up for the behaviour tests, using the already-cached version.
fn behaviour_project(
    root: &Path,
    name: &str,
    url_version: &str,
    kind: &str,
) -> cu::Result<PathBuf> {
    let proj = cu::path!(&root / "target" / "e2e" / "behaviour" / name);
    cu::fs::make_dir_empty(&proj)?;
    cu::fs::write(proj.join("build.gradle"), b"")?;
    cu::fs::write(
        proj.join("gradle/wrapper/gradle-wrapper.properties"),
        format!(
            "distributionUrl=https\\://services.gradle.org/distributions/gradle-{url_version}-{kind}.zip\n"
        ),
    )?;
    cu::fs::write(proj.join("gradle/wrapper/gradle-wrapper.jar"), b"GARBAGE\n")?;
    log!("created behaviour test project '{}'", proj.display());
    Ok(proj)
}
