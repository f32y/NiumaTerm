//! Encrypted at-rest storage for Agent Profile custom endpoint credentials.
//!
//! The custom API URL and API key are stored in `config.toml` as one
//! versioned AES-256-GCM value so a program reading the file cannot recover
//! them as plaintext. The key is compiled into the executable, so this only
//! protects against direct configuration reads; executable analysis or
//! process inspection can still recover the values.

#[cfg(test)]
mod tests;

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

/// Fixed application key. Randomly generated once; the bytes carry no
/// meaning. Changing them makes every previously saved `api-credentials`
/// value unreadable, so they must stay identical across releases. A future
/// key change requires a new version prefix with its own reader.
const KEY: [u8; 32] = [
    50, 115, 106, 127, 87, 114, 50, 181, 6, 252, 87, 27, 234, 146, 52, 129, 68, 126, 18, 153, 49,
    151, 155, 236, 238, 137, 42, 155, 197, 212, 89, 52,
];

/// Version prefix selecting the decoder before Base64 processing. Unknown
/// prefixes fail visibly instead of being guessed.
const PREFIX: &str = "aes256gcm-v1:";

/// Associated data binding ciphertext to this storage purpose, so a value
/// cannot be replayed into a different future encryption use of the same key.
const AAD: &[u8] = b"NiumaTerm/agent-profile-credentials/v1";

/// AES-GCM nonce length in bytes (96 bits).
const NONCE_LEN: usize = 12;

/// AES-GCM authentication tag length in bytes.
const TAG_LEN: usize = 16;

/// Plaintext payload. URL and key are encrypted together so neither can be
/// swapped independently of the other.
#[derive(Serialize, Deserialize)]
struct CredentialPayload {
    #[serde(default, rename = "api-base-url")]
    api_base_url: String,
    #[serde(default, rename = "api-key")]
    api_key: String,
}

/// Encrypt a custom API URL and API key into one `aes256gcm-v1:` value.
/// Every call draws a fresh nonce, so repeated encryption of the same input
/// produces different output. Errors never contain the input values.
pub(crate) fn encrypt(api_base_url: &str, api_key: &str) -> Result<String, String> {
    let payload = toml::to_string(&CredentialPayload {
        api_base_url: api_base_url.to_string(),
        api_key: api_key.to_string(),
    })
    .map_err(|_| -> String { "credential payload could not be encoded".into() })?;

    let mut nonce = [0u8; NONCE_LEN];

    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| -> String { "operating-system random source unavailable".into() })?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&KEY));

    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: payload.as_bytes(),
                aad: AAD,
            },
        )
        .map_err(|_| -> String { "credential encryption failed".into() })?;

    let mut bytes = nonce.to_vec();

    bytes.extend_from_slice(&ciphertext);

    Ok(format!("{PREFIX}{}", BASE64.encode(bytes)))
}

/// Decrypt an `api-credentials` value back into `(api_base_url, api_key)`.
/// Errors describe only the failure category so diagnostics never leak
/// encrypted or decrypted credential text.
pub(crate) fn decrypt(stored: &str) -> Result<(String, String), String> {
    let encoded = stored
        .strip_prefix(PREFIX)
        .ok_or_else(|| -> String { "unsupported credential format version".into() })?;
    let bytes = BASE64
        .decode(encoded)
        .map_err(|_| -> String { "credential value is not valid Base64".into() })?;

    if bytes.len() < NONCE_LEN + TAG_LEN {
        return Err("credential value is too short".into());
    }

    let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&KEY));

    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: AAD,
            },
        )
        .map_err(|_| -> String { "credential value failed authentication".into() })?;

    let text = String::from_utf8(plaintext)
        .map_err(|_| -> String { "decrypted credential payload is not valid UTF-8".into() })?;
    let payload: CredentialPayload = toml::from_str(&text)
        .map_err(|_| -> String { "decrypted credential payload could not be decoded".into() })?;

    Ok((payload.api_base_url, payload.api_key))
}
