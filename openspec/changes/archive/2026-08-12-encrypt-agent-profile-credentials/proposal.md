## Why

Agent Profile custom API URLs and API keys are currently written to `config.toml` as plaintext, so any tool that reads the file can recover them immediately. NiumaTerm should obscure these values at rest while preserving the existing profile workflow and accepting the limited protection of an application-embedded key.

## What Changes

- Store each custom API URL and API key together in one versioned AES-256-GCM value encoded for TOML.
- Compile one fixed, randomly generated 256-bit key into NiumaTerm and generate a fresh 96-bit nonce for every encryption operation.
- Continue reading legacy plaintext `api-base-url` and `api-key` fields, then replace them with the encrypted value on the next settings save.
- **BREAKING**: After a profile is saved with `api-credentials`, older NiumaTerm builds cannot restore its custom API URL or API key; rolling back requires re-entering those values.
- Treat invalid or modified encrypted data as a visible configuration error instead of silently launching an Agent without its configured credentials.
- Keep Agent Profile editing and Agent launch behavior unchanged after successful decryption.
- Limit this change to custom endpoint URL and API key fields. User-defined environment variables remain outside this scope.

## Capabilities

### New Capabilities

- `agent-profile-credential-storage`: Defines encrypted static storage, migration, error handling, and the intended protection boundary for Agent Profile custom endpoint credentials.

### Modified Capabilities

None.

## Impact

- `crates/config`: Agent Profile deserialization, settings patching, encryption helpers, migration behavior, and configuration tests.
- `crates/app`: Settings loading and error presentation may need to distinguish unreadable encrypted credentials from ordinary missing values.
- Workspace dependencies: add direct use of `aes-gcm`, `base64`, and a secure operating-system random source already available in the dependency graph.
- Existing Agent launch adapters continue receiving plaintext URL and API key values in memory and passing the key through the child process environment.
- The protection target is direct plaintext recovery from `config.toml`; it does not protect against executable analysis, process-memory access, or a program running with enough access to inspect Agent process environments.
