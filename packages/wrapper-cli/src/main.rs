//! A trustworthy replacement for a Java project's `gradlew` script.
//!
//! A project's `gradle/wrapper/gradle-wrapper.jar` is a binary blob that arrived
//! with the repo. It should NOT be trusted blindly, but Gradle pushes the responsibility
//! to each developer to ensure the jar is safe for every project. This is STUPID.
//!
//! This program replaces the `gradlew` script: it does exactly what the programmer should do
//! manually: ensures the jar's checksum is a known-good checksum, replace it with a known-good
//! jar (verified and cached system-wide) if not, and launches it.

use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    cu::cli::init_options(
        cu::lv::Color::Auto,
        cu::lv::Print::Normal,
        None,
        Arc::new(LogConfig),
    );
    match driver::run() {
        Ok(code) => code,
        Err(e) => {
            cu::error!("{e:?}\n");
            cu::hint!(
                "if the error above is related to generating or verifying gradle-wrapper.jar\n\
                consider using './gradlew' directly ONLY IF YOU TRUST THE PROJECT."
            );
            ExitCode::FAILURE
        }
    }
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

mod checksum;
mod driver;
mod environment;
mod exec;
mod gradle_dist;
mod known_good;
mod properties;
