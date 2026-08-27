//! Pure slash-command parsing and catalog logic for the agent composer.

use std::collections::{HashSet, VecDeque};
use std::mem;

use nmt_agent_utils::chat::{
    SkillCatalog, SkillInfo, SkillReference, SlashCommandArguments, SlashCommandInfo,
    SlashCommandRunPolicy, SlashCommandSource,
};
use nmt_i18n::i18n;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParsedSlashCommand {
    pub name: String,
    pub arguments: String,
    pub has_argument_separator: bool,
}

/// Parse only an input whose first byte is `/`. A slash later in ordinary
/// prose is deliberately invisible to command routing.
pub(super) fn parse_slash_command(input: &str) -> Option<ParsedSlashCommand> {
    let tail = input.strip_prefix('/')?;
    let token_end = tail.find(char::is_whitespace).unwrap_or(tail.len());
    let remainder = &tail[token_end..];

    Some(ParsedSlashCommand {
        name: tail[..token_end].to_ascii_lowercase(),
        arguments: remainder
            .trim_start_matches(char::is_whitespace)
            .to_string(),
        has_argument_separator: !remainder.is_empty(),
    })
}

/// Parse only an input whose first token is `$name`. Once the user types past
/// that token the composer holds a skill invocation with arguments, so the
/// picker stops claiming the input.
pub(super) fn parse_skill_prefix(input: &str) -> Option<String> {
    let tail = input.strip_prefix('$')?;

    (!tail.contains(char::is_whitespace)).then(|| tail.to_ascii_lowercase())
}

pub(super) fn local_commands() -> Vec<SlashCommandInfo> {
    vec![
        command(
            "new",
            i18n("agent-command-new-description"),
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::IdleOnly,
        ),
        command(
            "clear",
            i18n("agent-command-clear-description"),
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::IdleOnly,
        ),
        command(
            "resume",
            i18n("agent-command-resume-description"),
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::IdleOnly,
        ),
        command(
            "model",
            i18n("agent-command-model-description"),
            Some(i18n("agent-command-model-hint")),
            SlashCommandArguments::Choices,
            SlashCommandRunPolicy::Immediate,
        ),
        command(
            "permissions",
            i18n("agent-command-permissions-description"),
            Some(i18n("agent-command-permissions-hint")),
            SlashCommandArguments::Choices,
            SlashCommandRunPolicy::Immediate,
        ),
        command(
            "status",
            i18n("agent-command-status-description"),
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::Immediate,
        ),
    ]
}

pub(super) fn setting_value_label(value: &str) -> String {
    let key = match value {
        "default" => "agent-setting-value-default",
        "auto" => "agent-setting-value-auto",
        "acceptEdits" => "agent-setting-value-accept-edits",
        "plan" => "agent-setting-value-plan",
        "bypassPermissions" => "agent-setting-value-bypass-permissions",
        "untrusted" => "agent-setting-value-untrusted",
        "on-request" => "agent-setting-value-on-request",
        "never" => "agent-setting-value-never",
        "user" => "agent-setting-value-user",
        "auto_review" => "agent-setting-value-auto-review",
        "readOnly" | "read-only" => "agent-setting-value-read-only",
        "workspaceWrite" | "workspace-write" => "agent-setting-value-workspace-write",
        "dangerFullAccess" | "full-access" => "agent-setting-value-full-access",
        "none" => "agent-setting-value-none",
        "minimal" => "agent-setting-value-minimal",
        "low" => "agent-setting-value-low",
        "medium" => "agent-setting-value-medium",
        "high" => "agent-setting-value-high",
        "xhigh" => "agent-setting-value-xhigh",
        "max" => "agent-setting-value-max",
        "ultra" => "agent-setting-value-ultra",
        "ultracode" => "agent-setting-value-ultracode",
        "normal" => "agent-setting-value-normal",
        _ => return value.to_string(),
    };

    i18n(key).to_string()
}

fn command(
    name: &str,
    description: &str,
    argument_hint: Option<&str>,
    arguments: SlashCommandArguments,
    run_policy: SlashCommandRunPolicy,
) -> SlashCommandInfo {
    SlashCommandInfo {
        name: name.to_string(),
        description: description.to_string(),
        argument_hint: argument_hint.map(str::to_string),
        source: SlashCommandSource::Local,
        arguments,
        run_policy,
    }
}

