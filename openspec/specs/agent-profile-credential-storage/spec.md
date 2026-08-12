# Agent Profile Credential Storage Specification

## Purpose

Protects Agent Profile custom endpoint URLs and API keys from direct plaintext recovery when another program reads the NiumaTerm configuration file.

## Requirements

### Requirement: Custom endpoint credentials are stored as one encrypted value
The system SHALL persist an Agent Profile custom API URL and API key together in one versioned encrypted `api-credentials` value and SHALL omit their plaintext values from `config.toml`.

#### Scenario: Saving a configured custom endpoint
- **WHEN** a user saves an Agent Profile containing a custom API URL and API key
- **THEN** `config.toml` contains one non-plaintext `api-credentials` value for that profile and contains neither plaintext credential

#### Scenario: Saving the same credentials again
- **WHEN** the same custom API URL and API key are saved more than once
- **THEN** each save produces a newly randomized stored value that still restores the original credentials

### Requirement: Encrypted credentials preserve existing profile behavior
The system SHALL decrypt a valid `api-credentials` value when loading configuration and SHALL provide the restored URL and API key to the existing Agent Profile editor and Agent launch path.

#### Scenario: Reopening a profile with encrypted credentials
- **WHEN** the settings UI opens an Agent Profile whose encrypted credentials are valid
- **THEN** the editor receives the original custom API URL and API key

#### Scenario: Launching an Agent with encrypted credentials
- **WHEN** an Agent starts from a profile whose encrypted credentials are valid and whose custom endpoint is enabled
- **THEN** the Agent uses the original custom API URL and API key through the same provider-specific launch behavior as before encryption

### Requirement: Legacy plaintext credentials migrate on save
The system SHALL continue loading legacy `api-base-url` and `api-key` fields and SHALL replace them with `api-credentials` the next time settings are saved.

#### Scenario: Loading a legacy profile
- **WHEN** `config.toml` contains an Agent Profile with legacy plaintext credential fields and no `api-credentials` value
- **THEN** the profile loads with the original custom API URL and API key without modifying the file during load

#### Scenario: Saving after legacy load
- **WHEN** settings are saved after loading legacy plaintext credentials
- **THEN** the profile is written with `api-credentials` and the legacy plaintext fields are removed

### Requirement: Invalid encrypted credentials fail visibly
The system SHALL reject an unsupported, malformed, or modified `api-credentials` value and SHALL NOT launch an Agent using empty, partial, or legacy fallback credentials from that profile.

#### Scenario: Encrypted value cannot be decoded
- **WHEN** an Agent Profile contains malformed encrypted credential text
- **THEN** configuration loading reports an actionable credential error and preserves the stored value for recovery

#### Scenario: Encrypted value does not authenticate
- **WHEN** any encrypted credential byte has been modified
- **THEN** configuration loading reports an actionable credential error and does not expose a URL or API key from that value

#### Scenario: Encrypted and legacy fields coexist
- **WHEN** an Agent Profile contains both `api-credentials` and legacy plaintext credential fields
- **THEN** the system uses only `api-credentials` and does not fall back to the legacy fields if encrypted credential loading fails

### Requirement: Protection scope is limited and explicit
The system SHALL describe encrypted Agent Profile credentials as protection against direct plaintext recovery from `config.toml`, without claiming protection from executable analysis, process-memory access, or Agent environment inspection.

#### Scenario: User reviews credential storage behavior
- **WHEN** credential storage behavior is described in user-facing help or release notes
- **THEN** the description states the intended protection scope and does not present the embedded key as protection from local code analysis
