use std::path::Path;
use std::{fs, io, ptr, slice};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
};

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
    #[error("DPAPI refused the key material (wrong user context?)")]
    Dpapi,
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
            let private = dpapi_unprotect(&stored.private_dpapi).ok_or(KeyStoreError::Dpapi)?;
            Ok(StaticKeypair {
                private,
                public: stored.public,
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let keypair = generate_keypair()?;
            let stored = StoredKey {
                public: keypair.public.clone(),
                private_dpapi: dpapi_protect(&keypair.private).ok_or(KeyStoreError::Dpapi)?,
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

fn dpapi_protect(data: &[u8]) -> Option<Vec<u8>> {
    dpapi_call(data, |input, output| unsafe {
        CryptProtectData(
            input,
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            output,
        )
    })
}

fn dpapi_unprotect(blob: &[u8]) -> Option<Vec<u8>> {
    dpapi_call(blob, |input, output| unsafe {
        CryptUnprotectData(
            input,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            output,
        )
    })
}

fn dpapi_call(
    data: &[u8],
    f: impl Fn(*const CRYPT_INTEGER_BLOB, *mut CRYPT_INTEGER_BLOB) -> i32,
) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).ok()?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    if f(&input, &mut output) == 0 {
        return None;
    }
    // CryptProtectData allocates the output with LocalAlloc; copy then free.
    let result = unsafe { slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData.cast()) };
    Some(result)
}

#[cfg(test)]
mod tests {
    use std::{env, process};

    use super::*;

    #[test]
    fn dpapi_roundtrip() {
        let secret = b"thirty-two bytes of key material";
        let blob = dpapi_protect(secret).expect("protect");
        assert_ne!(blob, secret, "blob must not be plaintext");
        assert_eq!(dpapi_unprotect(&blob).expect("unprotect"), secret);
    }

    #[test]
    fn tampered_blob_fails() {
        let mut blob = dpapi_protect(b"secret").expect("protect");
        let last = blob.len() - 1;
        blob[last] ^= 0xFF;
        assert!(dpapi_unprotect(&blob).is_none());
    }

    #[test]
    fn keypair_persists_across_loads() {
        let dir = env::temp_dir().join(format!("nmt-key-test-{}", process::id()));
        let path = dir.join("host-key.json");
        let first = load_or_create_keypair(&path).expect("create");
        let second = load_or_create_keypair(&path).expect("load");
        assert_eq!(first.public, second.public);
        assert_eq!(first.private, second.private);
        fs::remove_dir_all(&dir).ok();
    }
}
