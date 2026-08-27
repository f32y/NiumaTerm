use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};

/// The host's authorized-device list: which client static public keys may
/// complete an IK handshake. Removing an entry is the revocation mechanism —
/// there is no other credential to invalidate.
pub struct AuthorizedDevices {
    path: PathBuf,
    entries: Vec<DeviceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub name: String,
    /// Hex-encoded X25519 public key (64 chars).
    pub public_key: String,
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Inverse of [`hex_encode`], for keys that round-tripped through a config or
/// device-list file.
pub fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

impl AuthorizedDevices {
    /// Load from `path`; a missing file is an empty list (first run).
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let entries = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, entries })
    }

    pub fn contains(&self, public_key: &[u8]) -> bool {
        let hex = hex_encode(public_key);
        self.entries.iter().any(|d| d.public_key == hex)
    }

    pub fn add(&mut self, name: &str, public_key: &[u8]) -> io::Result<()> {
        let hex = hex_encode(public_key);
        // Re-pairing the same device just refreshes its name.
        self.entries.retain(|d| d.public_key != hex);
        self.entries.push(DeviceEntry {
            name: name.to_owned(),
            public_key: hex,
        });
        self.save()
    }

    pub fn remove(&mut self, public_key_hex: &str) -> io::Result<bool> {
        let before = self.entries.len();
        self.entries.retain(|d| d.public_key != public_key_hex);
        let removed = self.entries.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn entries(&self) -> &[DeviceEntry] {
        &self.entries
    }

    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &self.path,
            serde_json::to_vec_pretty(&self.entries).expect("serializable"),
        )
    }
}

#[cfg(test)]
mod tests;
