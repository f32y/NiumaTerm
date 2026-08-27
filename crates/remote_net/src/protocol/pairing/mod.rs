use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Prefix marks the string as a NiumaTerm pairing code and versions the
/// payload layout so a future format change stays distinguishable.
const CODE_PREFIX: &str = "NMT1-";

/// One-time pairing offer, carried out-of-band (typed or pasted by the user).
/// The host public key inside is what makes the relay path MITM-proof: the
/// client pins it before ever talking through the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    pub relay_url: String,
    pub host_id: String,
    pub host_public_key: [u8; 32],
    pub token: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingCodeError {
    #[error("pairing code must start with {CODE_PREFIX}")]
    MissingPrefix,
    #[error("pairing code is not valid base32")]
    InvalidBase32,
    #[error("pairing code payload is malformed")]
    Malformed,
}

impl PairingCode {
    pub fn encode(&self) -> String {
        let payload = postcard::to_stdvec(self).expect("in-memory serialization cannot fail");
        let encoded = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &payload);
        format!("{CODE_PREFIX}{encoded}")
    }

    pub fn decode(code: &str) -> Result<Self, PairingCodeError> {
        // Tolerate the mangling that happens to hand-copied strings:
        // surrounding whitespace and lowercased letters.
        let code = code.trim();
        let body = code
            .strip_prefix(CODE_PREFIX)
            .or_else(|| code.strip_prefix(&CODE_PREFIX.to_lowercase()))
            .ok_or(PairingCodeError::MissingPrefix)?;
        let payload = base32::decode(
            base32::Alphabet::Rfc4648 { padding: false },
            &body.to_uppercase(),
        )
        .ok_or(PairingCodeError::InvalidBase32)?;
        postcard::from_bytes(&payload).map_err(|_| PairingCodeError::Malformed)
    }
}

/// Relay routing key, derived from the host's static public key so a host
/// cannot claim an id it doesn't own the key for (verifiable by clients).
pub fn derive_host_id(host_public_key: &[u8]) -> String {
    let digest = Sha256::digest(host_public_key);
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn new_pairing_token() -> [u8; 16] {
    let mut token = [0u8; 16];
    rand::rng().fill_bytes(&mut token);
    token
}

#[cfg(test)]
mod tests;
