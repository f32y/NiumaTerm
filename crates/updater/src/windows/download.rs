//! Fetching a published package and unpacking it where the swap can reach it.

#[cfg(test)]
#[path = "download_tests.rs"]
mod download_tests;

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{Client, Response};
use sha2::{Digest as _, Sha256};
use tokio::fs::File as AsyncFile;
use tokio::io::AsyncWriteExt as _;
use tokio::task::spawn_blocking;
use tracing::warn;

use crate::windows::InstallError;
use crate::windows::install::Installation;
use crate::windows::releases::{Asset, DOWNLOAD_URL_PREFIX, Release, user_agent};

/// Long enough for a package on a slow connection, short enough that a stalled
/// transfer does not leave the About page reporting an install forever.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A package is a few megabytes. The cap is what keeps a response that is not
/// one from being written to disk in full before anything notices.
const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

/// What the Windows packaging job names its archive, up to the version it
/// appends. A release carries the macOS archive too and both end in `.zip`, so
/// matching on the extension alone would let the order the assets arrive in
/// decide which system's build an installation downloads.
const PACKAGE_NAME_PREFIX: &str = "NiumaTerm-windows-x86_64-";

/// A captured release and destination, independent of later settings changes.
pub struct Download {
    pub(super) release: Release,
    pub(super) staging: PathBuf,
    pub(super) install: PathBuf,
    pub(super) version: &'static str,
    pub(super) testing: bool,
}

impl Download {
    /// Download, verify, unpack, and select replacement files.
    pub async fn run(self) -> Result<Installation, InstallError> {
        let staged = stage(&self.release, &self.staging, self.version).await?;

        Ok(Installation::new(
            self.release,
            staged,
            self.install,
            self.testing,
        ))
    }
}

/// Unpack `release`'s package into `staging`, and answer with the directory the
/// files ended up in.
///
/// The checksum published beside the package is what distinguishes a truncated
/// or corrupted download from a complete one before any of it replaces an
/// installed file. It travels with the package rather than independently of it,
/// so it does not establish who built the package, only that what arrived is
/// what was published.
///
/// Transfers wait on the network. Preparing the directory, hashing, and
/// unpacking are disk and CPU work, so they run as blocking stages.
async fn stage(release: &Release, staging: &Path, version: &str) -> Result<PathBuf, InstallError> {
    let (package, checksum) = package_assets(&release.assets).ok_or(InstallError::NoPackage)?;

    let name = sanitized(&release.label);
    let directory = staging.join(&name);

    blocking({
        let directory = directory.clone();

        move || prepare(&directory)
    })
    .await?;

    // The archive is kept beside the unpacked directory rather than inside it,
    // because that directory is read back as the list of files to install: a
    // download left behind by a removal that could not complete would otherwise
    // be installed as though the package had shipped it.
    let archive = staging.join(format!("{name}.zip"));

    download(&package.url, &archive, version).await?;

    let published = fetch_text(&checksum.url, version).await?;

    blocking(move || {
        verify(&archive, &published)?;

        unpack(&archive, &directory)?;

        let _ = fs::remove_file(&archive);

        Ok(directory)
    })
    .await
}

/// Run one blocking stage without occupying an I/O worker.
async fn blocking<T: Send + 'static>(
    stage: impl FnOnce() -> Result<T, InstallError> + Send + 'static,
) -> Result<T, InstallError> {
    spawn_blocking(stage).await.unwrap_or_else(|error| {
        warn!("update: a staging step did not finish: {error}");

        Err(InstallError::Unpack)
    })
}

fn prepare(directory: &Path) -> Result<(), InstallError> {
    // A staging directory left by an earlier attempt may hold files from
    // another release, which unpacking over would mix into this one.
    let _ = fs::remove_dir_all(directory);

    fs::create_dir_all(directory).map_err(|error| {
        warn!("update: creating {} failed: {error}", directory.display());

        InstallError::Unreachable
    })
}

/// The package and the checksum published for it. Both must be present: a
/// package without one cannot be checked, and installing an unchecked package
/// is the thing the checksum exists to prevent.
fn package_assets(assets: &[Asset]) -> Option<(&Asset, &Asset)> {
    let package = assets.iter().find(|asset| {
        asset.name.starts_with(PACKAGE_NAME_PREFIX)
            && asset.name.ends_with(".zip")
            && asset.url.starts_with(DOWNLOAD_URL_PREFIX)
    })?;

    let expected = format!("{}.sha256", package.name);

    let checksum = assets
        .iter()
        .find(|asset| asset.name == expected && asset.url.starts_with(DOWNLOAD_URL_PREFIX))?;

    Some((package, checksum))
}

fn client(version: &str) -> Result<Client, InstallError> {
    Client::builder()
        .timeout(TRANSFER_TIMEOUT)
        .user_agent(user_agent(version))
        .build()
        .map_err(|_| InstallError::Unreachable)
}

async fn download(url: &str, into: &Path, version: &str) -> Result<(), InstallError> {
    let mut response = client(version)?
        .get(url)
        .send()
        .await
        .and_then(Response::error_for_status)
        .map_err(|error| {
            warn!("update: downloading the package failed: {error}");

            InstallError::Unreachable
        })?;

    if response.content_length().unwrap_or(0) > MAX_PACKAGE_BYTES {
        return Err(InstallError::Unreachable);
    }

    let mut file = AsyncFile::create(into)
        .await
        .map_err(|_| InstallError::NotWritable)?;

    // Counting what actually arrives rather than trusting the declared length,
    // which a response is free to understate.
    let mut copied = 0u64;

    while let Some(chunk) = response.chunk().await.map_err(|error| {
        warn!("update: downloading the package failed: {error}");

        InstallError::Unreachable
    })? {
        copied += chunk.len() as u64;

        if copied >= MAX_PACKAGE_BYTES {
            return Err(InstallError::Unreachable);
        }

        file.write_all(&chunk)
            .await
            .map_err(|_| InstallError::NotWritable)?;
    }

    file.flush().await.map_err(|_| InstallError::NotWritable)
}

async fn fetch_text(url: &str, version: &str) -> Result<String, InstallError> {
    let fetched = async {
        client(version)
            .map_err(|_| None)?
            .get(url)
            .send()
            .await
            .and_then(Response::error_for_status)
            .map_err(Some)?
            .text()
            .await
            .map_err(Some)
    };

    fetched.await.map_err(|error| {
        if let Some(error) = error {
            warn!("update: downloading the checksum failed: {error}");
        }

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
