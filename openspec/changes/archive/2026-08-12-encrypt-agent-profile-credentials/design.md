## Context

`AgentProfile` currently carries `api_base_url` and `api_key` as ordinary strings. Serde loads both from `config.toml`, `patch_agent_document` writes both back as plaintext, the settings dialog edits the strings, and `agent_launch` passes them to the selected provider. Settings saves already use a temporary file followed by rename.

NiumaTerm has configuration paths for Windows, macOS, and other desktop targets, including a separate `Test` directory selected by `--testing`. The requested protection must therefore remain inside the configuration layer and must not depend on a platform credential service. See `proposal.md` for the intended protection boundary.

## Goals / Non-Goals

**Goals:**

- Remove custom API URLs and API keys from readable `config.toml` text.
- Detect malformed or modified encrypted values before any Agent launch.
- Preserve the current in-memory `AgentProfile`, settings editor, and provider launch behavior.
- Read existing plaintext profiles and migrate them only during a later settings save.
- Keep all persistent state in `config.toml` and use one stable application key across releases.

**Non-Goals:**

- Resist extraction of the embedded key from the executable.
- Protect decrypted values from process-memory readers or child environment inspection.
- Encrypt user-defined Agent Profile environment variables.
- Add a master password, operating-system credential service, key rotation UI, or account synchronization.
- Allow an older NiumaTerm build to read credentials after the new format has been saved.

## Decisions

### 1. Use AES-256-GCM with one embedded random key

The configuration crate will directly use `aes-gcm` with a 256-bit key generated once from a secure random source and committed as a `[u8; 32]` constant. The bytes will not be derived from a product name or another readable phrase. The key must remain unchanged across releases because previously saved values depend on it.

AES-GCM is selected because one operation provides confidentiality and modification detection. AES-CBC, AES-CTR, and simple byte transformations were rejected because they require separate integrity handling or provide only weaker obscuring. Windows DPAPI and Credential Manager were rejected for this change because the selected protection boundary does not justify platform-owned credential lifecycle work.

The key is intentionally recoverable through executable analysis. This design only removes immediately readable credentials from the configuration file.

### 2. Encrypt URL and API key together in a versioned text value

Each persisted profile will use this shape:

```toml
api-credentials = "aes256gcm-v1:<base64>"
```

The decoded bytes are:

```text
12-byte nonce | ciphertext with 16-byte authentication tag
```

The plaintext is a private Serde payload containing `api_base_url` and `api_key`. It will use the existing TOML serializer so no additional payload codec is needed. The encryption call will use a fixed associated-data label, `NiumaTerm/agent-profile-credentials/v1`, to prevent the stored value from being accepted by another future encryption purpose.

The payload is not bound to the profile name because renaming a profile must not invalidate its credentials. URL and key are kept in the same payload so they cannot be independently replaced. If both strings are empty, the writer omits `api-credentials`; if either string is non-empty, both are represented in the encrypted payload.

The prefix selects the decoder before Base64 processing. Unknown versions fail visibly instead of being guessed. Future formats can add another prefix while retaining the v1 reader.

### 3. Generate a new nonce for every settings save

Every encryption operation will fill a fresh 96-bit nonce from the operating-system random source. The nonce is stored next to its ciphertext and is not secret. A random-source failure aborts the settings save.

The settings writer rebuilds every Agent Profile table, so every profile containing credentials will receive a new encrypted value even when an unrelated setting caused the save. Retaining previous ciphertext in the runtime model was rejected because it would mix persistence state into `AgentProfile` and complicate edits. The small configuration size makes repeated encryption negligible.

### 4. Keep plaintext only in the runtime model

`AgentProfile` will continue exposing `api_base_url` and `api_key` strings to the application. The configuration layer will introduce a private persisted representation that accepts `api-credentials` plus the two legacy fields and converts it into the runtime type after decryption.

On save, `patch_agent_document` will encrypt the runtime strings and write only `api-credentials`. Encryption errors must propagate through `patch_settings_document` and `save_settings_to`; the temporary file must not replace the current configuration unless every profile was encoded successfully.

This keeps changes out of the settings editor and Agent adapters. It also keeps decrypted values in memory for the same duration as today, which is accepted by the stated protection boundary.

### 5. Make migration one-way and prevent plaintext fallback

Loading follows this order:

1. If `api-credentials` exists, decode and decrypt it. Ignore legacy credential fields, including when decryption fails.
2. Otherwise, read legacy `api-base-url` and `api-key` values.
3. Do not modify the configuration during load.
4. On the next settings save, write only `api-credentials` for every profile with either value present.

Encrypted-first precedence prevents a modified value from triggering a downgrade to plaintext data left beside it. A user recovering from damaged encrypted data can remove `api-credentials` and re-enter the values through settings.

The first encrypted save is a one-way migration for that profile. Release notes must state that an older build cannot restore it and that rollback requires re-entering the URL and key.

### 6. Fail configuration loading on invalid encrypted data

Malformed Base64, an unknown prefix, a payload shorter than the nonce and tag, authentication failure, invalid UTF-8, or payload decoding failure will produce a configuration error naming the affected profile without including its encrypted or decrypted value. NiumaTerm will use its existing startup configuration error presentation and will not launch an Agent from a partially restored profile.

Failing the configuration load was selected over silently clearing the fields because an empty API key can change provider selection or send traffic to an unintended destination. It was also selected over automatic deletion so the original value remains available for diagnosis.

### 7. Pin behavior with crypto and configuration tests

Unit tests will cover known-value decryption, round trips, different output for repeated encryption, modification rejection, malformed and unknown versions, and empty-value handling. A known encrypted test vector will make an accidental embedded-key or format change visible.

Configuration tests will assert that saved TOML contains neither plaintext input, that legacy profiles load without immediate file mutation, that a later save removes legacy fields, that encrypted values take precedence, and that a failed encryption leaves the existing file unchanged. Existing Agent launch tests will continue proving that runtime profile values reach the provider-specific environment and model provider configuration.

## Risks / Trade-offs

- [The embedded key can be extracted from the executable] → State the narrow protection target in release notes and avoid stronger security claims.
- [Changing the fixed key makes saved profiles unreadable] → Keep a known encrypted test vector and require a new versioned reader before any future key change.
- [A random nonce collision under one key weakens AES-GCM] → Use a fresh 96-bit operating-system random value for every save; the number of local profile writes is very small.
- [Unrelated settings saves change encrypted text] → Accept configuration churn to keep runtime and persistence models simple.
- [One damaged profile prevents configuration loading] → Return an error naming the profile and preserve the file so the user can remove the damaged field and re-enter credentials.
- [Older builds cannot read the new field] → Document the one-way migration and rollback procedure before release.

## Migration Plan

1. Add the v1 crypto helper, fixed key, dependencies, and deterministic test vector.
2. Add dual-format loading with encrypted-first precedence while retaining legacy plaintext input.
3. Change settings persistence to emit only `api-credentials` and propagate encryption failures before replacing `config.toml`.
4. Add migration, invalid-data, and atomic-save regression coverage.
5. Validate add, edit, restart, Agent launch, and legacy migration in an isolated `--testing` application launch.
6. Release with a note that the first settings save removes plaintext credentials and makes them unreadable to older builds.

Rollback of the application code does not decrypt or rewrite existing values. A user returning to an older build must delete `api-credentials` and re-enter the custom URL and API key in that build.