/// Normalize a provider/adapter name. Whitespace would make the advertised
/// command impossible to address as one slash token, so such names are
/// discarded rather than shown as entries that can never execute.
pub(super) fn normalize_command(mut command: SlashCommandInfo) -> Option<SlashCommandInfo> {
    let name = command.name.trim().trim_start_matches('/');

    if name.is_empty() || name.chars().any(char::is_whitespace) {
        return None;
    }

    command.name = name.to_ascii_lowercase();
    Some(command)
}

/// Merge in precedence order. Keeping the first normalized name makes local
/// commands authoritative over adapter commands, and adapter commands over
/// provider discovery, without relying on hash iteration order.
pub(super) fn merge_catalog(
    local: Vec<SlashCommandInfo>,
    adapter: Vec<SlashCommandInfo>,
    provider: Vec<SlashCommandInfo>,
) -> Vec<SlashCommandInfo> {
    let mut seen = HashSet::new();

    local
        .into_iter()
        .chain(adapter)
        .chain(provider)
        .filter_map(normalize_command)
        .filter(|command| seen.insert(command.name.clone()))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PaletteCatalogEntry {
    Command(SlashCommandInfo),
    Skill(SkillInfo),
}

fn text_match_rank<'a>(fields: impl IntoIterator<Item = &'a str>, query: &str) -> Option<usize> {
    let query = query.to_ascii_lowercase();
    let fields = fields
        .into_iter()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();

    if fields.iter().any(|field| field == &query) {
        Some(0)
    } else if fields.iter().any(|field| field.starts_with(&query)) {
        Some(1)
    } else if fields.iter().any(|field| field.contains(&query)) {
        Some(2)
    } else {
        None
    }
}

fn command_match_rank(command: &SlashCommandInfo, query: &str) -> Option<usize> {
    text_match_rank([command.name.as_str()], query)
}

fn skill_match_rank(skill: &SkillInfo, query: &str) -> Option<usize> {
    text_match_rank(
        [
            Some(skill.name.as_str()),
            skill.display_name.as_deref(),
            Some(skill.description.as_str()),
        ]
        .into_iter()
        .flatten(),
        query,
    )
}

/// Merge commands and skills into one ranked result without erasing their
/// different activation semantics. Within each rank, command catalog order
/// is stable and is followed by provider skill order.
pub(super) fn filter_palette_catalog(
    commands: &[SlashCommandInfo],
    skills: &[SkillInfo],
    query: &str,
) -> Vec<PaletteCatalogEntry> {
    let mut buckets = [Vec::new(), Vec::new(), Vec::new()];

    for command in commands {
        if let Some(rank) = command_match_rank(command, query) {
            buckets[rank].push(PaletteCatalogEntry::Command(command.clone()));
        }
    }
    for skill in skills {
        if let Some(rank) = skill_match_rank(skill, query) {
            buckets[rank].push(PaletteCatalogEntry::Skill(skill.clone()));
        }
    }

    buckets.into_iter().flatten().collect()
}

/// Skill results use the command palette's exact/prefix/substring ranking,
/// but identity stays attached to each full row so duplicate names from
/// different scopes are never collapsed.
pub(super) fn filter_skill_catalog(catalog: &[SkillInfo], query: &str) -> Vec<SkillInfo> {
    let mut buckets = [Vec::new(), Vec::new(), Vec::new()];

    for skill in catalog {
        if let Some(rank) = skill_match_rank(skill, query) {
            buckets[rank].push(skill.clone());
        }
    }

    buckets.into_iter().flatten().collect()
}

fn input_has_bound_skill_token(input: &str, binding: &SkillReference) -> bool {
    input
        .split_whitespace()
        .next()
        .is_some_and(|token| token == format!("${}", binding.name))
}

/// Editing task text after `$name` is safe; changing the first token turns
/// the composer back into ordinary unbound text.
pub(super) fn reconcile_skill_binding(input: &str, binding: &mut Option<SkillReference>) {
    if binding
        .as_ref()
        .is_some_and(|binding| !input_has_bound_skill_token(input, binding))
    {
        *binding = None;
    }
}

