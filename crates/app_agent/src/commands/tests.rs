use crate::RecentSessionsMode;
use crate::commands::*;

#[test]
fn replaced_session_epoch_rejects_expected_old_output_and_eof() {
    let old_epoch = 41;
    let current_epoch = next_session_epoch(old_epoch);
    assert!(!is_current_session_epoch(current_epoch, old_epoch));
    assert!(is_current_session_epoch(current_epoch, current_epoch));
}

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
fn local_resume_replaces_the_provider_catalog_entry() {
    let merged = merge_catalog(
        local_commands(),
        Vec::new(),
        vec![info("resume", SlashCommandSource::Provider)],
    );
    let resume = merged
        .iter()
        .find(|command| command.name == "resume")
        .unwrap();

    assert_eq!(resume.source, SlashCommandSource::Local);
    assert_eq!(resume.run_policy, SlashCommandRunPolicy::IdleOnly);
}

#[test]
fn recent_sessions_open_explicitly_after_a_conversation_starts() {
    assert!(RecentSessionsMode::Automatic.is_visible(true, 1));
    assert!(!RecentSessionsMode::Automatic.is_visible(false, 1));
    assert!(RecentSessionsMode::Open.is_visible(false, 1));
    assert!(!RecentSessionsMode::Hidden.is_visible(true, 1));
    assert!(!RecentSessionsMode::Loading.is_visible(true, 1));
    assert!(!RecentSessionsMode::Open.is_visible(false, 0));
}

#[test]
fn blank_tab_history_stays_open_on_outside_click() {
    assert!(!RecentSessionsMode::Automatic.dismisses_on_outside_click());
    assert!(RecentSessionsMode::Open.dismisses_on_outside_click());
}

#[test]
fn filter_orders_exact_prefix_then_substring_stably() {
    let catalog = vec![
        info("preview", SlashCommandSource::Provider),
        info("review", SlashCommandSource::Provider),
        info("review-file", SlashCommandSource::Provider),
    ];

    let names: Vec<String> = filter_palette_catalog(&catalog, &[], "review")
        .into_iter()
        .filter_map(|entry| match entry {
            PaletteCatalogEntry::Command(command) => Some(command.name.clone()),
            PaletteCatalogEntry::Skill(_) => None,
        })
        .collect();

    assert_eq!(names, vec!["review", "review-file", "preview"]);
    assert!(filter_palette_catalog(&catalog, &[], "missing").is_empty());
}

/// Ranking compares raw bytes with ASCII case folding rather than lowercasing
/// both sides into fresh strings, so an uppercase query must still reach every
/// rank. Getting this wrong would leave a typed `/Review` matching nothing.
#[test]
fn filter_ranks_ignore_case_on_both_sides() {
    let catalog = vec![
        info("preview", SlashCommandSource::Provider),
        info("review", SlashCommandSource::Provider),
        info("review-file", SlashCommandSource::Provider),
    ];
    let mut mixed = skill("Browser:Control", "C:\\p\\SKILL.md", "system", true);
    mixed.description = "Drives a REVIEW browser".into();

    let names: Vec<String> = filter_palette_catalog(&catalog, &[mixed], "ReViEw")
        .into_iter()
        .map(|entry| match entry {
            PaletteCatalogEntry::Command(command) => command.name.clone(),
            PaletteCatalogEntry::Skill(skill) => skill.name.clone(),
        })
        .collect();

    assert_eq!(
        names,
        vec!["review", "review-file", "preview", "Browser:Control"]
    );
}

#[test]
fn combined_palette_ranks_skill_exact_matches_before_command_substrings() {
    let commands = vec![
        info("preview", SlashCommandSource::Provider),
        info("status", SlashCommandSource::Local),
    ];
    let skills = vec![
        skill("review", "C:\\user\\review\\SKILL.md", "user", true),
        skill("review", "C:\\repo\\review\\SKILL.md", "repo", true),
        skill(
            "browser:control",
            "C:\\plugins\\browser\\SKILL.md",
            "system",
            true,
        ),
    ];

    let results = filter_palette_catalog(&commands, &skills, "review");

    assert!(matches!(
        &results[0],
        PaletteCatalogEntry::Skill(skill) if skill.path == "C:\\user\\review\\SKILL.md"
    ));
    assert!(matches!(
        &results[1],
        PaletteCatalogEntry::Skill(skill) if skill.path == "C:\\repo\\review\\SKILL.md"
    ));
    assert!(matches!(
        &results[2],
        PaletteCatalogEntry::Command(command) if command.name == "preview"
    ));
    assert!(matches!(
        &results[3],
        PaletteCatalogEntry::Skill(skill) if skill.name == "browser:control"
    ));

    let all = filter_palette_catalog(&commands, &skills, "");
    assert_eq!(all.len(), commands.len() + skills.len());
    assert!(matches!(all[0], PaletteCatalogEntry::Command(_)));
    assert!(matches!(all[2], PaletteCatalogEntry::Skill(_)));
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
    let mut ready = true;
    let mut queue = VecDeque::from(["compact"]);
    let mut awaiting = true;
    let mut selected = 3;
    let mut dismissed = true;
    let history_dismissed = true;
    let history = vec!["persisted session"];

    reset_command_runtime(
        false,
        &mut provider,
        &mut ready,
        &mut queue,
        &mut awaiting,
        &mut selected,
        &mut dismissed,
    );

    assert!(provider.is_empty());
    assert!(!ready);
    assert!(queue.is_empty());
    assert!(!awaiting);
    assert_eq!(selected, 0);
    assert!(!dismissed);
    assert!(history_dismissed);
    assert_eq!(history, vec!["persisted session"]);
    assert_eq!(next_session_epoch(7), 8);
    assert!(is_current_session_epoch(8, 8));
    assert!(!is_current_session_epoch(8, 7));
}

