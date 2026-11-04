//! The workspaces and sessions of connected paired hosts, listed under the
//! local workspaces: each host heads the workspaces it offers, and each of
//! those heads the sessions its tabs show.

use std::collections::{HashMap, HashSet};

use app::agent_tab::AgentKind;
use gpui::prelude::*;
use gpui::{
    AnyElement, Context, ElementId, FontWeight, SharedString, Window, div, px, relative,
    transparent_black,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::{ActiveTheme, IconName, IconNamed, Selectable as _, h_flex, v_flex};
use nmt_config::local_state::TabFold;
use nmt_remote_core::rpc::{SessionInfo, SessionKind, WorkspaceInfo};
use rust_i18n::t;

use crate::ui::composition::{
    HoverActionLayout, HoverActionVisibility, hover_action, sidebar_selection, toolbar_button,
};
use crate::ui::platform_style::{Host, PlatformStyle as _};
use crate::ui::remote::{self, RemoteWorkspace};
use crate::ui::remote_rename::{RenameTarget, open_rename_dialog};
use crate::ui::shell::{InlineRename, InlineRenameSession, InlineRenameStyle, pending_tab_icon};
use crate::ui::tab_bar::menu::tab_icon;
use crate::ui::tab_bar::{fold_block, fold_row, tab_row, tab_row_icon};
use crate::ui::workspace_sidebar::SIDEBAR_ROW_GUTTER;
use crate::ui::workspace_sidebar::list::{
    Disclosure, WORKSPACE_LIST_GAP, WORKSPACE_NAME_TEXT, WORKSPACE_PATH_TEXT, selection_bar,
    tail_preserving_path, workspace_row_button,
};
use crate::ui::{AppWindow, modern_dropdown};
use crate::workspace::workspace_display_label;

/// The heading of the remote section and one block per host under it. Each
/// host's block spaces its rows by the list gap, like the local workspaces
/// above, but owns those gaps itself: a host folding its rows away folds
/// the gaps between them too, which the list's own gap could not do.
#[allow(clippy::too_many_arguments)]
pub(super) fn remote_workspace_blocks(
    heading: AnyElement,
    remote: &[RemoteWorkspace],
    folds: &HashMap<(String, String), TabFold>,
    collapsed_hosts: &HashSet<String>,
    renames: &InlineRenameSession,
    width: f32,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> Vec<AnyElement> {
    let mut blocks = vec![heading];

    for (index, host) in remote.iter().enumerate() {
        // A hidden host hides its workspaces and every session under it, but
        // never touches the workspaces' own folds, which apply again once
        // the host shows.
        let host_shown = !collapsed_hosts.contains(host.id.as_str());

        let is_selected =
            |session: &SessionInfo| host.selected.as_deref() == Some(session.session.as_str());

        // With the session on screen hidden along with the whole host, the
        // host row takes over the selection it would show.
        let host_highlight = !host_shown && host.sessions.iter().any(is_selected);

        let mut rows = vec![host_row(
            index,
            host,
            host_shown,
            host_highlight,
            window,
            cx,
        )];

        let workspaces: &[WorkspaceInfo] = host
            .offers
            .as_ref()
            .map_or(&[], |offers| offers.workspaces.as_slice());

        // Older hosts offer the raw name; sessions carry the display label.
        let labels: Vec<String> = workspaces
            .iter()
            .map(|workspace| workspace_display_label(&workspace.name, &workspace.path))
            .collect();

        // A host that sends workspace ids is matched by them, so workspaces
        // sharing a name keep their own sessions. Older hosts send none,
        // and their sessions fall back to the workspace's name.
        let in_workspace = |session: &SessionInfo, workspace: &WorkspaceInfo, label: &str| {
            session
                .workspace
                .as_ref()
                .is_some_and(|held| match &workspace.id {
                    Some(id) => held.id == *id,
                    None => held.name == label,
                })
        };

        // Sessions no offered workspace holds: tabs of a host without
        // workspaces, of a workspace the host no longer offers, or not yet
        // placed by the host. They sit right under the host row, since after
        // the last workspace they would read as that workspace's sessions.
        let loose = host
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| {
                !workspaces
                    .iter()
                    .zip(&labels)
                    .any(|(workspace, label)| in_workspace(session, workspace, label))
            })
            .collect::<Vec<_>>();

        if !loose.is_empty() {
            rows.extend(list_gap(
                ElementId::Name(format!("remote-loose-gap:{}", host.id.as_str()).into()),
                host_shown,
                window,
                cx,
            ));

            rows.extend(loose.into_iter().filter_map(|(row, session)| {
                fold_row(
                    session_fold_id(host, session),
                    host_shown,
                    window,
                    cx,
                    |cx| session_row(index, row, host, session, renames, cx),
                )
            }));
        }

        for (slot, (workspace, label)) in workspaces.iter().zip(&labels).enumerate() {
            let fold_key = (
                host.id.as_str().to_owned(),
                workspace.id.clone().unwrap_or_else(|| label.clone()),
            );

            let fold = folds.get(&fold_key).copied().unwrap_or_default();

            let sessions = host
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, session)| in_workspace(session, workspace, label))
                .filter_map(|(row, session)| {
                    let shown = host_shown
                        && match fold {
                            TabFold::All => true,
                            TabFold::Awake => !session.pending,
                            TabFold::Collapsed => false,
                        };

                    fold_row(session_fold_id(host, session), shown, window, cx, |cx| {
                        session_row(index, row, host, session, renames, cx)
                    })
                })
                .collect::<Vec<_>>();

            // With the session on screen folded away, its workspace row takes
            // over the selection it would show, unless the host row already
            // has because the whole host is hidden.
            let highlight = host_shown
                && fold == TabFold::Collapsed
                && host
                    .sessions
                    .iter()
                    .any(|session| in_workspace(session, workspace, label) && is_selected(session));

            let fold_id = ElementId::Name(
                format!("remote-workspace-fold:{}:{}", fold_key.0, fold_key.1).into(),
            );

            let row = WorkspaceRow {
                index,
                slot,
                fold,
                fold_key,
                highlight,
            };

            rows.extend(fold_block(
                fold_id,
                host_shown,
                WORKSPACE_LIST_GAP + WORKSPACE_ROW_HEIGHT,
                window,
                cx,
                |window, cx| {
                    v_flex()
                        .w_full()
                        .pt(px(WORKSPACE_LIST_GAP))
                        .child(workspace_row(
                            row, host, workspace, label, width, window, cx,
                        ))
                        .into_any_element()
                },
            ));

            rows.extend(sessions);
        }

        blocks.push(v_flex().w_full().children(rows).into_any_element());
    }

    blocks
}

