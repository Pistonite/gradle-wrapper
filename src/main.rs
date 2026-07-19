//! A trustworthy replacement for a Java project's `gradlew` script.
//!
//! A project's `gradle/wrapper/gradle-wrapper.jar` is a binary blob that arrived
//! with the repo, and every repo is untrusted. This program never runs it as
//! found. It reads only the Gradle *version* out of `gradle-wrapper.properties`,
//! downloads the official distribution from services.gradle.org over a URL it
//! reconstructs itself, verifies it against Gradle's published checksum, and uses
//! it to generate a fresh wrapper jar. The generated jar is checked against
//! Gradle's published wrapper checksum, cached as known-good, copied over the
//! repo's copy, and only then run.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

mod downloader;
mod exec_replace;
mod properties;

/// Where Gradle publishes distributions and their checksums.
const SERVICES_BASE: &str = "https://services.gradle.org/distributions";

fn main() -> std::process::ExitCode {
    init_logging();

    match run() {
        Ok(code) => code,
        Err(e) => {
            // Match the shell script's `die`: blank line, message, blank line.
            eprintln!("\n{e:?}\n");
            std::process::ExitCode::from(1)
        }
    }
}

fn run() -> Result<std::process::ExitCode> {
    let project = find_project_dir()?;
    log::debug!("project dir: {}", project.display());

    let java = find_java()?;
    log::debug!("java: {}", java.display());

    let props_path = project.join("gradle/wrapper/gradle-wrapper.properties");
    let version = properties::read_version(&props_path)?;
    log::debug!("gradle version: {version}");

    // Fetch (generating if necessary) a wrapper jar we actually trust.
    let known_good = ensure_known_good(&version, &java)?;

    // Put it in the project, but only if what's there differs.
    sync_into_project(&project, &known_good)?;

    // Finally run it, exactly as the shell script would.
    let jar = project.join("gradle/wrapper/gradle-wrapper.jar");
    let app_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "gradlew".to_string());

    let mut cmd = std::process::Command::new(&java);
    cmd.args(jvm_opts())
        .arg(format!("-Dorg.gradle.appname={app_name}"))
        .arg("-jar")
        .arg(&jar)
        .args(std::env::args_os().skip(1))
        .current_dir(std::env::current_dir()?);

    log::debug!("exec: {cmd:?}");
    Ok(exec_replace::exec_replace(cmd))
}

/// Paths to the trusted, cached wrapper files for a given Gradle version.
struct KnownGood {
    jar: PathBuf,
    properties: PathBuf,
}

/// The managed directory, `$GRADLE_WRAPPER_HOME` or `~/.gradle-wrapper`.
fn wrapper_home() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("GRADLE_WRAPPER_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| anyhow!("neither GRADLE_WRAPPER_HOME nor HOME is set"))?;
    Ok(PathBuf::from(home).join(".gradle-wrapper"))
}

/// Return the known-good wrapper files for `version`, generating them if this is
/// the first time we've seen that version.
fn ensure_known_good(version: &str, java: &Path) -> Result<KnownGood> {
    let dir = wrapper_home()?.join("known-good");
    let known = KnownGood {
        jar: dir.join(format!("gradle-wrapper-{version}.jar")),
        properties: dir.join(format!("gradle-wrapper-{version}.properties")),
    };

    if known.jar.is_file() && known.properties.is_file() {
        log::debug!("known-good cache hit for {version}");
        return Ok(known);
    }

    log::debug!("known-good cache miss for {version}, generating");
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    generate(version, java, &known)?;
    Ok(known)
}

/// Download the distribution and use it to generate a wrapper jar.
fn generate(version: &str, java: &Path, known: &KnownGood) -> Result<()> {
    let work = wrapper_home()?.join("work").join(work_id(version));
    std::fs::create_dir_all(&work).with_context(|| format!("cannot create {}", work.display()))?;

    let result = generate_in(&work, version, java, known);

    match &result {
        Ok(()) => {
            // Only clean up on success; on failure the work dir is evidence.
            if let Err(e) = std::fs::remove_dir_all(&work) {
                log::debug!("could not remove work dir {}: {e}", work.display());
            }
        }
        Err(_) => {
            eprintln!(
                "gradlew: work directory left for inspection: {}",
                work.display()
            );
        }
    }
    result
}

