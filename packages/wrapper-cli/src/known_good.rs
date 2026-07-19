use std::path::{Path, PathBuf};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::process::{self, Command, Stdio};
use std::time::SystemTime;

use cu::pre::*;

use crate::properties::ValidatedVersion;
use crate::{exec, checksum, gradle_dist};

/// Paths to the cached wrapper files for a given Gradle version.
#[derive(Debug)]
pub struct KnownGood {
    pub jar: PathBuf,
    pub properties: PathBuf,
}

impl KnownGood {
    /// Copy into known good cache
    pub fn promote(
        wrapper_home: &Path,
        version: &ValidatedVersion,
        jar: PathBuf,
        properties: PathBuf,
    ) -> cu::Result<Self> {
        let kg = Self::from_paths_unchecked(wrapper_home, version);
        cu::fs::copy(jar, &kg.jar)?;
        cu::fs::copy(properties, &kg.properties)?;
        cu::check!(
            Self::find(wrapper_home, version),
            "failed to copy generated wrapper into known good cache"
        )
    }
    pub fn find(wrapper_home: &Path, version: &ValidatedVersion) -> Option<Self> {
        let kg = Self::from_paths_unchecked(wrapper_home, version);
        if kg.jar.is_file() && kg.properties.is_file() {
            cu::debug!("known-good cache hit for {version}: {kg:?}");
            return Some(kg);
        }
        cu::debug!("known-good cache miss for {version}");
        None
    }

    fn from_paths_unchecked(wrapper_home: &Path, version: &ValidatedVersion) -> Self {
        let dir = wrapper_home.join("known-good");
        let jar = dir.join(format!("gradle-wrapper-{version}.jar"));
        let properties = dir.join(format!("gradle-wrapper-{version}.properties"));
        Self { jar, properties }
    }
}

pub fn generate(
    version: &ValidatedVersion,
    project: &Path,
    java: &Path,
    wrapper_home: &Path
) -> cu::Result<()> {
    let bar = cu::progress("generating gradle-wrapper.jar")
        .keep(false)
        .spawn();
    // generate a work directory name unique to this invocation.
    let work_id = {
        let mut h = DefaultHasher::new();
        version.hash(&mut h);
        process::id().hash(&mut h);
        project.hash(&mut h);
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .hash(&mut h);
        format!("{:016x}", h.finish())
    };
    let work_dir = cu::path!(&(&wrapper_home) / "work" / work_id);
    cu::fs::make_dir_empty(&work_dir)?;

    cu::progress!(bar, "downloading gradle");
    let zip_path = work_dir.join("gradle-bin.zip");
    cu::check!(
        gradle_dist::fetch_bin_zip(&version, &zip_path),
        "failed to download gradle"
    )?;

    cu::progress!(bar, "unzipping gradle");
    let unpacked = work_dir.join("gradle-bin");
    let dist = gradle_dist::unzip(&zip_path, &version, &unpacked)?;
    let launcher_jar = cu::check!(
        gradle_dist::find_launcher_jar(&dist),
        "failed to find gradle launcher in downloaded gradle"
    )?;

    cu::progress!(bar, "running 'gradle wrapper'");
    let (jar, properties) = {
        // generate a stub build.gradle for gradle to treat the directory as a project
        let stub = work_dir.join("stub-project");
        cu::fs::write(stub.join("build.gradle"), b"")?;
        // not using cu::Command since that depends on tokio
        let mut cmd = Command::new(java);
        cmd.current_dir(&stub)
            .args(exec::jvm_opts())
            .arg("-Dorg.gradle.appname=gradle")
            .arg("-jar")
            .arg(&launcher_jar)
            .arg("wrapper")
            // Don't leave a daemon running out of a directory we're about to delete.
            .arg("--no-daemon")
            // capture stdout/stderr and print it with debug so it does not appear by default
            // e.g. when piping gradlew properties | grep ...
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // No --gradle-version: the option only exists from Gradle ~4.8, and it is redundant anyway.
        cu::debug!("launching gradle command: {cmd:?}");
        let mut child = cu::check!(cmd.spawn(), "failed to run gradle wrapper")?;
        let status = child.wait();
        if let Some(mut out) = child.stdout.take() {
            let mut s = String::new();
            if out.read_to_string(&mut s).is_ok() {
                for l in s.lines() {
                    cu::debug!("[gradle:stdout] {l}");
                }
            }
        }
        if let Some(mut out) = child.stderr.take() {
            let mut s = String::new();
            if out.read_to_string(&mut s).is_ok() {
                for l in s.lines() {
                    cu::debug!("[gradle:stderr] {l}");
                }
            }
        }
        let status = cu::check!(status, "error waiting for gradle wrapper command")?;
        if !status.success() {
            cu::bail!("gradle wrapper command failed with {status}");
        }
        let generated_jar = stub.join("gradle/wrapper/gradle-wrapper.jar");
        let generated_props = stub.join("gradle/wrapper/gradle-wrapper.properties");
        if !generated_jar.is_file() {
            cu::bail!(
                "'gradle wrapper' command did not produce '{}'",
                generated_jar.display()
            );
        }
        if !generated_props.is_file() {
            cu::bail!(
                "'gradle wrapper' command did not produce '{}'",
                generated_props.display()
            );
        }
        // purposely re-fetching even the main driver already fetched it - in case the main
        // driver had an error when fetching
        match gradle_dist::fetch_wrapper_sha256(&version) {
            Err(e) => {
                // this is ok and needed because:
                // 1. some gradle versions don't publish an expected sha256
                // 2. the jar is generated by the verified distribution, so the trust chain
                //    still holds
                cu::warn!(
                "failed to get expected wrapper sha256, skipping verification: {e:?}"
            );
            }
            Ok(x) => {
                let actual_sha256 = checksum::sha256_file(&generated_jar)?;
                cu::check!(
                    checksum::verify(&x, &actual_sha256),
                    "failed to verify integrity of generated wrapper jar"
                )?;
            }
        };
        (generated_jar, generated_props)
    };
    bar.done();

    let known_good = KnownGood::promote(wrapper_home, &version, jar, properties)?;

    let _ = cu::fs::rec_remove(&work_dir);
    cu::debug!("saved known good wrapper to cache: {known_good:?}");
    Ok(())
}
