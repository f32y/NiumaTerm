use std::path::Path;
use std::{fs, io};

use nmt_platform::windows::data_protection;
use serde::{Deserialize, Serialize};

use crate::protocol::{NoiseError, StaticKeypair, generate_keypair};

/// On-disk identity key. The private half is a DPAPI blob bound to the
/// current Windows user, so copying the file to another account (or reading
/// it offline) yields nothing decryptable. The public half stays plaintext:
/// X25519 publics are not derivable from the DPAPI blob without a decrypt,
/// and callers need the public key without paying a DPAPI round trip.
#[derive(Serialize, Deserialize)]
struct StoredKey {
    public: Vec<u8>,
    private_dpapi: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyStoreError {
    #[error("key file I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("key file is corrupt")]
    Corrupt,
    #[error("DPAPI refused the key material (wrong user context?): {0}")]
    Dpapi(io::Error),
    #[error("key generation failed: {0}")]
    Generate(#[from] NoiseError),
}

/// Load the identity keypair at `path`, generating and persisting a fresh one
/// on first use. Used for both the host identity and the client device
/// identity — they are the same shape, just different files.
pub fn load_or_create_keypair(path: &Path) -> Result<StaticKeypair, KeyStoreError> {
    match fs::read(path) {
        Ok(bytes) => {
            let stored: StoredKey =
                serde_json::from_slice(&bytes).map_err(|_| KeyStoreError::Corrupt)?;
            let private =
                data_protection::unprotect(&stored.private_dpapi).map_err(KeyStoreError::Dpapi)?;
            Ok(StaticKeypair {
                private,
                public: stored.public,
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let keypair = generate_keypair()?;
            let stored = StoredKey {
                public: keypair.public.clone(),
                private_dpapi: data_protection::protect(&keypair.private)
                    .map_err(KeyStoreError::Dpapi)?,
            };
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, serde_json::to_vec(&stored).expect("serializable"))?;
            Ok(keypair)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests;
