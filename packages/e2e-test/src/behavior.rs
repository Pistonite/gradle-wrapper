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

pub fn version_file_preferred(root: &Path, gradlew: &Path) -> cu::Result<()> {
    let java = helper::ensure_jdk(BEHAVIOUR_JDK)?;

    // The properties file names a different version from .version.
    // since 2.0 cannot run on JDK 25 it will just fail which tells us
    // the wrong version is used
    let proj = behaviour_project(root, "versionfile", "2.0", "bin")?;
    // Trailing newline is intentional - the version must be trimmed.
    cu::fs::write(
        proj.join("gradle/wrapper/.version"),
        format!("{BEHAVIOUR_VERSION}\n"),
    )?;

    let out = helper::invoke_gradlew(root, gradlew, &proj, &java, &["--version"])?;
    cu::ensure!(out.stdout.contains(&format!("Gradle {BEHAVIOUR_VERSION}")))?;
    cu::ensure!(!out.stdout.contains("Gradle 2.0"))?;
    log!("ok - .version won over a different version in gradle-wrapper.properties");

    // The properties file should have been replaced with the known-good one for
    // the version .version asked for.
    let props = cu::fs::read_string(proj.join("gradle/wrapper/gradle-wrapper.properties"))?;
    if !props.contains(&format!("gradle-{BEHAVIOUR_VERSION}-bin.zip")) {
        cu::bail!(
            "properties were not replaced with the known-good {BEHAVIOUR_VERSION} file: {props}"
        );
    }
    log!("ok - properties replaced with the known-good copy");

    // Now make the properties file unparseable. With .version present the tool
    // should never look at it, so this must still work.
    cu::fs::write(
        proj.join("gradle/wrapper/gradle-wrapper.properties"),
        b"!!! not a properties file - no distributionUrl here at all !!!\n",
    )?;
    let out = helper::invoke_gradlew(root, gradlew, &proj, &java, &["--version"])?;
    cu::ensure!(out.stdout.contains(&format!("Gradle {BEHAVIOUR_VERSION}")))?;
    log!("ok - .version worked with an unparseable gradle-wrapper.properties");

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
