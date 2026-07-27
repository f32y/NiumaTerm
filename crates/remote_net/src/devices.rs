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
mod tests {
    use std::{env, process};

    use super::*;

    #[test]
    fn add_contains_remove_persist() {
        let dir = env::temp_dir().join(format!("nmt-dev-test-{}", process::id()));
        let path = dir.join("authorized_devices.json");
        let key = [7u8; 32];

        let mut devices = AuthorizedDevices::load(path.clone()).unwrap();
        assert!(!devices.contains(&key));
        devices.add("laptop", &key).unwrap();
        assert!(devices.contains(&key));

        // Persisted: a fresh load sees the entry.
        let reloaded = AuthorizedDevices::load(path.clone()).unwrap();
        assert!(reloaded.contains(&key));
        assert_eq!(reloaded.entries()[0].name, "laptop");

        assert!(devices.remove(&hex_encode(&key)).unwrap());
        assert!(!devices.contains(&key));
        let reloaded = AuthorizedDevices::load(path).unwrap();
        assert!(!reloaded.contains(&key));
        fs::remove_dir_all(&dir).ok();
    }
}
