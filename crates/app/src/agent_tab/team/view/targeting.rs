use std::collections::BTreeSet;

use gpui::{Anchor, Entity, IntoElement};
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use nmt_agent::team::content::UserInput;
use nmt_agent::team::discussion::{DiscussionMode, DiscussionState};
use nmt_agent::team::identity::{DiscussionId, MemberId};
use nmt_agent::team::room::Room;
use rust_i18n::t;

use crate::agent_tab::team::TeamCommand;
use crate::agent_tab::team::view::TeamPane;
use crate::agent_tab::thread_controls::settings_pill;

/// Who the next message from the team composer goes to, and how: straight to
/// the selected members, or as a discussion among them whose report one
/// author writes, with or without that author moderating.
pub(super) struct DiscussionTargeting {
    selected: BTreeSet<MemberId>,
    author: Option<MemberId>,
    moderated: bool,
    discussion_mode: bool,
}

/// A discussion message that cannot be sent because nobody is chosen to write
/// its report.
pub(super) struct MissingAuthor;

impl DiscussionTargeting {
    /// Targeting that matches `room` as it stands: every member still taking
    /// part is a recipient, and a discussion under way keeps its own author
    /// and mode.
    pub(super) fn from_room(room: &Room) -> Self {
        let selected = room
            .members()
            .iter()
            .filter(|member| !member.excluded())
            .map(|member| member.id())
            .collect::<BTreeSet<_>>();

        let active = room
            .discussions()
            .iter()
            .find(|run| run.state() != DiscussionState::Completed);

        let author = active
            .map(|run| run.mode().report_author())
            .or_else(|| room.members().first().map(|member| member.id()));

        let moderated =
            active.is_some_and(|run| matches!(run.mode(), DiscussionMode::Moderated { .. }));

        Self {
            selected,
            author,
            moderated,
            discussion_mode: active.is_some(),
        }
    }

    pub(super) fn selected(&self) -> &BTreeSet<MemberId> {
        &self.selected
    }

    pub(super) fn author(&self) -> Option<MemberId> {
        self.author
    }

    pub(super) fn moderated(&self) -> bool {
        self.moderated
    }

    /// Take a newly added member in as a recipient, and as the author when
    /// there is none yet.
    pub(super) fn member_added(&mut self, member: MemberId) {
        self.selected.insert(member);

        self.author.get_or_insert(member);
    }

    /// The command that sends `input`. While a discussion runs it corrects
    /// that discussion; otherwise it starts one or messages the recipients
    /// directly, depending on the chosen mode.
    pub(super) fn command(
        &self,
        input: UserInput,
        discussion_running: bool,
    ) -> Result<TeamCommand, MissingAuthor> {
        if discussion_running {
            return Ok(TeamCommand::Correction(input));
        }

        if !self.discussion_mode {
            return Ok(TeamCommand::Direct {
                input,
                recipients: self.selected.iter().copied().collect(),
            });
        }

        let author = self.author.ok_or(MissingAuthor)?;

        Ok(TeamCommand::Start {
            input,
            participants: self.selected.iter().copied().collect(),
            mode: discussion_mode(self.moderated, author),
        })
    }

    /// The command that moves running `discussion` to the given moderation
    /// and author, or `None` when no author is chosen.
    pub(super) fn change_mode(
        discussion: DiscussionId,
        moderated: bool,
        author: Option<MemberId>,
    ) -> Option<TeamCommand> {
        author.map(|author| TeamCommand::ChangeMode {
            discussion,
            mode: discussion_mode(moderated, author),
        })
    }

    /// Choose a send mode: `0` messages directly, `1` starts a discussion
    /// with a fixed report author, `2` one that author moderates.
    pub(super) fn set_mode(&mut self, selection: usize) {
        self.discussion_mode = selection != 0;
        self.moderated = selection == 2;
    }

    pub(super) fn set_author(&mut self, author: MemberId) {
        self.author = Some(author);
    }

    fn toggle_recipient(&mut self, member: MemberId) {
        if !self.selected.remove(&member) {
            self.selected.insert(member);
        }
    }