fn generate_in(work: &Path, version: &str, java: &Path, known: &KnownGood) -> Result<()> {
    // 1. Download and verify the distribution.
    let zip_url = format!("{SERVICES_BASE}/gradle-{version}-bin.zip");
    let zip_path = work.join("gradle-bin.zip");
    eprintln!("gradlew: downloading {zip_url}");
    let actual = downloader::download(&zip_url, &zip_path)?;
    let expected = downloader::fetch_text(&format!("{zip_url}.sha256"))?;
    downloader::verify(&format!("gradle-{version}-bin.zip"), &actual, &expected)?;

    // 2. Unzip.
    let unpacked = work.join("gradle-bin");
    downloader::unzip(&zip_path, &unpacked)?;
    let dist = unpacked.join(format!("gradle-{version}"));
    if !dist.is_dir() {
        bail!(
            "distribution did not contain the expected directory {}",
            dist.display()
        );
    }

    // 3. Stub project: an empty build.gradle is the minimum for Gradle to
    //    recognise a project it can run the `wrapper` task in.
    let stub = work.join("stub-project");
    std::fs::create_dir_all(&stub)?;
    std::fs::write(stub.join("build.gradle"), b"")?;

    // 4. Run the distribution the way its own bin/gradle script does. No
    //    -javaagent: measured to produce a byte-identical jar (see LOG.md).
    let launcher = find_launcher_jar(&dist)?;
    let mut cmd = std::process::Command::new(java);
    cmd.args(jvm_opts())
        .arg("-Dorg.gradle.appname=gradle")
        .arg("-jar")
        .arg(&launcher)
        .arg("wrapper")
        .arg("--gradle-version")
        .arg(version)
        // Don't leave a daemon running out of a directory we're about to delete.
        .arg("--no-daemon")
        .current_dir(&stub);

    log::debug!("generating: {cmd:?}");
    eprintln!("gradlew: generating a trusted gradle-wrapper.jar for {version}");

    // Generation is incidental to whatever the user actually asked for, so its
    // output must not land on stdout — otherwise a cold-cache `gradlew properties
    // | grep x` would have Gradle's build log spliced into the piped result.
    // Redirect it to stderr, keeping it visible without corrupting the pipeline.
    cmd.stdout(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot run {}", java.display()))?;
    if let Some(mut out) = child.stdout.take() {
        std::io::copy(&mut out, &mut std::io::stderr()).ok();
    }
    let status = child.wait().context("waiting for the gradle wrapper task")?;
    if !status.success() {
        bail!("gradle `wrapper` task failed with {status}");
    }

    // 5. Verify the generated jar against Gradle's *published* wrapper checksum.
    //    This is what makes the jar trustworthy: not "we built it", but "it is
    //    provably identical to the one Gradle publishes".
    let generated_jar = stub.join("gradle/wrapper/gradle-wrapper.jar");
    let generated_props = stub.join("gradle/wrapper/gradle-wrapper.properties");
    if !generated_jar.is_file() {
        bail!(
            "gradle `wrapper` task did not produce {}",
            generated_jar.display()
        );
    }
    let jar_hash = downloader::sha256_file(&generated_jar)?;
    let published = downloader::fetch_text(&format!(
        "{SERVICES_BASE}/gradle-{version}-wrapper.jar.sha256"
    ))?;
    downloader::verify("generated gradle-wrapper.jar", &jar_hash, &published)?;

    // 6. Promote into the cache. Write-then-rename so a half-written jar is
    //    never visible to a concurrent invocation.
    promote(&generated_jar, &known.jar)?;
    promote(&generated_props, &known.properties)?;

    Ok(())
}

/// Copy `src` to `dest` atomically, via a temporary file in the same directory.
fn promote(src: &Path, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension(format!("tmp{}", std::process::id()));
    std::fs::copy(src, &tmp)
        .with_context(|| format!("cannot copy {} to {}", src.display(), tmp.display()))?;
    std::fs::rename(&tmp, dest)
        .with_context(|| format!("cannot rename {} to {}", tmp.display(), dest.display()))?;
    log::debug!("promoted {} -> {}", src.display(), dest.display());
    Ok(())
}

/// A work directory name unique to this invocation.
fn work_id(version: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    version.hash(&mut h);
    std::process::id().hash(&mut h);
    std::env::current_dir().ok().hash(&mut h);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Find the jar that launches Gradle inside an unpacked distribution.
///
/// 9.x ships `gradle-gradle-cli-main-<v>.jar`; pre-9.0 ships
/// `gradle-launcher-<v>.jar`. 9.6.1 ships *both*, so this is a preference order
/// rather than an either/or.
fn find_launcher_jar(dist: &Path) -> Result<PathBuf> {
    let lib = dist.join("lib");
    for prefix in ["gradle-gradle-cli-main-", "gradle-launcher-"] {
        let mut found: Vec<PathBuf> = std::fs::read_dir(&lib)
            .with_context(|| format!("cannot read {}", lib.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(prefix) && n.ends_with(".jar"))
            })
            .collect();
        found.sort();
        if let Some(jar) = found.pop() {
            log::debug!("launcher jar: {}", jar.display());
            return Ok(jar);
        }
    }
    bail!(
        "no launcher jar (gradle-gradle-cli-main-*.jar or gradle-launcher-*.jar) in {}",
        lib.display()
    )
}

/// Copy the known-good files into the project, but only where they differ.
///
/// Skipping identical files keeps a clean repo clean — no spurious git diff on
/// every invocation.
///
/// Only the jar and properties are ever written. The `wrapper` task also emits
/// `gradlew` and `gradlew.bat`, and copying those would overwrite *this binary*
/// with a shell script.
fn sync_into_project(project: &Path, known: &KnownGood) -> Result<()> {
    let dir = project.join("gradle/wrapper");
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;

    for (src, name) in [
        (&known.jar, "gradle-wrapper.jar"),
        (&known.properties, "gradle-wrapper.properties"),
    ] {
        let dest = dir.join(name);
        if dest.is_file()
            && let (Ok(a), Ok(b)) = (downloader::sha256_file(&dest), downloader::sha256_file(src))
            && a == b
        {
            log::debug!("{name} already matches known-good, not copying");
            continue;
        }
        log::debug!("copying known-good {name} into project");
        eprintln!("gradlew: replacing {name} with the known-good copy");
        std::fs::copy(src, &dest)
            .with_context(|| format!("cannot copy {} to {}", src.display(), dest.display()))?;
    }
    Ok(())
}

/// Logging is off unless `RUST_LOG` is set.
///
/// A bare level (`RUST_LOG=debug`) is scoped to this crate. Without that, the
/// TLS and HTTP layers bury our own output in handshake chatter — the plain
/// `RUST_LOG=debug` the README documents has to be the useful one. Anything more
/// specific (`RUST_LOG=gradlew=trace,ureq=debug`) is passed through untouched.
fn init_logging() {
    const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

    let filter = match std::env::var("RUST_LOG") {
        Ok(v) if LEVELS.contains(&v.trim().to_ascii_lowercase().as_str()) => {
            format!("{}={}", env!("CARGO_CRATE_NAME"), v.trim())
        }
        Ok(v) => v,
        Err(_) => "off".to_string(),
    };

    env_logger::Builder::new()
        .parse_filters(&filter)
        .format_timestamp(None)
        .init();
}

/// Find the `java` executable, mirroring the shell script's logic exactly,
/// including its error messages.
fn find_java() -> Result<PathBuf> {
    let java_home = std::env::var("JAVA_HOME").unwrap_or_default();
    // The .bat strips quotes from JAVA_HOME; harmless to do everywhere.
    let java_home = java_home.trim().trim_matches('"');

    if !java_home.is_empty() {
        let home = Path::new(java_home);

        // IBM's JDK on AIX uses strange locations for the executables.
        let aix = home.join("jre/sh/java");
        let cmd = if cfg!(unix) && is_executable(&aix) {
            aix
        } else if cfg!(windows) {
            home.join("bin/java.exe")
        } else {
            home.join("bin/java")
        };

        // Windows checks existence; Unix checks executability.
        let ok = if cfg!(windows) {
            cmd.is_file()
        } else {
            is_executable(&cmd)
        };
        if !ok {
            bail!(
                "ERROR: JAVA_HOME is set to an invalid directory: {java_home}\n\n\
                 Please set the JAVA_HOME variable in your environment to match the\n\
                 location of your Java installation."
            );
        }
        return Ok(cmd);
    }

    let exe = if cfg!(windows) { "java.exe" } else { "java" };
    which(exe).ok_or_else(|| {
        anyhow!(
            "ERROR: JAVA_HOME is not set and no 'java' command could be found in your PATH.\n\n\
             Please set the JAVA_HOME variable in your environment to match the\n\
             location of your Java installation."
        )
    })
}

/// Search `PATH` for an executable.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            if cfg!(windows) {
                candidate.is_file()
            } else {
                is_executable(candidate)
            }
        })
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