/// At least the height of a host workspace row, whose height follows its
/// text, and close to it so folding the host shrinks the row at once.
const WORKSPACE_ROW_HEIGHT: f32 = 28.0;

/// The gap the list puts between two of its blocks, kept inside a host's
/// block so it folds away with the rows it separates.
fn list_gap(
    id: ElementId,
    shown: bool,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> Option<AnyElement> {
    fold_block(id, shown, WORKSPACE_LIST_GAP, window, cx, |_, _| {
        div().h(px(WORKSPACE_LIST_GAP)).into_any_element()
    })
}

/// Names a session's fold motion by host and session instead of list
/// position, so a session moving between the host's loose rows and one of
/// its workspaces keeps the motion it is in.
fn session_fold_id(host: &RemoteWorkspace, session: &SessionInfo) -> ElementId {
    ElementId::Name(
        format!(
            "remote-session-fold:{}:{}",
            host.id.as_str(),
            session.session
        )
        .into(),
    )
}

/// A host heading its workspaces. New tabs start from a workspace row, so
/// what they open is tied to a place on the host. The row shows or hides
/// everything the host lists, and still reads as a heading at rest: it
/// takes no fill until the pointer is on it.
fn host_row(
    index: usize,
    host: &RemoteWorkspace,
    shown: bool,
    highlight: bool,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> AnyElement {
    let name: SharedString = host.name.clone().into();
    let selection = sidebar_selection(cx);
    let host_id = host.id.as_str().to_owned();

    // Keyed by host instead of list position, so a host connecting above
    // this one does not hand this row another's reveal.
    let disclosure = Disclosure::new(
        ElementId::Name(format!("remote-host-disclosure:{host_id}").into()),
        if shown {
            TabFold::All
        } else {
            TabFold::Collapsed
        },
        window,
        cx,
    );

    let item = Button::new(("remote-host", index))
        .custom(
            ButtonCustomVariant::new(cx)
                .color(transparent_black())
                .hover(selection.hover_background)
                .active(selection.active_background),
        )
        .w_full()
        .h_auto()
        .min_h(px(24.))
        .line_height(relative(1.5))
        // The name starts on the session rows' icon column until the
        // disclosure mark makes it slide over, as on a workspace row.
        .pl(px(SIDEBAR_ROW_GUTTER))
        .pr(px(SIDEBAR_ROW_GUTTER))
        .py_0p5()
        .accessibility_label(name.clone())
        .tooltip(host.link.clone())
        .selected(highlight)
        // Button resolves selected colors after element styles, so the
        // sidebar-accent pair must be the selected custom variant itself.
        .when(highlight, |this| {
            this.custom(
                ButtonCustomVariant::new(cx)
                    .foreground(selection.active_foreground)
                    .active(selection.active_background),
            )
        })
        .child(
            h_flex()
                .relative()
                .w_full()
                .items_center()
                .child(disclosure.mark(cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .ml(disclosure.name_offset())
                        .truncate()
                        .text_left()
                        .text_size(px(WORKSPACE_NAME_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().sidebar_foreground.opacity(0.7))
                        .child(name),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.sidebar.toggle_remote_host(host_id.clone());

            cx.notify();
        }));

    let renamed = (host.id.clone(), host.name.clone());

    div()
        .id(("remote-host-menu", index))
        .w_full()
        .relative()
        .modern_context_menu(move |menu, _, _| {
            let (id, name) = renamed.clone();

            menu.item(t!("remote-host-rename"), move |window, cx| {
                open_rename_dialog(RenameTarget::Host(id.clone()), name.clone(), window, cx)
            })
            .icon(IconName::PenLine)
        })
        .child(disclosure.hover_area(("remote-host-hover", index), item))
        // After the row itself, because the row's selected fill would
        // otherwise paint over the bar's lane.
        .children((highlight && Host::SIDEBAR_SELECTION_MARK).then(|| selection_bar(cx)))
        .into_any_element()
}

/// A workspace the host offers, set like a local workspace row: its name
/// with the path trailing it, and a hover control that starts a terminal or
/// one of the host's agents there.
fn workspace_row(
    row: WorkspaceRow,
    host: &RemoteWorkspace,
    workspace: &WorkspaceInfo,
    label: &str,
    width: f32,
    window: &mut Window,
    cx: &mut Context<AppWindow>,
) -> AnyElement {
    let WorkspaceRow {
        index,
        slot,
        fold,
        fold_key,
        highlight,
    } = row;

    let id = index * 1000 + slot;
    let menu_host = host.id.clone();
    let path = workspace.path.clone();

    let (agents, terminals) = host
        .offers
        .as_ref()
        .map(|offers| (offers.agents.clone(), offers.terminals.clone()))
        .unwrap_or_default();

    let new_tab = hover_action(
        ("remote-workspace-new-tab", id),
        t!("sidebar-tab-new"),
        HoverActionLayout::Bare,
        HoverActionVisibility::OnGroupHover("remote-ws-item".into()),
        modern_dropdown(
            toolbar_button(("remote-workspace-new-tab-button", id))
                .icon(IconName::Plus)
                .accessibility_label(t!("sidebar-tab-new")),
            move |mut menu, _, _| {
                // With no profile listed to choose from, one entry starts
                // whatever the host's default shell is.
                let choices: Vec<(SharedString, Option<String>)> = if terminals.is_empty() {
                    vec![(t!("settings-remote-new-terminal").into(), None)]
                } else {
                    terminals
                        .iter()
                        .map(|name| (name.clone().into(), Some(name.clone())))
                        .collect()
                };

                for (label, profile) in choices {
                    let terminal_host = menu_host.clone();
                    let path = path.clone();

                    menu = menu
                        .item(label, move |window, cx| {
                            remote::open_terminal_tab(
                                &terminal_host,
                                path.clone(),
                                profile.clone(),
                                window,
                                cx,
                            )
                        })
                        .icon(tab_icon(None, false));
                }

                if !agents.is_empty() {
                    menu = menu.separator();
                }

                for agent in &agents {
                    let agent_host = menu_host.clone();
                    let chosen = agent.clone();
                    let path = path.clone();

                    menu = menu
                        .item(agent.name.clone(), move |window, cx| {
                            remote::open_agent(&agent_host, &chosen, path.clone(), window, cx)
                        })
                        .icon(tab_icon(AgentKind::from_id(&agent.harness), false));
                }

                menu
            },
        ),
    );

    // The same budget a local row gives its path, so a host's paths keep
    // their leaf directory in view the way local ones do.
    let path_budget = (width - 80.0 - 8.0 * label.chars().count() as f32) / 7.0;

    let display_path: SharedString = tail_preserving_path(
        &workspace.path,
        (path_budget.floor().max(0.0) as usize).clamp(8, 64),
    )
    .into();

    let label: SharedString = label.to_owned().into();

    let name = h_flex()
        .w_full()
        .gap_1p5()
        .items_baseline()
        .child(
            div()
                .min_w_0()
                .text_left()
                .text_size(px(WORKSPACE_NAME_TEXT))
                .font_weight(FontWeight::NORMAL)
                .truncate()
                .child(label.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_left()
                .text_size(px(WORKSPACE_PATH_TEXT))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(cx.theme().sidebar_foreground.opacity(0.4))
                .child(display_path),
        );

    let selection = sidebar_selection(cx);

    // Keyed by host and workspace instead of list position, so a host
    // connecting above this one does not hand this row another's reveal.
    let disclosure = Disclosure::new(
        ElementId::Name(format!("remote-disclosure:{}:{}", fold_key.0, fold_key.1).into()),
        fold,
        window,
        cx,
    );

    let item = workspace_row_button(("remote-workspace", id), cx)
        .accessibility_label(label)
        .selected(highlight)
        // Button resolves selected colors after element styles, so the
        // sidebar-accent pair must be the selected custom variant itself.
        .when(highlight, |this| {
            this.custom(
                ButtonCustomVariant::new(cx)
                    .foreground(selection.active_foreground)
                    .active(selection.active_background),
            )
        })
        .group("remote-ws-item")
        // The name starts on the session rows' icon column until the
        // disclosure mark makes it slide over.
        .pl(px(SIDEBAR_ROW_GUTTER))
        .child(
            h_flex()
                .relative()
                .w_full()
                .gap_1p5()
                .items_center()
                .child(disclosure.mark(cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .ml(disclosure.name_offset())
                        .child(name),
                )
                .child(new_tab),
        )
        // Opening a session is done from its own row, so the workspace row
        // is left to fold them.
        .on_click(cx.listener(move |this, _, _, cx| {
            let (host, workspace) = fold_key.clone();

            this.sidebar.cycle_remote_fold(host, workspace);

            cx.notify();
        }));

    div()
        .w_full()
        .relative()
        .child(disclosure.hover_area(("remote-workspace-hover", id), item))
        // After the row itself, because the row's selected fill would
        // otherwise paint over the bar's lane.
        .children((highlight && Host::SIDEBAR_SELECTION_MARK).then(|| selection_bar(cx)))
        .into_any_element()
}

/// Where a host workspace's row sits in the list and how its sessions fold.
struct WorkspaceRow {
    /// The host's position among the connected hosts.
    index: usize,

    /// The workspace's position among those its host offers.
    slot: usize,

    fold: TabFold,

    /// The host and workspace the sidebar keeps the fold under.
    fold_key: (String, String),

    /// The session on screen is in this workspace and folded away.
    highlight: bool,
}

/// A session on a host, drawn as a local tab row: opening it follows it in a
/// tab here, or shows the tab already following it. The session whose tab is
/// on screen is marked the way the local list marks its tab, since that tab
/// sits in no list of its own.
///
/// A session this computer follows acts like a local tab: it is renamed and
/// closed from its menu, closed from its hover control, and can also be
/// disconnected from, which closes only what follows it here. A session it
/// does not follow offers only connecting to it: renaming or ending something
/// on another computer is left to whoever has it open.
fn session_row(
    index: usize,
    row: usize,
    host: &RemoteWorkspace,
    session: &SessionInfo,
    renames: &InlineRenameSession,
    cx: &mut Context<AppWindow>,
) -> AnyElement {
    let icon = match session.kind {
        SessionKind::Agent => tab_icon(
            session.harness.as_deref().and_then(AgentKind::from_id),
            false,
        ),
        SessionKind::Terminal | SessionKind::Unknown => tab_icon(None, false),
    };

    let key = index * 1000 + row;
    let host_id = host.id.clone();
    let opened = session.clone();
    let title: SharedString = session.title.clone().into();
    let selected = host.selected.as_deref() == Some(session.session.as_str());
    let followed = host.followed.contains(&session.session);

    let label: AnyElement = match renames.remote_input(&host.id, &session.session).cloned() {
        Some(input) => {
            let rename_shell = cx.entity();

            InlineRename::new(
                ("remote-session-rename", key),
                title.clone(),
                input,
                InlineRenameStyle::SidebarTab,
                move |window, cx| {
                    rename_shell
                        .update(cx, |this, cx| this.finish_remote_rename(false, window, cx));
                },
            )
            .into_any_element()
        }
        None => div()
            .flex_1()
            .overflow_hidden()
            .truncate()
            .child(title.clone())
            .into_any_element(),
    };

    let close = {
        let host_id = host.id.clone();
        let host_name = host.name.clone();
        let closed = session.clone();

        hover_action(
            ("remote-session-close", key),
            t!("tabbar-menu-close"),
            HoverActionLayout::Inline,
            HoverActionVisibility::OnGroupHover("remote-session".into()),
            "\u{00d7}",
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();

            this.request_close_remote_session(
                host_id.clone(),
                host_name.clone(),
                closed.clone(),
                window,
                cx,
            );
        }))
    };

    // A session still asleep on its host wears the faded row and moon a
    // local tab waiting to start does, so both lists tell running tabs from
    // sleeping ones the same way.
    let glyph = match session.pending {
        true => pending_tab_icon(("remote-session-pending", key)).into_any_element(),
        false => icon.into_any_element(),
    };

    let row = tab_row(("remote-session", key), title, selected, cx)
        .group("remote-session")
        .when(session.pending, |this| this.opacity(0.6))
        .child(tab_row_icon(glyph))
        .child(label)
        .when(followed, |row| row.child(close))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_remote_session(&host_id, &opened, window, cx)
        }));

    let menu_shell = cx.entity();
    let menu_host = host.id.clone();
    let menu_host_name = host.name.clone();
    let menu_session = session.clone();

    div()
        .id(("remote-session-menu", key))
        .w_full()
        .modern_context_menu(move |menu, _, _| {
            if !followed {
                let connect_shell = menu_shell.clone();
                let connected = (menu_host.clone(), menu_session.clone());

                return menu
                    .item(t!("remote-session-connect"), move |window, cx| {
                        let (host, session) = connected.clone();

                        connect_shell.update(cx, |this, cx| {
                            this.open_remote_session(&host, &session, window, cx)
                        });
                    })
                    .icon(PlugIcon);
            }

            let rename_shell = menu_shell.clone();
            let disconnect_shell = menu_shell.clone();
            let close_shell = menu_shell.clone();

            let renamed = (menu_host.clone(), menu_session.clone());

            let disconnected = (
                menu_host.clone(),
                menu_host_name.clone(),
                menu_session.session.clone(),
            );

            let closed = (
                menu_host.clone(),
                menu_host_name.clone(),
                menu_session.clone(),
            );

            menu.item(t!("tabbar-menu-rename"), move |window, cx| {
                let (host, session) = renamed.clone();

                rename_shell.update(cx, |this, cx| {
                    this.start_remote_rename(host, session, window, cx)
                });
            })
            .icon(IconName::PenLine)
            .item(t!("remote-session-disconnect"), move |window, cx| {
                let (host, name, session) = disconnected.clone();

                disconnect_shell.update(cx, |this, cx| {
                    this.request_disconnect_session(host, name, session, window, cx)
                });
            })
            .icon(UnplugIcon)
            .item(t!("tabbar-menu-close"), move |window, cx| {
                let (host, name, session) = closed.clone();

                close_shell.update(cx, |this, cx| {
                    this.request_close_remote_session(host, name, session, window, cx)
                });
            })
            .icon(IconName::Close)
        })
        .child(row)
        .into_any_element()
}

/// Plug pulled from its socket (`assets/icons/unplug.svg`), for leaving a
/// session that keeps running on its host.
struct UnplugIcon;

impl IconNamed for UnplugIcon {
    fn path(self) -> SharedString {
        "icons/unplug.svg".into()
    }
}

/// Plug ready to go in (`assets/icons/plug.svg`), for opening a session that
/// runs on its host.
struct PlugIcon;

impl IconNamed for PlugIcon {
    fn path(self) -> SharedString {
        "icons/plug.svg".into()
    }
}
