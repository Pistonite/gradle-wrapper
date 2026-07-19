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

fn main() -> ExitCode {
    cu::cli::level("");
    match driver::run() {
        Ok(code) => code,
        Err(e) => {
            cu::error!("{e:?}");
            cu::hint!("if you trust the project, consider using './gradlew' directly.");
            ExitCode::FAILURE
        }
    }
}

mod checksum;
mod driver;
mod environment;
mod exec;
mod gradle_dist;
mod known_good;
mod properties;
