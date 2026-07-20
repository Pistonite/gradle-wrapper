use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::{self, JoinHandle};

use cu::pre::*;

use crate::known_good::{self, KnownGood};
use crate::properties::{self, ValidatedVersion};
use crate::{checksum, exec};

#[derive(Clone)]
pub struct Environment {
    /// Root of the current project (located by looking for
    /// gradle/wrapper/gradle-wrapper.properties)
    project: PathBuf,
    /// The java executable
    java: PathBuf,
    /// The managed directory that stores known-good wrapper jars, temp downloads, and temp projects
    /// for generating the wrapper jars
    wrapper_home: PathBuf,
}

////////////////////// Init Operations

impl Environment {
    pub fn setup() -> cu::Result<Self> {
        let project = find_project_dir()?;
        let java = find_java()?;
        let wrapper_home = find_wrapper_home()?;
        Ok(Self {
            project,
            java,
            wrapper_home,
        })
    }
}

/// Locate the project root.
///
/// The official `gradlew` script expects itself to be located at the root of the project.
/// However in our program, the script is installed on the system, so we need to walk the
/// current directory to find the root
fn find_project_dir() -> cu::Result<PathBuf> {
    let markers = [
        "gradle/wrapper/.version",
        "gradle/wrapper/gradle-wrapper.properties",
    ];

    let cwd = cu::check!(
        std::env::current_dir(),
        "cannot determine current directory"
    )?;
    let mut dir = cwd.as_path();
    loop {
        for marker in markers {
            if dir.join(marker).is_file() {
                cu::debug!("project dir: {}", dir.display());
                return Ok(dir.to_path_buf());
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    cu::bail!("no Gradle project found.");
}

/// Find the `java` executable, mirroring the `gradlew` shell script's logic
pub fn find_java() -> cu::Result<PathBuf> {
    let java_home = cu::env_var("JAVA_HOME").unwrap_or_default();
    // The .bat strips quotes from JAVA_HOME; harmless to do everywhere.
    let java_home = java_home.trim().trim_matches('"');

    if !java_home.is_empty() {
        let home = Path::new(java_home);
        cu::debug!("JAVA_HOME is set: '{}'", home.display());

        // IBM's JDK on AIX uses strange locations for the executables.
        let aix = home.join("jre/sh/java");
        let cmd = if cfg!(unix) && is_executable(&aix) {
            aix
        } else if cfg!(windows) {
            home.join("bin/java.exe")
        } else {
            home.join("bin/java")
        };

        if !is_executable(&cmd) {
            cu::error!("JAVA_HOME is set to an invalid directory: {java_home}.");
            cu::error!("");
            cu::hint!(
                "Please set the JAVA_HOME variable in your environment to match the location of your Java installation"
            );
            cu::bail!("cannot locate java runtime");
        }
        cu::debug!("found java from JAVA_HOME: '{}'", cmd.display());
        return Ok(cmd);
    }

    // fallback to find in PATH
    match cu::which(if cfg!(windows) { "java.exe" } else { "java" }) {
        Ok(java) => {
            cu::debug!("found java from PATH: '{}'", java.display());
            Ok(java)
        }
        Err(e) => {
            cu::error!("JAVA_HOME is not set and no 'java' command could be found in your PATH.");
            cu::error!("");
            cu::hint!(
                "Please set the JAVA_HOME variable in your environment to match the location of your Java installation"
            );
            cu::rethrow!(e, "cannot locate java runtime");
        }
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The managed directory, `$GRADLE_WRAPPER_HOME` or `~/.gradle-wrapper`.
fn find_wrapper_home() -> cu::Result<PathBuf> {
    // An empty value is treated as unset, matching how find_java() handles an
    // empty JAVA_HOME. Otherwise `GRADLE_WRAPPER_HOME=` would resolve to a
    // relative path and scatter cache directories through the user's project.
    let dir = cu::env_var("GRADLE_WRAPPER_HOME").unwrap_or_default();
    if !dir.is_empty() {
        return Ok(dir.into());
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_default();
    if home.is_empty() {
        cu::bail!(
            "unable to determine HOME; please set GRADLE_WRAPPER_HOME or HOME environment variable"
        );
    }
    Ok(cu::path!((home.into()) / ".gradle-wrapper"))
}

////////////////////// Other Operations

impl Environment {
    /// Read `gradle-wrapper.properties` and extract the Gradle version.
    ///
    /// falls back to GRADLE_VERSION environment variable
    pub fn read_version_from_wrapper_properties(&self) -> cu::Result<ValidatedVersion> {
        // prefer gradle/wrapper/.version - not a standard but used by this tool
        // to skip checking gradle-wrapper.properties entirely
        let version_path = self.project.join("gradle/wrapper/.version");
        if let Ok(v) = cu::fs::read_string(&version_path) {
            let v = cu::check!(
                ValidatedVersion::try_new(v.trim().to_string()),
                "failed to read version from gradle/wrapper/.version"
            )?;
            return Ok(v);
        }
        let props_path = self
            .project
            .join("gradle/wrapper/gradle-wrapper.properties");
        let text = cu::fs::read_string(props_path)?;
        #[cu::context(
            "failed to extract version from gradle-wrapper.properties; this is unexpected - please proceed with caution with this project if it's untrusted; to unblock, set GRADLE_VERSION environment variable to the version you wish to use, the rerun the program"
        )]
        fn extract_impl(text: &str) -> cu::Result<ValidatedVersion> {
            let props = properties::parse(text);
            let url = cu::check!(
                props.get("distributionUrl"),
                "no distributionUrl in gradle-wrapper.properties"
            )?;
            let version = ValidatedVersion::try_from_url(url)?;
            cu::debug!("read gradle version from gradle-wrapper.properties: {version}");
            Ok(version)
        }
        match extract_impl(&text) {
            Ok(x) => Ok(x),
            Err(e) => {
                let version_env = cu::env_var("GRADLE_VERSION").unwrap_or_default();
                if version_env.is_empty() {
                    return Err(e);
                }
                cu::warn!("failed to read Gradle version from gradle-wrapper.properties: {e:?}");
                cu::warn!("fallback to GRADLE_VERSION environment variable: {version_env}");
                cu::check!(
                    ValidatedVersion::try_new(version_env),
                    "invalid gradle version from GRADLE_VERSION environment variable"
                )
            }
        }
    }

    /// Hash the existing `gradle-wrapper.jar` in the project
    pub fn read_existing_wrapper_sha256(&self) -> cu::Result<String> {
        let jar_path = self.project.join("gradle/wrapper/gradle-wrapper.jar");
        checksum::sha256_file(&jar_path)
    }

    /// Hash the cached known good `gradle-wrapper.jar`
    pub fn read_cached_known_good_wrapper_sha256(
        &self,
        version: &ValidatedVersion,
    ) -> cu::Result<String> {
        let known_good = self.get_known_good(version)?;
        checksum::sha256_file(&known_good.jar)
    }

    pub fn get_known_good(&self, version: &ValidatedVersion) -> cu::Result<KnownGood> {
        cu::check!(
            KnownGood::find(&self.wrapper_home, version),
            "{version} not found in cache"
        )
    }

    pub fn generate_known_good_thread(
        self,
        version: &ValidatedVersion,
    ) -> JoinHandle<cu::Result<()>> {
        let version = version.clone();
        thread::spawn(move || {
            known_good::generate(&version, &self.project, &self.java, &self.wrapper_home)
        })
    }

    pub fn run_project_wrapper_jar(
        &self,
        known_good: &KnownGood,
        sync_jar: bool,
    ) -> cu::Result<ExitCode> {
        let dir = self.project.join("gradle/wrapper");
        let jar = dir.join("gradle-wrapper.jar");
        cu::fs::make_dir(&dir)?;
        if sync_jar {
            cu::debug!("replacing the project's gradle-wrapper.jar with the known-good copy");
            cu::fs::copy(&known_good.jar, &jar)?;
        }

        // The properties file also needs to be checked and replaced with the known-good,
        // even if the jar is good, because the distributionUrl inside could be malicious
        let properties = dir.join("gradle-wrapper.properties");
        let expected_hash = checksum::sha256_file(&known_good.properties)?;
        let sync_properties = match checksum::sha256_file(&properties) {
            Err(e) => {
                cu::debug!("cannot hash the project's gradle-wrapper.properties: {e:?}");
                true
            }
            Ok(actual_hash) => checksum::verify(&expected_hash, &actual_hash).is_err(),
        };
        if sync_properties {
            cu::debug!(
                "replacing the project's gradle-wrapper.properties with the known-good copy"
            );
            cu::fs::copy(&known_good.properties, properties)?;
        }

        // Launch via -classpath and an explicit main class rather than `java -jar`.
        //
        // Modern wrapper jars carry `Main-Class` in their manifest, so `-jar` works
        // and that is what the 9.x script uses. Older ones do not: Gradle 2.0's jar
        // has no Main-Class at all, and `-jar` fails with "no main manifest
        // attribute". Every gradlew script before 9.x therefore used -classpath with
        // the main class named explicitly.
        //
        // `org.gradle.wrapper.GradleWrapperMain` is present in both, so this one
        // form works across the whole 2.0-to-9.x range.
        let mut cmd = std::process::Command::new(&self.java);
        cmd.current_dir(&self.project)
            .args(exec::jvm_opts())
            .arg("-Dorg.gradle.appname=gradlew")
            .arg("-classpath")
            .arg(&jar)
            .arg("org.gradle.wrapper.GradleWrapperMain")
            .args(std::env::args_os().skip(1));

        cu::debug!("exec: {cmd:?}");
        Ok(exec::exec_replace(cmd))
    }
}
