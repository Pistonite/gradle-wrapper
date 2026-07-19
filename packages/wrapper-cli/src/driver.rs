use std::process::ExitCode;
use std::thread::JoinHandle;

use cu::pre::*;

use crate::environment::Environment;
use crate::known_good::KnownGood;
use crate::properties::ValidatedVersion;
use crate::{checksum, gradle_dist};

/// Entry-point of the gradlew script
pub fn run() -> cu::Result<ExitCode> {
    let environment = Environment::setup()?;
    let version = environment.read_version_from_wrapper_properties()?;

    // source of truth - published checksum by Gradle (but might fail)
    let checksum_official = gradle_dist::fetch_wrapper_sha256(&version).ok();
    // local source - in the project
    let checksum_existing = environment.read_existing_wrapper_sha256();
    // local source - known good
    let checksum_known_good = environment.read_cached_known_good_wrapper_sha256(&version);
    let known_good_handle = if let Err(e) = checksum_known_good.as_ref() {
        cu::warn!("wrapper version {version} does not have a local known-good: {e:?}");
        let handle = environment.clone().generate_known_good_thread(&version);
        Some(handle)
    } else {
        None
    };

    let need_to_sync_known_good = match checksum_existing {
        Err(_) => {
            // did not get a local checksum; syncing known-good is required
            let (known_good, _) = join_known_good_handle(
                &environment,
                &version,
                checksum_official,
                checksum_known_good.ok(),
                known_good_handle,
            )?;
            // verified known good to be copied to the project
            Some(known_good)
        }
        Ok(checksum_existing) => {
            let (known_good, checksum_known_good) = join_known_good_handle(
                &environment,
                &version,
                checksum_official.clone(),
                checksum_known_good.ok(),
                known_good_handle,
            )?;
            match checksum_official {
                Some(checksum_official) => {
                    match checksum::verify(&checksum_official, &checksum_existing) {
                        Err(e) => {
                            cu::debug!(
                                "current gradle-wrapper.jar does not match the official checksum: {e:?}"
                            );
                            // verified known good to be copied to the project
                            Some(known_good)
                        }
                        Ok(_) => {
                            cu::debug!("verified existing gradle-wrapper.jar against official checksum");
                            // can use existing
                            None
                        }
                    }
                }
                None => {
                    // no official checksum, verify against known-good
                    match checksum::verify(&checksum_known_good, &checksum_existing) {
                        Err(e) => {
                            cu::debug!(
                                "current gradle-wrapper.jar does not match the known-good checksum: {e:?}"
                            );
                            // verified known good to be copied to the project
                            Some(known_good)
                        }
                        Ok(_) => {
                            cu::debug!("verified existing gradle-wrapper.jar against known-good checksum");
                            // can use existing
                            None
                        }
                    }
                }
            }
        }
    };

    environment.run_project_wrapper_jar(need_to_sync_known_good.as_ref())
}

fn join_known_good_handle(
    environment: &Environment,
    version: &ValidatedVersion,
    checksum_official: Option<String>,
    checksum_known_good: Option<String>,
    mut known_good_handle: Option<JoinHandle<cu::Result<()>>>,
) -> cu::Result<(KnownGood, String)> {
    if let Some(x) = known_good_handle.take() {
        match x.join() {
            Err(_) => {
                cu::bail!("subthread panicked!");
            }
            Ok(x) => x?,
        }
    }
    let (known_good, checksum_known_good) = match checksum_known_good {
        None => {
            let known_good = cu::check!(
                environment.get_known_good(&version),
                "failed to find known good after generating"
            )?;
            let checksum_known_good = checksum::sha256_file(&known_good.jar)?;
            (known_good, checksum_known_good)
        }
        Some(checksum_known_good) => (environment.get_known_good(&version)?, checksum_known_good),
    };
    // time-of-check time-of-use attack: re-validate it before using if we have an
    // official checksum, for max security
    if let Some(checksum_official) = checksum_official {
        if let Err(e) = checksum::verify(&checksum_official, &checksum_known_good) {
            let _ = cu::fs::remove(known_good.jar);
            cu::rethrow!(
                e,
                "failed to verify (no longer) known-good wrapper with official checksum"
            );
        }
        cu::debug!("verified known-good gradle-wrapper.jar against official checksum");
    }
    Ok((known_good, checksum_known_good))
}
