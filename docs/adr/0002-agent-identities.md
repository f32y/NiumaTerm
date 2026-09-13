# Shared agent identities and launch profiles

Status: accepted, 2026-09-13.

Profiles, sessions, and background tasks describe the same three agent kinds.
Separate enums and a copied launch profile made each added setting require
repeated conversions in the application.

`nmt_profile` owns `AgentKind`, `AgentProfile`, and `AgentProfileLauncher`.
Sessions and background task keys reuse that identity; the launcher reads the
shared profile directly. Configuration files retain `claude-code`, `codex`, and
`deepseek` through profile serialization. Existing session serialization retains
its own labels. Round-trip tests cover both representations for all three kinds.

The update service retains its two-provider type because DeepSeek is installed
outside that service. Background task reference variants retain their distinct
provider data; they are not interchangeable identifiers. Capabilities remain
in the agent crate, and icons remain in the application.

Implementations: `crates/profile/src/kind.rs`, `crates/profile/src/lib.rs`,
`crates/agent/src/profile.rs`, and `crates/agent/src/session/capabilities.rs`.