fn skill(name: &str, path: &str, scope: &str, enabled: bool) -> SkillInfo {
    SkillInfo {
        name: name.into(),
        description: format!("Use {name} for reviews"),
        path: path.into(),
        scope: scope.into(),
        enabled,
        display_name: None,
    }
}

#[test]
fn skill_filter_ranks_fields_and_preserves_duplicate_paths() {
    let mut plugin = skill(
        "browser:control-in-app-browser",
        "C:\\plugins\\browser\\SKILL.md",
        "system",
        true,
    );
    plugin.display_name = Some("Browser Control".into());
    let catalog = vec![
        skill("review", "C:\\user\\review\\SKILL.md", "user", true),
        skill("review", "C:\\repo\\review\\SKILL.md", "repo", false),
        plugin,
    ];

    let duplicate_results = filter_skill_catalog(&catalog, "review");
    assert_eq!(duplicate_results.len(), 3);
    assert_eq!(duplicate_results[0].name, "review");
    assert_eq!(duplicate_results[1].name, "review");
    assert_ne!(duplicate_results[0].path, duplicate_results[1].path);
    assert_eq!(duplicate_results[2].name, "browser:control-in-app-browser");
    assert_eq!(
        filter_skill_catalog(&catalog, "browser control")[0].name,
        "browser:control-in-app-browser"
    );
    assert_eq!(filter_skill_catalog(&catalog, "missing"), Vec::new());
}

#[test]
fn skill_binding_survives_argument_edits_but_not_token_edits() {
    let mut binding = Some(SkillReference {
        name: "review".into(),
        path: "C:\\skills\\review\\SKILL.md".into(),
    });

    reconcile_skill_binding("$review focus on parsing", &mut binding);
    assert!(binding.is_some());
    reconcile_skill_binding("$other focus on parsing", &mut binding);
    assert!(binding.is_none());
}

#[test]
fn skill_binding_validation_rejects_stale_and_disabled_paths() {
    let binding = SkillReference {
        name: "review".into(),
        path: "C:\\skills\\review\\SKILL.md".into(),
    };
    let enabled = SkillCatalog {
        skills: vec![skill("review", &binding.path, "user", true)],
        errors: Vec::new(),
    };
    let disabled = SkillCatalog {
        skills: vec![skill("review", &binding.path, "user", false)],
        errors: Vec::new(),
    };

    assert_eq!(
        validate_skill_binding("$review changes", Some(&binding), Some(&enabled)),
        Ok(Some(binding.clone()))
    );
    assert!(validate_skill_binding("$review changes", Some(&binding), Some(&disabled)).is_err());
    assert!(
        validate_skill_binding(
            "$review changes",
            Some(&binding),
            Some(&SkillCatalog::default())
        )
        .is_err()
    );
    assert_eq!(
        validate_skill_binding("$review changes", None, Some(&enabled)),
        Ok(None)
    );
}

#[test]
fn skill_selection_keeps_exact_path_and_rejects_disabled_rows() {
    let enabled = skill(
        "browser:control",
        "C:\\plugins\\browser\\SKILL.md",
        "system",
        true,
    );
    let disabled = skill(
        "browser:control",
        "C:\\repo\\browser\\SKILL.md",
        "repo",
        false,
    );

    assert_eq!(
        prepare_skill_selection(&enabled),
        Ok((
            "$browser:control ".into(),
            SkillReference {
                name: "browser:control".into(),
                path: "C:\\plugins\\browser\\SKILL.md".into(),
            }
        ))
    );
    assert!(prepare_skill_selection(&disabled).is_err());
}

#[test]
fn skill_prefix_covers_only_the_leading_token() {
    assert_eq!(parse_skill_prefix("$"), Some(String::new()));
    assert_eq!(parse_skill_prefix("$Review"), Some("review".to_string()));
    assert_eq!(parse_skill_prefix("$review changes"), None);
    assert_eq!(parse_skill_prefix("/review"), None);
    assert_eq!(parse_skill_prefix("cost is $5"), None);
}
