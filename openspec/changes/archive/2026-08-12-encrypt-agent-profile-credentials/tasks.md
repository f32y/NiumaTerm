## 1. Crypto Foundation

- [x] 1.1 Add direct `aes-gcm`, `base64`, and secure random dependencies to `nmt_config`, update Cargo and Bazel dependency declarations, and refresh the required lockfiles.
- [x] 1.2 Add a private configuration crypto module with the stable random 32-byte key, v1 prefix, associated-data label, 12-byte nonce generation, and AES-256-GCM encode/decode helpers.
- [x] 1.3 Add focused crypto tests for a known encrypted vector, successful round trips, newly randomized repeated output, modified data rejection, short input, malformed Base64, and unknown versions.

## 2. Agent Profile Configuration

- [x] 2.1 Add a private persisted Agent Profile representation that decrypts `api-credentials`, accepts legacy `api-base-url` and `api-key` only when the encrypted field is absent, and returns errors that name the profile without including credential data.
- [x] 2.2 Keep the runtime `AgentProfile` plaintext fields unchanged while converting from the persisted representation during configuration loading.
- [x] 2.3 Change Agent Profile settings persistence to encrypt URL and key together, omit all credential fields when both values are empty, and remove legacy plaintext fields from saved TOML.
- [x] 2.4 Propagate random-generation and encryption errors through settings patching so the temporary configuration never replaces the current file after a credential encoding failure.
- [x] 2.5 Add configuration tests proving plaintext is absent after save, repeated saves produce different stored text, empty credentials are omitted, and a save/load round trip restores both values.
- [x] 2.6 Add migration tests proving legacy input does not change during load, migrates on the next save, and never overrides an existing encrypted value.
- [x] 2.7 Add invalid-data tests proving malformed, unsupported, and modified values fail without credential text in diagnostics or fallback to adjacent legacy fields.

## 3. Application Behavior and Validation

- [x] 3.1 Verify the Agent Profile editor and both provider launch paths receive restored URL and API key values after a configuration round trip, adding regression coverage where current tests do not reach that path.
- [x] 3.2 Add concise user-facing text describing protection from direct plaintext configuration reads and the limits against executable or process inspection.
- [x] 3.3 Run formatting, `nmt_config` and affected `app` tests, scoped Clippy checks, and the repository dependency lock validation.
- [x] 3.4 In an isolated `NiumaTerm.exe --testing` launch, validate add, edit, save, file inspection, restart, Agent launch, legacy migration, and modified encrypted data handling.
- [x] 3.5 Add release guidance stating that the first encrypted save is a one-way migration for older builds and that rollback requires re-entering the custom URL and API key.