/// Re-check the live replacement snapshot immediately before submission so
/// a file watcher cannot leave the UI holding a removed or disabled path.
pub(super) fn validate_skill_binding(
    input: &str,
    binding: Option<&SkillReference>,
    catalog: Option<&SkillCatalog>,
) -> Result<Option<SkillReference>, String> {
    let Some(binding) = binding else {
        return Ok(None);
    };
    if !input_has_bound_skill_token(input, binding) {
        return Ok(None);
    }

    let Some(catalog) = catalog else {
        return Err(i18n("agent-command-skill-loading").to_string());
    };
    let Some(skill) = catalog
        .skills
        .iter()
        .find(|skill| skill.name == binding.name && skill.path == binding.path)
    else {
        return Err(i18n("agent-command-skill-unavailable").replace("{name}", &binding.name));
    };
    if !skill.enabled {
        return Err(i18n("agent-command-skill-disabled").replace("{name}", &binding.name));
    }

    Ok(Some(binding.clone()))
}

pub(super) fn prepare_skill_selection(
    skill: &SkillInfo,
) -> Result<(String, SkillReference), String> {
    if !skill.enabled {
        return Err(i18n("agent-command-skill-disabled-by-codex").replace("{name}", &skill.name));
    }

    Ok((
        format!("${} ", skill.name),
        SkillReference {
            name: skill.name.clone(),
            path: skill.path.clone(),
        },
    ))
}

/// Resolve a typed choice by exact value/display label, then by a unique
/// prefix. Ambiguous or unknown input is rejected instead of silently
/// selecting a different setting.
pub(super) fn resolve_choice(input: &str, choices: &[(String, String)]) -> Result<String, String> {
    let query = input.trim().to_ascii_lowercase();
    let exact = choices.iter().find(|(value, label)| {
        value.eq_ignore_ascii_case(&query) || label.eq_ignore_ascii_case(&query)
    });

    if let Some((value, _)) = exact {
        return Ok(value.clone());
    }

    let candidates: Vec<&(String, String)> = choices
        .iter()
        .filter(|(value, label)| {
            value.to_ascii_lowercase().starts_with(&query)
                || label.to_ascii_lowercase().starts_with(&query)
        })
        .collect();

    match candidates.as_slice() {
        [(value, _)] => Ok(value.clone()),
        [] => Err(i18n("agent-command-value-unknown").replace("{value}", input)),
        _ => Err(i18n("agent-command-value-ambiguous").replace("{value}", input)),
    }
}

/// Consume the marker set by an accepted backend command. The caller starts
/// command working UI only when this is invoked by a real TurnStarted event.
pub(super) fn claim_command_turn_start(awaiting: &mut bool) -> bool {
    mem::take(awaiting)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteDirection {
    Previous,
    Next,
}

pub(super) fn move_palette_selection(
    current: usize,
    row_count: usize,
    direction: PaletteDirection,
) -> Option<usize> {
    if row_count == 0 {
        return None;
    }

    Some(match direction {
        PaletteDirection::Previous if current == 0 => row_count - 1,
        PaletteDirection::Previous => current.min(row_count - 1) - 1,
        PaletteDirection::Next => (current + 1) % row_count,
    })
}

pub(super) fn next_session_epoch(current: u64) -> u64 {
    current.wrapping_add(1)
}

pub(super) fn is_current_session_epoch(current: u64, event_epoch: u64) -> bool {
    current == event_epoch
}

/// Reset command-only session state while keeping provider history and the
/// tab's history-dismissal choice outside this function untouched.
#[allow(clippy::too_many_arguments)]
pub(super) fn reset_command_runtime<A, T>(
    commands_ready: bool,
    pending_approval: &mut Option<A>,
    provider_commands: &mut Vec<SlashCommandInfo>,
    provider_commands_ready: &mut bool,
    queue: &mut VecDeque<T>,
    awaiting_turn: &mut bool,
    palette_selected: &mut usize,
    palette_dismissed: &mut bool,
) {
    *pending_approval = None;
    provider_commands.clear();
    *provider_commands_ready = commands_ready;
    queue.clear();
    *awaiting_turn = false;
    *palette_selected = 0;
    *palette_dismissed = false;
}

#[cfg(test)]
mod tests;
