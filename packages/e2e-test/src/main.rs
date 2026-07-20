//! End-to-end fixture test suite
//!
//! Usage:
//!   cargo run -p e2e-test              # everything
//!   cargo run -p e2e-test -- 2.0 9.6.1 # only these Gradle versions
//!   cargo run -p e2e-test -- behaviour # only the behaviour tests

use std::path::{Path, PathBuf};

use cu::pre::*;

use crate::helper::VERBOSE;

type TestObject = (String, Box<dyn FnOnce() -> cu::Result<()>>);
fn tests(root: &Path, gradlew_bin: &Path) -> Vec<TestObject> {
    let mut out = Vec::<TestObject>::new();
    macro_rules! add_fixture {
        ($gradle_version:literal, $jdk_version:expr) => {{
            let gradle_version = $gradle_version.to_string();
            let jdk_version = $jdk_version.to_string();
            let root = root.to_path_buf();
            let gradlew_bin = gradlew_bin.to_path_buf();
            out.push((
                format!("fixture_{gradle_version}"),
                Box::new(move || {
                    fixture::run_test(&root, &gradlew_bin, &gradle_version, &jdk_version)?;
                    cu::Ok(())
                }),
            ))
        }};
    }
    macro_rules! add_behavior {
        ($test_name:ident) => {{
            let root = root.to_path_buf();
            let gradlew_bin = gradlew_bin.to_path_buf();
            let test_name = concat!("behavior_", stringify!($test_name)).to_string();
            out.push((
                test_name,
                Box::new(move || {
                    behavior::$test_name(&root, &gradlew_bin)?;
                    cu::Ok(())
                }),
            ))
        }};
    }

    let jdk_8 = if cfg!(target_os = "macos") {
        // openjdk 8 doesn't exist for mac
        "temurin@8"
    } else {
        "openjdk@8"
    };
    // (Gradle version, JDK). Java 8 covers Gradle 2.0–8.14.x, so one JDK carries
    // almost the whole range; see https://docs.gradle.org/current/userguide/compatibility.html
    add_fixture!("2.0", jdk_8); // oldest version to support jdk 8
    add_fixture!("2.14.1", jdk_8); // last 2.x
    add_fixture!("3.5.1", jdk_8); // last 3.x - no published wrapper checksum
    add_fixture!("4.10.3", jdk_8); // last 4.x
    add_fixture!("5.6.4", jdk_8); // last 5.x
    add_fixture!("6.9.4", jdk_8); // last 6.x
    add_fixture!("7.6.4", jdk_8); // last 7.x
    add_fixture!("8.14.3", jdk_8); // last 8.x - Java 8's upper bound
    add_fixture!("9.0.0", "openjdk@21"); // first 9.x, gradle-gradle-cli-main layout
    add_fixture!("9.6.1", "openjdk@25"); // current release on the latest LTS

    add_behavior!(tamper);
    add_behavior!(all_downgrade);
    add_behavior!(bad_version);
    add_behavior!(exit_code);
    add_behavior!(java_home_invalid);
    add_behavior!(version_file_preferred);

    out
}

#[derive(clap::Parser, AsRef)]
struct Args {
    test_selections: Vec<String>,

    /// Execute a clean run
    #[clap(short, long)]
    clean: bool,
    #[clap(flatten)]
    common: cu::cli::Flags,
}

struct LogConfig;
impl cu::cli::LogConfig for LogConfig {
    fn process(&self, record: &cu::lv::LogRecord) -> (cu::lv::Lv, bool) {
        if let Some(x) = record.module_path() {
            if x.starts_with("ureq") || x.starts_with("rustls") {
                return (cu::lv::T, true);
            }
        }
        cu::cli::DefaultLogConfig.process(record)
    }
}

#[cu::cli(log_config = |_| LogConfig)]
fn main(args: Args) -> cu::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent_abs_times(2)?;

    // JABBA_HOME must be set before any jabba call, or JDKs are installed
    // system-wide instead of project-local. Set once, here, so no call site can
    // forget it.
    let jabba_home = root.join(".jabba");
    unsafe { std::env::set_var("JABBA_HOME", &jabba_home) };
    cu::debug!("JABBA_HOME = {}", jabba_home.display());

    cu::info!("cleaning previous run output");
    cu::fs::rec_remove(root.join("target/e2e"))?;
    cu::fs::rec_remove(root.join(".gradle-wrapper/work"))?;
    if args.clean {
        cu::hint!("cleaning previous run cache");
        cu::fs::rec_remove(&jabba_home)?;
        cu::fs::rec_remove(root.join(".gradle-wrapper"))?;
        cu::fs::rec_remove(root.join(".gradle-test-home"))?;
    }

    // build the program
    let gradlew = {
        let cargo = cu::env_var("CARGO")?;
        let (child, bar) = PathBuf::from(cargo)
            .command()
            .current_dir(&root)
            .args(["build", "--release", "--bin", "gradlew"])
            .preset(cu::pio::cargo("building gradlew binary"))
            .spawn()?;
        child.wait_nz()?;
        bar.done();
        let gradlew = cu::path!(
            &root
                / "target"
                / "release"
                / if cfg!(windows) {
                    "gradlew.exe"
                } else {
                    "gradlew"
                }
        );
        if !gradlew.is_file() {
            cu::bail!(
                "failed to find gradlew binary after building: '{}'",
                gradlew.display()
            );
        }
        gradlew
    };

    let mut selected_tests = vec![];
    for test in tests(&root, &gradlew) {
        if !args.test_selections.is_empty() {
            if !args.test_selections.iter().any(|x| test.0.contains(x)) {
                continue;
            }
        }
        selected_tests.push(test);
    }

    let total = selected_tests.len();
    if total == 0 {
        cu::bail!("no tests matched the test selections");
    }
    if total == 1 {
        VERBOSE.store(true, std::sync::atomic::Ordering::Relaxed);
        let (test_name, test_fn) = selected_tests.into_iter().next().unwrap();
        cu::hint!("running single test: {test_name}");
        cu::check!(test_fn(), "test failed: {test_name}")?;
        cu::info!("test passed!");
    } else {
        let bar = cu::progress("e2e tests")
            .total(total)
            .keep(false)
            .percentage(false)
            .spawn();
        let mut failed = 0;

        for (test_name, test_fn) in selected_tests {
            log!("===== >> {test_name} << =====");
            if failed == 0 {
                cu::progress!(bar, "running: {test_name}");
            } else {
                cu::progress!(bar, "running: {test_name}, {failed} failed");
            }
            match test_fn() {
                Err(e) => {
                    cu::error!("FAIL: {test_name}\nerror: {e:?}");
                    failed += 1;
                }
                Ok(_) => {
                    cu::info!("PASS: {test_name}");
                }
            }
            cu::progress!(bar += 1);
        }

        bar.done();
        let passed = total - failed;
        cu::info!("result: {passed}/{total} passed, {failed} failures");
        if failed != 0 {
            cu::bail!("there were test failures");
        }
        cu::info!("all tests passed!");
    }

    Ok(())
}
mod behavior;
mod fixture;
mod helper;
