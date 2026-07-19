use std::path::{Path, PathBuf};

use cu::pre::*;
use ureq::{Body, http::Response};
use zip::ZipArchive;

use crate::checksum;
use crate::properties::ValidatedVersion;

/// Where Gradle publishes distributions and their checksums.
static SERVICES_BASE: &str = "https://services.gradle.org/distributions";

pub fn fetch_wrapper_sha256(version: &ValidatedVersion) -> cu::Result<String> {
    fetch_text(&format!(
        "{SERVICES_BASE}/gradle-{version}-wrapper.jar.sha256"
    ))
}

pub fn fetch_bin_zip(version: &ValidatedVersion, out_path: &Path) -> cu::Result<()> {
    let zip_url = format!("{SERVICES_BASE}/gradle-{version}-bin.zip");
    {
        let mut resp = fetch(&zip_url)?;
        let mut writer = cu::fs::buf_writer(out_path)?;
        let mut reader = resp.body_mut().as_reader();
        cu::check!(
            std::io::copy(&mut reader, &mut writer),
            "failed to write to output zip file"
        )?;
    }
    cu::check!(
        verify_bin_zip(out_path, zip_url),
        "failed to verify checksum for downloaded gradle version {version}"
    )?;
    Ok(())
}

fn verify_bin_zip(out_path: &Path, zip_url: String) -> cu::Result<()> {
    let hash_url = {
        let mut x = zip_url;
        x.push_str(".sha256");
        x
    };
    let expected_hash = fetch_text(&hash_url)?;
    let actual_hash = checksum::sha256_file(out_path)?;
    checksum::verify(&expected_hash, &actual_hash)
}

/// Unzip the downloaded distribution, return the distribution root path
pub fn unzip(zip_path: &Path, version: &ValidatedVersion, out_path: &Path) -> cu::Result<PathBuf> {
    cu::fs::make_dir_empty(out_path)?;
    let file = cu::fs::reader(zip_path)?;
    let mut zip = cu::check!(
        ZipArchive::new(file),
        "failed to open downloaded gradle zip"
    )?;
    cu::check!(
        zip.extract(out_path),
        "failed to extract downloaded gradle zip"
    )?;
    let dist_name = format!("gradle-{version}");
    let dist = out_path.join(dist_name);
    if !dist.is_dir() {
        cu::bail!(
            "distribution did not contain the expected directory '{}'",
            dist.display()
        );
    }
    Ok(dist)
}

/// Find the jar that launches Gradle inside an unpacked distribution.
///
/// 9.x ships `gradle-gradle-cli-main-<v>.jar`; pre-9.0 ships
/// `gradle-launcher-<v>.jar`. 9.6.1 ships *both*, so this is a preference order
/// rather than an either/or.
pub fn find_launcher_jar(dist: &Path) -> cu::Result<PathBuf> {
    let lib = dist.join("lib");
    let mut found = None;
    for entry in cu::fs::read_dir(&lib)? {
        let entry = entry?;
        let path = entry.path();
        // lossy is fine since we are matching based on prefix
        let Some(file_name) = path.file_name() else {
            continue;
        };
        let file_name = file_name.to_string_lossy();
        if !file_name.ends_with(".jar") {
            continue;
        }
        if file_name.starts_with("gradle-gradle-cli-main-") {
            found = Some(path);
            break;
        }
        // prefer cli-main but also use launcher for older versions
        if file_name.starts_with("gradle-launcher-") {
            found = Some(path);
        }
    }
    if let Some(jar) = found {
        cu::debug!("found gradle main jar: {}", jar.display());
        return Ok(jar);
    }
    cu::bail!(
        "no launcher jar (gradle-gradle-cli-main-*.jar or gradle-launcher-*.jar) in '{}'",
        lib.display()
    )
}

/// Fetch a small text body (a `.sha256` file) and return it *raw* (untrimmed).
fn fetch_text(url: &str) -> cu::Result<String> {
    let mut resp = fetch(url)?;
    let body = cu::check!(
        resp.body_mut().read_to_string(),
        "GET failed to read body: {url}"
    )?;
    Ok(body)
}
fn fetch(url: &str) -> cu::Result<Response<Body>> {
    cu::debug!("GET {url}");
    let resp = cu::check!(ureq::get(url).call(), "GET failed: {url}")?;
    let status = resp.status();
    if !status.is_success() {
        cu::bail!("GET failed with status {status}: {url}");
    }
    Ok(resp)
}
