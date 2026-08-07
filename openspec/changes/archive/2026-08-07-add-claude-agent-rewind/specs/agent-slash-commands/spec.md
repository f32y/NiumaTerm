## ADDED Requirements

### Requirement: Claude rewind is a local interactive command
Claude Agent Tab SHALL expose `/rewind` as a NiumaTerm-owned `IdleOnly` command that opens the native checkpoint selection flow. Codex Agent Tab SHALL NOT advertise this command. Executing `/rewind` MUST NOT send slash text through `send_user_message` or the Claude provider-command path, and the command itself SHALL NOT create a user bubble, provider turn, or working timer.

#### Scenario: Open rewind from an idle Claude tab
- **WHEN** an idle Claude Agent Tab user selects or submits `/rewind`
- **THEN** Agent Tab opens the rewind target selector without sending `/rewind` to the Claude process

#### Scenario: Rewind is unavailable during active work
- **WHEN** a Claude turn, approval, or provider command is active
- **THEN** `/rewind` remains visible with an idle-only disabled reason and cannot begin until the session is idle

#### Scenario: Open the Codex slash palette
- **WHEN** a Codex Agent Tab user opens or filters the slash command palette
- **THEN** `/rewind` is absent unless a separate Codex rewind capability is implemented

#### Scenario: Rewind flow reports local feedback
- **WHEN** rewind is cancelled, succeeds, fails, or partially succeeds
- **THEN** its feedback remains distinguishable from conversation turns and does not advance turn counters or completed-turn folding
