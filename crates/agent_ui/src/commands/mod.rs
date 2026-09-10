//! Pure slash-command parsing and catalog logic for the agent composer.

use nmt_agent::catalog::{
    ChoiceError, SkillError, prepare_skill_selection as prepare_core_skill_selection,
    resolve_choice as resolve_core_choice, validate_skill_binding as validate_core_skill_binding,
};
pub(super) use nmt_agent::catalog::{
    merge_catalog, parse_skill_prefix, parse_slash_command, reconcile_skill_binding,
};
use nmt_agent::chat::{
    SkillCatalog, SkillInfo, SkillReference, SlashCommandArguments, SlashCommandInfo,
    SlashCommandRunPolicy, SlashCommandSource,
};
use nmt_i18n::i18n;

pub(super) fn validate_skill_binding(
    input: &str,
    binding: Option<&SkillReference>,
    catalog: Option<&SkillCatalog>,
) -> Result<Option<SkillReference>, String> {
    validate_core_skill_binding(input, binding, catalog).map_err(|error| match error {
        SkillError::Loading => i18n("agent-command-skill-loading").to_owned(),
        SkillError::Unavailable(name) => {
            i18n("agent-command-skill-unavailable").replace("{name}", &name)
        }
        SkillError::Disabled(name) => i18n("agent-command-skill-disabled").replace("{name}", &name),
    })
}
pub(super) fn prepare_skill_selection(
    skill: &SkillInfo,
) -> Result<(String, SkillReference), String> {
    prepare_core_skill_selection(skill)
        .map_err(|_| i18n("agent-command-skill-disabled-by-codex").replace("{name}", &skill.name))
}
pub(super) fn resolve_choice(input: &str, choices: &[(String, String)]) -> Result<String, String> {
    resolve_core_choice(input, choices).map_err(|error| {
        i18n(match error {
            ChoiceError::Unknown => "agent-command-value-unknown",
            ChoiceError::Ambiguous => "agent-command-value-ambiguous",
        })
        .replace("{value}", input)
    })
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteCatalogEntry<'a> {
    Command(&'a SlashCommandInfo),
    Skill(&'a SkillInfo),
}

/// Case-insensitive `starts_with` over ASCII, without lowercasing either side
/// into a fresh `String`. UTF-8 is self-synchronizing, so comparing raw bytes
/// can only match on a character boundary.
fn starts_with_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .get(..needle.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Case-insensitive `contains` over ASCII, on the same terms as
/// [`starts_with_ignore_ascii_case`]. An empty needle is contained by
/// everything, which also keeps `windows` away from a zero width.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    needle.is_empty()
        || haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn text_match_rank<'a>(fields: impl IntoIterator<Item = &'a str>, query: &str) -> Option<usize> {
    // Each field is read three times at most and the ranks are ordered, so the
    // fields are collected once rather than re-walking the iterator per rank.
    let fields = fields.into_iter().collect::<Vec<_>>();

    if fields.iter().any(|field| field.eq_ignore_ascii_case(query)) {
        Some(0)
    } else if fields
        .iter()
        .any(|field| starts_with_ignore_ascii_case(field, query))
    {
        Some(1)
    } else if fields
        .iter()
        .any(|field| contains_ignore_ascii_case(field, query))
    {
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
pub(super) fn filter_palette_catalog<'a>(
    commands: &'a [SlashCommandInfo],
    skills: &'a [SkillInfo],
    query: &str,
) -> Vec<PaletteCatalogEntry<'a>> {
    let mut buckets = [Vec::new(), Vec::new(), Vec::new()];

    for command in commands {
        if let Some(rank) = command_match_rank(command, query) {
            buckets[rank].push(PaletteCatalogEntry::Command(command));
        }
    }

    for skill in skills {
        if let Some(rank) = skill_match_rank(skill, query) {
            buckets[rank].push(PaletteCatalogEntry::Skill(skill));
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

#[cfg(test)]
mod tests;
