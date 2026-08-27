use gpui::{ClipboardItem, Role, relative};

use crate::ui::workspace_sidebar::*;

impl Sidebar {
    /// One sidebar workspace item: a selectable button with busy indicator,
    /// name/cwd lines, hover-close, and a right-click menu (Rename / Close).
    /// While this workspace is being renamed (`rename` matches its id), the
    /// name line is replaced by the rename input.
    pub(super) fn render_item(
        &self,
        idx: usize,
        ws: &WorkspaceSummary,
        rename: Option<&(WorkspaceId, Entity<InputState>)>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let settings_entry = ws.kind == WorkspaceKind::Settings;
        let selection = sidebar_selection(cx);
        // In the vertical tab-bar style every tab of this workspace is on
        // screen as its own row carrying its own status mark and progress, so
        // the workspace's aggregate of them would say the same thing twice.
        // The status column keeps its width either way: the tab rows place
        // their marks in that lane.
        let vertical_tabs = cx.global::<AppSettings>().tab_bar_style == TabBarStyle::Vertical;
        let highlight_active = ws.active && !vertical_tabs;
        let (glyphs, status_label) = workspace_status_glyphs(
            ws.agent_status,
            ws.terminal_activity,
            ("workspace-busy", idx),
            cx,
        );

        let indicator = v_flex()
            .id(("workspace-status", idx))
            // The column's width is fixed so an idle workspace can suppress its
            // glyphs without shifting its name relative to active neighbours;
            // the height follows its contents so a stacked pair centers as a
            // group and a lone glyph centers on its own.
            .w_4()
            .flex_none()
            .gap_0p5()
            .items_center()
            .justify_center()
            .when(!vertical_tabs, |this| {
                this.aria_label(status_label.clone()).children(glyphs)
            })
            .into_any_element();

        let ws_id = ws.id;

        let renaming = rename
            .filter(|(id, _)| *id == ws_id)
            .map(|(_, input)| input.clone());

        let controls: AnyElement = if vertical_tabs && !settings_entry {
            // This row heads the workspace's own tab list here, so its control
            // adds a tab to that list; closing moves to the context menu. The
            // press activates the workspace before the menu opens, so the
            // profile the user picks lands in the workspace they clicked
            // (a tab always opens in the active workspace). Popover stops the
            // press from reaching the row behind it, so the activation has to
            // run on the capture side of the mouse-down.
            let menu_shell = cx.entity();
            hover_action(
                ("workspace-new-tab", idx),
                i18n("sidebar-tab-new"),
                HoverActionLayout::Bare,
                HoverActionVisibility::OnGroupHover("ws-item".into()),
                Button::new(("workspace-new-tab-button", idx))
                    // A pixel size leaves the box to the styles below:
                    // Button only derives its padding and glyph size from
                    // it, while the named sizes would pin the height too.
                    .with_size(px(NEW_TAB_GLYPH))
                    .ghost()
                    .aria_label(i18n("sidebar-tab-new"))
                    .size(px(NEW_TAB_BUTTON))
                    .child("+")
                    .dropdown_menu(move |menu, window, cx| {
                        new_tab_menu(menu, &menu_shell, window, cx)
                    }),
            )
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                this.workspaces.activate(idx);
                this.focus_active(window, cx);
                this.sync_session_memory(cx);
                cx.notify();
            }))
            .into_any_element()
        } else if ws.pinned {
            let label = i18n("sidebar-workspace-menu-unpin");
            hover_action(
                ("workspace-pin", idx),
                label,
                HoverActionLayout::Inline,
                HoverActionVisibility::OnGroupHover("ws-item".into()),
                Icon::new(PinIcon).small(),
            )
            .into_any_element()
        } else if ws.closeable {
            // Hover-only `×` closes the workspace and drops all of its
            // tabs (panes/PTYs die with the dropped Workspace).
            hover_action(
                ("workspace-close", idx),
                i18n("sidebar-workspace-menu-close"),
                HoverActionLayout::Inline,
                HoverActionVisibility::OnGroupHover("ws-item".into()),
                "×",
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.request_close_workspace(ws_id, window, cx);
            }))
            .into_any_element()
        } else {
            div().px_1().child("").into_any_element()
        };

        let suffix = h_flex()
            .gap_1()
            .children((ws.unread_count > 0).then(|| {
                div()
                    .id(("workspace-unread", idx))
                    .aria_label(
                        i18n("sidebar-workspace-unread-label")
                            .replace("{count}", &ws.unread_count.to_string()),
                    )
                    .size_5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .child(ws.unread_count.to_string())
            }))
            .child(controls);

        let full_path = ws.cwd.clone();
        // The `+N` token holds a fixed lane beside the path, so the path's own
        // budget shrinks by its width instead of pushing it off the row.
        let additional_count = ws.additional_cwds.len();
        let additional_summary = (additional_count > 0).then(|| {
            i18n("sidebar-workspace-additional-count")
                .replace("{count}", &additional_count.to_string())
        });
        let path_budget = (self.width
            - 80.0
            - additional_summary
                .as_ref()
                .map_or(0.0, |token| 8.0 + 7.0 * token.chars().count() as f32))
            / 7.0;
        let display_path = tail_preserving_path(
            &full_path,
            (path_budget.floor().max(0.0) as usize).clamp(8, 64),
        );
        // Tooltip and assistive technology get every directory in order; the
        // row itself only has room for the primary path.
        let dirs_description = workspace_dirs_description(&ws.cwd, &ws.additional_cwds);
        // A temporary workspace wears the same `*` an unsaved document does,
        // so its absence from the next session is visible before the user
        // closes the window.
        let display_label = match ws.temporary {
            true => format!("* {}", workspace_display_label(&ws.name, &ws.cwd)),
            false => workspace_display_label(&ws.name, &ws.cwd),
        };
        let name = div()
            .id(("workspace-secondary", idx))
            .aria_label(display_label.clone())
            .w_full()
            .text_left()
            .text_sm()
            .truncate();

        let name: AnyElement = if let Some(input) = renaming {
            let rename_shell = cx.entity();
            InlineRename::new(
                ("workspace-secondary", idx),
                display_label.clone(),
                input,
                InlineRenameStyle::Workspace,
                move |window, cx| {
                    rename_shell.update(cx, |this, cx| {
                        this.finish_workspace_rename(false, window, cx)
                    });
                },
            )
            .into_any_element()
        } else {
            name.child(display_label.clone()).into_any_element()
        };

        let drag_name: SharedString = display_label.clone().into();
        let drag_cwd: SharedString = display_path.clone().into();
        let drag_agent_status = ws.agent_status;
        let drag_terminal_activity = ws.terminal_activity;

        // Replicate the item's rendered width: sidebar width minus the card
        // gutter/border and the card's inner paddings around the list.
        let drag_width = (self.width - 36.0).max(80.0);
        let item = Button::new(("workspace", idx))
            .ghost()
            .when(!settings_entry, |this| {
                this.tooltip(dirs_description.clone())
            })
            .aria_label(if settings_entry {
                display_label.clone()
            } else {
                i18n("sidebar-workspace-item-label")
                    .replace("{name}", &display_label)
                    .replace("{path}", &dirs_description)
                    .replace("{status}", &status_label)
            })
            // The active tab's own row is highlighted in the vertical tab-bar
            // style, and it sits under its workspace, so highlighting the
            // workspace too would fill two rows for one selection.
            .selected(highlight_active)
            // Button resolves selected colors after element styles, so the
            // sidebar-accent pair must be the selected custom variant itself.
            .when(highlight_active, |this| {
                this.custom(
                    ButtonCustomVariant::new(cx)
                        .foreground(selection.active_foreground)
                        .active(selection.active_background),
                )
            })
            .w_full()
            .h_auto()
            .px_2()
            .py_1()
            .group("ws-item")
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(indicator)
                    .child(
                        v_flex()
                            .flex_1()
                            .overflow_hidden()
                            .items_start()
                            .child(name)
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_1()
                                    .items_center()
                                    .child(
                                        div()
                                            .id(("workspace-path", idx))
                                            .flex_1()
                                            .text_left()
                                            .text_xs()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .when(!settings_entry, |this| {
                                                this.aria_label(dirs_description.clone())
                                            })
                                            .text_color(cx.theme().sidebar_foreground.opacity(0.6))
                                            // The settings entry has no working
                                            // directory. A blank run still forms a line
                                            // box, so its row stands as tall as the
                                            // workspaces around it.
                                            .child(if settings_entry {
                                                SharedString::new_static(" ")
                                            } else {
                                                display_path.into()
                                            }),
                                    )
                                    .children(additional_summary.map(|token| {
                                        div()
                                            .id(("workspace-additional-dirs", idx))
                                            .flex_none()
                                            .text_xs()
                                            .aria_label(
                                                i18n("sidebar-workspace-additional-label").replace(
                                                    "{count}",
                                                    &additional_count.to_string(),
                                                ),
                                            )
                                            .text_color(cx.theme().sidebar_foreground.opacity(0.6))
                                            .child(token)
                                    })),
                            ),
                    )
                    .child(suffix),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.workspaces.activate(idx);
                this.focus_active(window, cx);
                this.sync_session_memory(cx);
                cx.notify();
            }));

        // Right-click menu. Close reuses the same confirm-gated path as the
        // hover `×` (last workspace included: quit/replace/cancel dialog).
        let shell = cx.entity();
        let drag_shell = shell.clone();
        let pinned = ws.pinned;
        let closeable = ws.closeable;
        let pin_label = if pinned {
            i18n("sidebar-workspace-menu-unpin")
        } else {
            i18n("sidebar-workspace-menu-pin")
        };
        let cwd = ws.cwd.clone();
        let temporary = ws.temporary;

        let progress = (!vertical_tabs)
            .then(|| ws.progress.fraction())
            .flatten()
            .map(|fraction| workspace_progress_bar(fraction, cx));

        div()
            .id(("workspace-menu", idx))
            .w_full()
            .relative()
            .when(self.dragging == Some(idx), |this| this.opacity(0.0))
            // Make way for the dragged item: the hovered item slides down,
            // opening an insertion gap at the pointer.
            .when(self.drag_over == Some(idx), |this| {
                this.mt(px(WS_MAKE_WAY_PX))
            })
            .on_drag(WorkspaceDrag { from: idx }, move |_, _, _, cx| {
                drag_shell.update(cx, |this, cx| {
                    this.sidebar.dragging = Some(idx);
                    cx.notify();
                });
                cx.new(|_| WorkspaceDragPreview {
                    name: drag_name.clone(),
                    cwd: drag_cwd.clone(),
                    agent_status: drag_agent_status,
                    terminal_activity: drag_terminal_activity,
                    width: drag_width,
                })
            })
            .on_drag_move(
                cx.listener(move |this, e: &DragMoveEvent<WorkspaceDrag>, _, cx| {
                    if !e.bounds.contains(&e.event.position) {
                        return;
                    }
                    // No gap over the drag's own item: dropping there is a
                    // no-op.
                    let target = (e.drag(cx).from != idx).then_some(idx);
                    if this.sidebar.drag_over != target {
                        this.sidebar.drag_over = target;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &WorkspaceDrag, window, cx| {
                // The list-level fallback handler must not also reorder this
                // drop.
                cx.stop_propagation();

                this.sidebar.drag_over = None;
                this.sidebar.dragging = None;

                this.reorder_workspaces(drag.from, idx, window, cx);
            }))
            .modern_context_menu(move |menu, _, _| {
                let rename_shell = shell.clone();
                let dirs_shell = shell.clone();
                let close_shell = shell.clone();
                let pin_shell = shell.clone();
                let activate_shell = shell.clone();
                let cwd = cwd.clone();

                // Pinning and closing are the two a user reaches for without
                // reading, so they lead as a row of buttons rather than taking a
                // line each. The settings entry is dismissible and nothing else.
                menu.commands(|row| {
                    row.when(!settings_entry, |row| {
                        row.item(pin_label, move |_, cx| {
                            pin_shell.update(cx, |this, cx| {
                                this.set_workspace_pinned(ws_id, !pinned, cx)
                            });
                        })
                        .icon(PinIcon)
                    })
                    .item_disabled(
                        i18n("sidebar-workspace-menu-close"),
                        !closeable,
                        move |window, cx| {
                            close_shell.update(cx, |this, cx| {
                                this.request_close_workspace(ws_id, window, cx)
                            });
                        },
                    )
                    .icon(IconName::Close)
                })
                // Renaming and copying a path both describe a workspace the user
                // owns, which the settings entry is not.
                .when(!settings_entry, |menu| {
                    menu.item(i18n("sidebar-workspace-menu-rename"), move |window, cx| {
                        rename_shell.update(cx, |this, cx| {
                            this.start_workspace_rename(ws_id, window, cx)
                        });
                    })
                    .icon(IconName::PenLine)
                    .item(
                        i18n("sidebar-workspace-menu-edit-dirs"),
                        move |window, cx| {
                            dirs_shell
                                .update(cx, |this, cx| this.edit_workspace_dirs(ws_id, window, cx));
                        },
                    )
                    .icon(IconName::Folder)
                    .item(i18n("sidebar-workspace-menu-copy-path"), move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(cwd.clone()));
                    })
                    .icon(IconName::Copy)
                    // Only a temporary workspace has anything to adopt.
                    .when(temporary, |menu| {
                        menu.item(i18n("sidebar-workspace-menu-activate"), move |_, cx| {
                            activate_shell
                                .update(cx, |this, cx| this.activate_as_workspace(ws_id, cx));
                        })
                        .icon(IconName::CircleCheck)
                    })
                })
            })
            .child(item)
            // After the row itself, because the row's selected fill would
            // otherwise paint over the bar's lane.
            .children(highlight_active.then(|| selection_bar(cx)))
            .children(progress)
            .into_any_element()
    }

    /// One tab of a workspace, rendered as a child row under it. Clicking it
    /// switches to that workspace *and* that tab, so a row under an inactive
    /// workspace is a single-click jump rather than a two-step one.
    pub(super) fn render_tab_row(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        tab: &SidebarTab,
        closeable: bool,
        rename: Option<&(TabId, Entity<InputState>)>,
        cx: &mut Context<Shell>,
    ) -> AnyElement {
        let tab_id = tab.id;
        let key = tab_id.0 as usize;
        let active = tab.active;
        let selection = sidebar_selection(cx);

        let close = hover_action(
            ("sidebar-tab-close", key),
            i18n("tabbar-menu-close"),
            HoverActionLayout::Inline,
            HoverActionVisibility::OnGroupHover("sidebar-tab".into()),
            "\u{00d7}",
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.request_close_tab(tab_id, window, cx);
        }));

        // One mark per row, because the lane it sits in is one glyph wide.
        // Ordered by what is worth acting on first: an agent mid-turn, then
        // what the shell is doing, then output nobody has read. A tab's own
        // kind of status therefore outranks the generic unread dot, the way
        // the horizontal strip orders them too.
        let status_mark: Option<AnyElement> = match (tab.agent_kind.is_some(), tab.busy) {
            (true, true) => Some(StatusMark::busy(("sidebar-tab-busy", key)).into_any_element()),
            _ => terminal_presentation(tab.terminal)
                .map(|(visual, aria)| {
                    div()
                        .id(("sidebar-tab-terminal", key))
                        .aria_label(aria)
                        .flex()
                        .child(terminal_dot(visual, TAB_ROW_DOT, cx))
                        .into_any_element()
                })
                .or_else(|| {
                    tab.unread.then(|| {
                        StatusMark::new(
                            ("sidebar-tab-unread", key),
                            StatusMarkTone::Primary,
                            px(TAB_ROW_DOT),
                        )
                        .label(i18n("sidebar-workspace-unread-label").replace("{count}", "1"))
                        .into_any_element()
                    })
                }),
        };

        let renaming = rename
            .filter(|(id, _)| *id == tab_id)
            .map(|(_, input)| input.clone());

        let label: AnyElement = match renaming {
            Some(input) => {
                let rename_shell = cx.entity();
                InlineRename::new(
                    ("sidebar-tab-rename", key),
                    tab.label.clone(),
                    input,
                    InlineRenameStyle::SidebarTab,
                    move |window, cx| {
                        rename_shell
                            .update(cx, |this, cx| this.finish_tab_rename(false, window, cx));
                    },
                )
                .into_any_element()
            }
            None => div()
                .flex_1()
                .overflow_hidden()
                .truncate()
                .when(tab.exited, |this| this.text_color(cx.theme().danger))
                .child(tab.label.clone())
                .into_any_element(),
        };

        let menu_shell = cx.entity();
        let drag_shell = cx.entity();
        let drag_label = tab.label.clone();
        // The row spans the list column: sidebar width minus the card gutter,
        // the card's inner padding, and the scrollbar lane.
        let drag_width = (self.width - 36.0).max(80.0);

        let row = h_flex()
            .id(("sidebar-tab", key))
            // The row is the selectable thing in this style: its fill is the
            // only cue that a tab is the one on screen, so it carries the
            // selected state assistive technology reads. The label is stated
            // rather than derived, because the row also holds status marks and
            // swaps its text for an input while the tab is being renamed.
            .role(Role::Tab)
            .aria_label(tab.label.clone())
            .aria_selected(active)
            .group("sidebar-tab")
            .relative()
            .w_full()
            .h(px(TAB_ROW_HEIGHT))
            // Indent past the workspace item's status column so the rows read
            // as belonging to the workspace above them.
            .pl_6()
            .pr_1()
            .gap_1()
            .items_center()
            .rounded(UI_RADIUS)
            .text_xs()
            // A restored-but-not-yet-spawned tab renders faded, the same
            // "sleeping tab" cue the horizontal strip uses.
            .when(tab.pending, |this| this.opacity(0.6))
            .children(active.then(|| selection_bar(cx)))
            .when(active, |this| {
                this.bg(selection.active_background)
                    .text_color(selection.active_foreground)
            })
            .when(!active, |this| {
                this.text_color(selection.idle_foreground)
                    .hover(|this| this.bg(selection.hover_background))
            })
            .child(div().flex_none().flex().child(match tab.pending {
                true => pending_tab_icon(("sidebar-tab-pending", key)).into_any_element(),
                false => tab_icon(tab.agent_kind, tab.settings).into_any_element(),
            }))
            .child(label)
            // The status mark sits in the lane the workspace item's status
            // column occupies — between that item's horizontal padding and
            // this row's indent — so every tab's mark lines up under it,
            // whatever kind of tab it is. Out of the flow, because that lane
            // is this row's left padding. The offset is 2px short of that
            // item's own padding because the item is a button, and its chrome
            // shifts the column it draws right of where the padding alone puts
            // it; matching the drawn column is what the eye reads as one line.
            .children(status_mark.map(|mark| {
                div()
                    .absolute()
                    .left(px(6.0))
                    .w_4()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(mark)
            }))
            // Bell dot, in the warning color so it reads apart from the unread
            // dot when a tab carries both.
            .children(tab.bell.then(|| {
                StatusMark::new(
                    ("sidebar-tab-bell", key),
                    StatusMarkTone::Warning,
                    px(TAB_ROW_DOT),
                )
            }))
            .when(closeable, |this| this.child(close))
            .children(tab.progress.map(|report| {
                let (color, fraction) = progress_visual(report, cx);
                div()
                    .absolute()
                    .bottom_0()
                    .left(UI_RADIUS)
                    .right(UI_RADIUS)
                    .h(px(2.0))
                    .child(
                        div()
                            .h_full()
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(color),
                    )
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.workspaces.activate(ws_idx);
                this.workspaces.active_tabs_mut().activate(tab_idx);
                this.focus_active(window, cx);
                this.sync_session_memory(cx);
                cx.notify();
            }));

        div()
            .id(("sidebar-tab-menu", key))
            .w_full()
            .when(self.tab_dragging == Some((ws_idx, tab_idx)), |this| {
                this.opacity(0.0)
            })
            // Make way for the dragged row: the hovered row slides down,
            // opening an insertion gap at the pointer.
            .when(self.tab_drag_over == Some((ws_idx, tab_idx)), |this| {
                this.mt(px(TAB_ROW_HEIGHT))
            })
            .on_drag(
                SidebarTabDrag {
                    workspace: ws_idx,
                    from: tab_idx,
                    tab: tab_id,
                },
                move |_, _, _, cx| {
                    drag_shell.update(cx, |this, cx| {
                        this.sidebar.tab_dragging = Some((ws_idx, tab_idx));
                        cx.notify();
                    });
                    cx.new(|_| SidebarTabDragPreview {
                        label: drag_label.clone(),
                        width: drag_width,
                    })
                },
            )
            .on_drag_move(
                cx.listener(move |this, e: &DragMoveEvent<SidebarTabDrag>, _, cx| {
                    if !e.bounds.contains(&e.event.position) {
                        return;
                    }
                    let drag = e.drag(cx);
                    // No gap over the drag's own row, and none over another
                    // workspace's rows, where the drop would be refused.
                    let target = (drag.workspace == ws_idx && drag.from != tab_idx)
                        .then_some((ws_idx, tab_idx));

                    if this.sidebar.tab_drag_over != target {
                        this.sidebar.tab_drag_over = target;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(move |this, drag: &SidebarTabDrag, window, cx| {
                // The list-level fallback handler must not also reorder this
                // drop.
                cx.stop_propagation();

                this.sidebar.tab_drag_over = None;
                this.sidebar.tab_dragging = None;

                if drag.workspace == ws_idx {
                    this.reorder_tab(drag.tab, drag.from, tab_idx, window, cx);
                }

                cx.notify();
            }))
            .modern_context_menu(move |menu, _, _| {
                let rename_shell = menu_shell.clone();
                let close_shell = menu_shell.clone();

                menu.item(i18n("tabbar-menu-rename"), move |window, cx| {
                    rename_shell.update(cx, |this, cx| this.start_tab_rename(tab_id, window, cx));
                })
                .icon(IconName::PenLine)
                .item_disabled(i18n("tabbar-menu-close"), !closeable, move |window, cx| {
                    close_shell.update(cx, |this, cx| this.request_close_tab(tab_id, window, cx));
                })
                .icon(IconName::Close)
            })
            .child(row)
            .into_any_element()
    }
}