/// Assemble JVM options the way the scripts do: `DEFAULT_JVM_OPTS`, then
/// `JAVA_OPTS`, then `GRADLE_OPTS`. Order matters — later options win in the JVM,
/// so `GRADLE_OPTS` must be able to override the defaults.
fn jvm_opts() -> Vec<String> {
    let mut opts = vec!["-Xmx64m".to_string(), "-Xms64m".to_string()];
    for var in ["JAVA_OPTS", "GRADLE_OPTS"] {
        if let Ok(value) = std::env::var(var) {
            opts.extend(split_args(&value));
        }
    }
    opts
}

/// Split a string the way the scripts' `xargs -n1` does: on whitespace, honouring
/// single and double quotes and backslash escapes, with the quotes removed.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();

    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    // Inside double quotes a backslash still escapes.
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                        started = true;
                    }
                }
                c if c.is_whitespace() => {
                    if started {
                        out.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                c => {
                    cur.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Locate the project root.
///
/// The shell script resolves `$0` through symlinks and takes its directory. We do
/// the same, then fall back to walking up from the CWD, which covers the case
/// where the binary lives on `PATH` rather than in the project.
fn find_project_dir() -> Result<PathBuf> {
    let marker = Path::new("gradle/wrapper/gradle-wrapper.properties");

    // 1. Alongside the binary, mirroring the script's APP_HOME.
    if let Ok(exe) = std::env::current_exe()
        && let Ok(exe) = exe.canonicalize()
        && let Some(dir) = exe.parent()
        && dir.join(marker).is_file()
    {
        return Ok(dir.to_path_buf());
    }

    // 2. Walk up from the CWD.
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    let mut dir = cwd.as_path();
    loop {
        if dir.join(marker).is_file() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    Err(anyhow!(
        "ERROR: no Gradle project found.\n\n\
         Looked for gradle/wrapper/gradle-wrapper.properties next to this binary \
         and in every directory from {} upwards.",
        cwd.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_args_basic() {
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("   "), Vec::<String>::new());
        assert_eq!(split_args("-Xmx64m -Xms64m"), ["-Xmx64m", "-Xms64m"]);
        // Runs of whitespace collapse, like xargs.
        assert_eq!(split_args("  a \t b \n c "), ["a", "b", "c"]);
    }

    #[test]
    fn split_args_quotes_are_removed() {
        // This is the shape DEFAULT_JVM_OPTS actually has in the script.
        assert_eq!(split_args(r#""-Xmx64m" "-Xms64m""#), ["-Xmx64m", "-Xms64m"]);
        assert_eq!(split_args(r#"-Dfoo="a b""#), ["-Dfoo=a b"]);
        assert_eq!(split_args("-Dfoo='a b'"), ["-Dfoo=a b"]);
        // A quoted empty string is still an argument.
        assert_eq!(split_args(r#"a "" b"#), ["a", "", "b"]);
    }

    #[test]
    fn split_args_escapes() {
        assert_eq!(split_args(r"a\ b"), ["a b"]);
        assert_eq!(split_args(r"-Dp=C:\\tmp"), [r"-Dp=C:\tmp"]);
        // Single quotes are literal; double quotes still honour backslash.
        assert_eq!(split_args(r#""a\"b""#), [r#"a"b"#]);
    }

    #[test]
    fn jvm_opts_order_lets_gradle_opts_win() {
        // Order is what matters here: later options override earlier ones in the
        // JVM, so GRADLE_OPTS must come last. Serialised because these env vars
        // are process-global.
        unsafe {
            std::env::set_var("JAVA_OPTS", "-Da=1");
            std::env::set_var("GRADLE_OPTS", "-Xmx512m");
        }
        let opts = jvm_opts();
        unsafe {
            std::env::remove_var("JAVA_OPTS");
            std::env::remove_var("GRADLE_OPTS");
        }
        assert_eq!(opts, ["-Xmx64m", "-Xms64m", "-Da=1", "-Xmx512m"]);
    }
}