    /// The recipients pill: a check per member, the member conversations to
    /// inspect, and the way to add a member.
    pub(super) fn render_recipients(
        &self,
        room: &Room,
        pane: Entity<TeamPane>,
    ) -> impl IntoElement + use<> {
        let members: Vec<_> = room
            .members()
            .iter()
            .map(|member| (member.id(), member.name().to_owned(), member.excluded()))
            .collect();

        let selected = self.selected.clone();

        let names = members
            .iter()
            .filter(|(id, _, _)| selected.contains(id))
            .map(|(_, name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let recipient_label = if names.is_empty() {
            t!("team-select-members").into_owned()
        } else {
            names
        };

        settings_pill(Button::new("team-recipients"))
            .label(recipient_label)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let mut menu = menu;

                for (id, name, excluded) in &members {
                    let id = *id;
                    let pane = pane.clone();

                    menu = menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(selected.contains(&id))
                            .disabled(*excluded)
                            .on_click(move |_, _, cx| {
                                pane.update(cx, |pane, cx| {
                                    pane.targeting.toggle_recipient(id);

                                    cx.notify();
                                });
                            }),
                    );
                }

                if !members.is_empty() {
                    let inspect = members.clone();
                    let view = pane.clone();

                    menu = menu.separator().submenu(
                        t!("team-conversations"),
                        window,
                        cx,
                        move |menu, _, _| {
                            let mut menu = menu;

                            for (id, name, _) in &inspect {
                                let id = *id;
                                let view = view.clone();

                                menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                                    move |_, window, cx| {
                                        view.update(cx, |pane, cx| {
                                            pane.inspect_member(id, window, cx)
                                        });
                                    },
                                ));
                            }

                            menu
                        },
                    );
                }

                let pane = pane.clone();

                menu.item(PopupMenuItem::new(t!("team-add-member")).on_click(
                    move |_, window, cx| {
                        pane.update(cx, |pane, cx| {
                            pane.member_draft
                                .open_member_form(&pane.runtime, window, cx)
                        });
                    },
                ))
            })
    }

    /// The mode pill: direct, fixed-author, or moderated sending, and the
    /// report author. A running discussion cannot go back to direct sending.
    pub(super) fn render_mode(
        &self,
        room: &Room,
        discussion_running: bool,
        pane: Entity<TeamPane>,
    ) -> impl IntoElement + use<> {
        let mode_label = if !self.discussion_mode {
            t!("team-send-direct")
        } else if self.moderated {
            t!("team-moderated")
        } else {
            t!("team-fixed")
        };

        let authors: Vec<_> = room
            .members()
            .iter()
            .filter(|member| !member.excluded())
            .map(|member| (member.id(), member.name().to_owned()))
            .collect();

        let author = self.author;
        let moderated = self.moderated;
        let discussion_mode = self.discussion_mode;

        settings_pill(Button::new("team-mode"))
            .label(mode_label)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let mut menu = menu;

                for (index, label) in [
                    t!("team-send-direct"),
                    t!("team-fixed"),
                    t!("team-moderated"),
                ]
                .into_iter()
                .enumerate()
                {
                    let pane = pane.clone();

                    let checked = match index {
                        0 => !discussion_mode,
                        1 => discussion_mode && !moderated,
                        _ => discussion_mode && moderated,
                    };

                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(checked)
                            .disabled(index == 0 && discussion_running)
                            .on_click(move |_, _, cx| {
                                pane.update(cx, |pane, cx| pane.select_mode(index, cx));
                            }),
                    );
                }

                let pane = pane.clone();
                let authors = authors.clone();

                menu.separator()
                    .submenu(t!("team-select-author"), window, cx, move |menu, _, _| {
                        let mut menu = menu;

                        for (id, name) in &authors {
                            let id = *id;
                            let pane = pane.clone();

                            menu = menu.item(
                                PopupMenuItem::new(name.clone())
                                    .checked(author == Some(id))
                                    .on_click(move |_, _, cx| {
                                        pane.update(cx, |pane, cx| pane.select_author(id, cx));
                                    }),
                            );
                        }

                        menu
                    })
            })
    }
}

/// A discussion whose report `author` writes, moderated by that author when
/// `moderated` holds.
fn discussion_mode(moderated: bool, author: MemberId) -> DiscussionMode {
    if moderated {
        DiscussionMode::Moderated { moderator: author }
    } else {
        DiscussionMode::Fixed {
            report_author: author,
        }
    }
}
