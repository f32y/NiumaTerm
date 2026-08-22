//! Fetching a published package and unpacking it where the swap can reach it.

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use sha2::{Digest as _, Sha256};
use tracing::warn;

use crate::update::InstallError;
use crate::update::releases::{Asset, DOWNLOAD_URL_PREFIX, Release, user_agent};

/// Long enough for a package on a slow connection, short enough that a stalled
/// transfer does not leave the About page reporting an install forever.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A package is a few megabytes. The cap is what keeps a response that is not
/// one from being written to disk in full before anything notices.
const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Unpack `release`'s package into `staging`, and answer with the directory the
/// files ended up in.
///
/// The checksum published beside the package is what distinguishes a truncated
/// or corrupted download from a complete one before any of it replaces an
/// installed file. It travels with the package rather than independently of it,
/// so it does not establish who built the package, only that what arrived is
/// what was published.
pub(crate) fn stage(release: &Release, staging: &Path) -> Result<PathBuf, InstallError> {
    let (package, checksum) = package_assets(&release.assets).ok_or(InstallError::NoPackage)?;

    let directory = staging.join(sanitized(&release.label));

    // A staging directory left by an earlier attempt may hold files from
    // another release, which unpacking over would mix into this one.
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).map_err(|error| {
        warn!("update: creating {} failed: {error}", directory.display());

        InstallError::Unreachable
    })?;

    let archive = directory.join("package.zip");

    download(&package.url, &archive)?;
    verify(&archive, &fetch_text(&checksum.url)?)?;
    unpack(&archive, &directory)?;

    let _ = fs::remove_file(&archive);

    Ok(directory)
}

/// The package and the checksum published for it. Both must be present: a
/// package without one cannot be checked, and installing an unchecked package
/// is the thing the checksum exists to prevent.
fn package_assets(assets: &[Asset]) -> Option<(&Asset, &Asset)> {
    let package = assets
        .iter()
        .find(|asset| asset.name.ends_with(".zip") && asset.url.starts_with(DOWNLOAD_URL_PREFIX))?;
    let expected = format!("{}.sha256", package.name);
    let checksum = assets.iter().find(|asset| asset.name == expected)?;

    Some((package, checksum))
}

fn client() -> Result<Client, InstallError> {
    Client::builder()
        .timeout(TRANSFER_TIMEOUT)
        .user_agent(user_agent())
        .build()
        .map_err(|_| InstallError::Unreachable)
}

fn download(url: &str, into: &Path) -> Result<(), InstallError> {
    let mut response = client()?
        .get(url)
        .send()
        .and_then(Response::error_for_status)
        .map_err(|error| {
            warn!("update: downloading the package failed: {error}");

            InstallError::Unreachable
        })?;

    if response.content_length().unwrap_or(0) > MAX_PACKAGE_BYTES {
        return Err(InstallError::Unreachable);
    }

    let mut file = File::create(into).map_err(|_| InstallError::NotWritable)?;

    // Copying through a bounded reader rather than trusting the declared length,
    // which a response is free to understate.
    let copied = io::copy(&mut response.by_ref().take(MAX_PACKAGE_BYTES), &mut file)
        .map_err(|_| InstallError::Unreachable)?;

    if copied == MAX_PACKAGE_BYTES {
        return Err(InstallError::Unreachable);
    }

    file.flush().map_err(|_| InstallError::NotWritable)
}

fn fetch_text(url: &str) -> Result<String, InstallError> {
    client()?
        .get(url)
        .send()
        .and_then(Response::error_for_status)
        .and_then(Response::text)
        .map_err(|error| {
            warn!("update: downloading the checksum failed: {error}");

            InstallError::Unreachable
        })
}

fn verify(archive: &Path, published: &str) -> Result<(), InstallError> {
    let expected = expected_digest(published).ok_or(InstallError::Checksum)?;
    let mut file = File::open(archive).map_err(|_| InstallError::Checksum)?;
    let mut hasher = Sha256::new();

    io::copy(&mut file, &mut hasher).map_err(|_| InstallError::Checksum)?;

    let actual = hex(&hasher.finalize());

    if actual == expected {
        Ok(())
    } else {
        warn!("update: package digest {actual} does not match the published {expected}");

        Err(InstallError::Checksum)
    }
}

/// The digest out of a `sha256sum` line, which is the digest followed by the
/// name it was taken over. Only the digest is compared: the name in the file is
/// the one the publisher used, not the one the package was saved under here.
fn expected_digest(published: &str) -> Option<String> {
    let digest = published.split_whitespace().next()?.to_ascii_lowercase();

    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(digest)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unpack(archive: &Path, into: &Path) -> Result<(), InstallError> {
    let file = File::open(archive).map_err(|_| InstallError::Unpack)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| {
        warn!("update: the package is not a readable archive: {error}");

        InstallError::Unpack
    })?;

    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|_| InstallError::Unpack)?;
        let name = flat_name(entry.name()).ok_or_else(|| {
            warn!(
                "update: the package holds an entry named `{}`",
                entry.name()
            );

            InstallError::Unpack
        })?;

        let mut target = File::create(into.join(name)).map_err(|_| InstallError::NotWritable)?;

        io::copy(&mut entry, &mut target).map_err(|_| InstallError::Unpack)?;
    }

    Ok(())
}

/// The entry's name, if it is one a package produces.
///
/// The published package is a flat list of files, so any path structure in a
/// name belongs to an archive that is not one — including the `..` and absolute
/// forms that would otherwise write outside the staging directory. Rejecting
/// the archive rather than skipping the entry keeps a package that cannot be
/// trusted from being installed in part.
fn flat_name(name: &str) -> Option<&str> {
    let plain = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.contains('\0');

    plain.then_some(name)
}

/// A release label reaches this as a directory name, and a tag is free to hold
/// characters a path is not.
fn sanitized(label: &str) -> String {
    label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '.' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
