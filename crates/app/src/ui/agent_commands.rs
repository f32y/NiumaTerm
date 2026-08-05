//! Pure slash-command parsing and catalog logic for the agent composer.

use std::collections::{HashSet, VecDeque};
use std::mem;

use nmt_agent_utils::chat::{
    SlashCommandArguments, SlashCommandInfo, SlashCommandRunPolicy, SlashCommandSource,
};

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

pub(super) fn local_commands() -> Vec<SlashCommandInfo> {
    vec![
        command(
            "new",
            "Start a new conversation",
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::IdleOnly,
        ),
        command(
            "clear",
            "Clear this conversation and start over",
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::IdleOnly,
        ),
        command(
            "model",
            "Choose the model for subsequent turns",
            Some("<model>"),
            SlashCommandArguments::Choices,
            SlashCommandRunPolicy::Immediate,
        ),
        command(
            "permissions",
            "Choose the approval or permission policy",
            Some("<policy>"),
            SlashCommandArguments::Choices,
            SlashCommandRunPolicy::Immediate,
        ),
        command(
            "status",
            "Show known session settings and state",
            None,
            SlashCommandArguments::None,
            SlashCommandRunPolicy::Immediate,
        ),
    ]
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

/// Exact matches precede prefixes, which precede substring matches. Within
/// each bucket the merged catalog order remains stable.
pub(super) fn filter_catalog(catalog: &[SlashCommandInfo], query: &str) -> Vec<SlashCommandInfo> {
    let query = query.to_ascii_lowercase();
    let mut exact = Vec::new();
    let mut prefix = Vec::new();
    let mut substring = Vec::new();

    for command in catalog {
        if command.name == query {
            exact.push(command.clone());
        } else if command.name.starts_with(&query) {
            prefix.push(command.clone());
        } else if command.name.contains(&query) {
            substring.push(command.clone());
        }
    }

    exact.extend(prefix);
    exact.extend(substring);
    exact
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
        [] => Err(format!("Unknown value: {input}")),
        _ => Err(format!("Ambiguous value: {input}")),
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

/// Reset command-only session state while keeping provider history and the
/// tab's history-dismissal choice outside this function untouched.
pub(super) fn reset_command_runtime<A, T>(
    is_codex: bool,
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
    *provider_commands_ready = is_codex;
    queue.clear();
    *awaiting_turn = false;
    *palette_selected = 0;
    *palette_dismissed = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, source: SlashCommandSource) -> SlashCommandInfo {
        SlashCommandInfo {
            name: name.to_string(),
            description: name.to_string(),
            argument_hint: None,
            source,
            arguments: SlashCommandArguments::None,
            run_policy: SlashCommandRunPolicy::Immediate,
        }
    }

    #[test]
    fn parser_only_claims_a_leading_slash_and_preserves_argument_text() {
        assert_eq!(parse_slash_command("explain a/b"), None);
        assert_eq!(
            parse_slash_command("/review   path with spaces  "),
            Some(ParsedSlashCommand {
                name: "review".into(),
                arguments: "path with spaces  ".into(),
                has_argument_separator: true,
            })
        );
    }

    #[test]
    fn merge_normalizes_names_and_honors_layer_precedence() {
        let merged = merge_catalog(
            vec![info("status", SlashCommandSource::Local)],
            vec![info("/Status", SlashCommandSource::Adapter)],
            vec![
                info(" status ", SlashCommandSource::Provider),
                info("review", SlashCommandSource::Provider),
                info("not valid", SlashCommandSource::Provider),
            ],
        );

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].source, SlashCommandSource::Local);
        assert_eq!(merged[1].name, "review");
    }

    #[test]
    fn filter_orders_exact_prefix_then_substring_stably() {
        let catalog = vec![
            info("preview", SlashCommandSource::Provider),
            info("review", SlashCommandSource::Provider),
            info("review-file", SlashCommandSource::Provider),
        ];

        let names: Vec<String> = filter_catalog(&catalog, "review")
            .into_iter()
            .map(|command| command.name)
            .collect();

        assert_eq!(names, vec!["review", "review-file", "preview"]);
        assert!(filter_catalog(&catalog, "missing").is_empty());
    }

    #[test]
    fn enum_choice_requires_an_exact_or_unique_prefix_match() {
        let choices = vec![
            ("default".into(), "Default".into()),
            ("danger-full-access".into(), "Danger Full Access".into()),
            ("deny".into(), "Deny".into()),
        ];

        assert_eq!(resolve_choice("DEFAULT", &choices), Ok("default".into()));
        assert_eq!(
            resolve_choice("dang", &choices),
            Ok("danger-full-access".into())
        );
        assert!(resolve_choice("d", &choices).is_err());
        assert!(resolve_choice("unknown", &choices).is_err());
    }

    #[test]
    fn command_working_begins_only_when_a_pending_command_gets_turn_started() {
        let mut local_command = false;
        assert!(!claim_command_turn_start(&mut local_command));

        let mut provider_command = true;
        assert!(claim_command_turn_start(&mut provider_command));
        assert!(!provider_command);
    }

    #[test]
    fn palette_direction_navigation_wraps_and_handles_catalog_changes() {
        assert_eq!(
            move_palette_selection(0, 5, PaletteDirection::Previous),
            Some(4)
        );
        assert_eq!(
            move_palette_selection(4, 5, PaletteDirection::Next),
            Some(0)
        );
        assert_eq!(
            move_palette_selection(8, 3, PaletteDirection::Previous),
            Some(1)
        );
        assert_eq!(move_palette_selection(0, 0, PaletteDirection::Next), None);
    }

    #[test]
    fn clear_resets_command_runtime_without_owning_history_state() {
        let mut provider = vec![info("review", SlashCommandSource::Provider)];
        let mut approval = Some("command approval");
        let mut ready = true;
        let mut queue = VecDeque::from(["compact"]);
        let mut awaiting = true;
        let mut selected = 3;
        let mut dismissed = true;
        let history_dismissed = true;
        let history = vec!["persisted session"];

        reset_command_runtime(
            false,
            &mut approval,
            &mut provider,
            &mut ready,
            &mut queue,
            &mut awaiting,
            &mut selected,
            &mut dismissed,
        );

        assert!(approval.is_none());
        assert!(provider.is_empty());
        assert!(!ready);
        assert!(queue.is_empty());
        assert!(!awaiting);
        assert_eq!(selected, 0);
        assert!(!dismissed);
        assert!(history_dismissed);
        assert_eq!(history, vec!["persisted session"]);
        assert_eq!(next_session_epoch(7), 8);
    }
}
