use std::rc::Rc;

use gpui::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, ListAlignment, ListState,
    ParentElement as _, SharedString, StyleRefinement, Styled, Window, div, list,
    prelude::FluentBuilder as _, px,
};
use rust_i18n::t;

use crate::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    h_flex,
    label::Label,
    scroll::ScrollableElement,
    setting::{RenderOptions, SettingGroup, settings::SettingsState},
    v_flex,
};

/// A setting page that can contain multiple setting groups.
#[derive(Clone)]
pub struct SettingPage {
    pub(super) icon: Option<Icon>,
    resettable: bool,
    pub(super) default_open: bool,
    pub(super) title: SharedString,
    pub(super) title_suffix: Option<Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>>,
    pub(super) description: Option<SharedString>,
    pub(super) groups: Vec<SettingGroup>,
    pub(super) header_style: StyleRefinement,
}

impl SettingPage {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            icon: None,
            resettable: true,
            default_open: false,
            title: title.into(),
            title_suffix: None,
            description: None,
            groups: Vec::new(),
            header_style: StyleRefinement::default(),
        }
    }

    /// Set the title of the setting page.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// Set a custom element to render after the title in the page header.
    ///
    /// For example, an info icon button that opens the help documentation.
    pub fn title_suffix<F, E>(mut self, suffix: F) -> Self
    where
        E: IntoElement,
        F: Fn(&mut Window, &mut App) -> E + 'static,
    {
        self.title_suffix = Some(Rc::new(move |window, cx| {
            suffix(window, cx).into_any_element()
        }));
        self
    }

    /// Set the icon of the setting page.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Set the description of the setting page, default is None.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the default open state of the setting page, default is false.
    pub fn default_open(mut self, default_open: bool) -> Self {
        self.default_open = default_open;
        self
    }

    /// Set whether the setting page is resettable, default is true.
    ///
    /// If true and the items in this page has changed, the reset button will appear.
    pub fn resettable(mut self, resettable: bool) -> Self {
        self.resettable = resettable;
        self
    }

    /// Add a setting group to the page.
    pub fn group(mut self, group: SettingGroup) -> Self {
        self.groups.push(group);
        self
    }

    /// Add multiple setting groups to the page.
    pub fn groups(mut self, groups: impl IntoIterator<Item = SettingGroup>) -> Self {
        self.groups.extend(groups);
        self
    }

    /// Set the style refinement for the header of the setting page.
    pub fn header_style(mut self, style: &StyleRefinement) -> Self {
        self.header_style = style.clone();
        self
    }

    pub(super) fn render(
        &self,
        ix: usize,
        single_group: bool,
        state: &Entity<SettingsState>,
        options: &RenderOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let search_input = state.read(cx).search_input.clone();
        let query = search_input.read(cx).value();

        // Single-group layout, outside search: the page shows exactly ONE
        // group — each subcategory acts as its own page, picked by the
        // sidebar selection (first group by default) — instead of one long
        // scroll through every group. A search query (and the classic
        // layout) lists every matching group, so hits are never hidden
        // behind the selection.
        let groups: Vec<(usize, SettingGroup)> = if single_group && query.is_empty() {
            let selected = state.read(cx).selected_index;
            let group_ix = (selected.page_ix == ix)
                .then_some(selected.group_ix)
                .flatten()
                .unwrap_or(0)
                .min(self.groups.len().saturating_sub(1));

            self.groups
                .get(group_ix)
                .map(|group| (group_ix, group.clone()))
                .into_iter()
                .collect()
        } else {
            self.groups
                .iter()
                .enumerate()
                .filter(|(_, group)| group.is_match(&query, cx))
                .map(|(group_ix, group)| (group_ix, group.clone()))
                .collect()
        };
        let groups_count = groups.len();

        // Header and reset operate on what is on screen — in single-group
        // display, resetting groups the user cannot see would be surprising.
        // In the classic layout every group is on screen, so this matches
        // the old reset-all behavior.
        let displayed: Vec<SettingGroup> = groups.iter().map(|(_, group)| group.clone()).collect();
        let title: SharedString = match displayed
            .first()
            .filter(|_| single_group && query.is_empty())
            .and_then(|group| group.title.clone())
        {
            Some(group_title) if group_title != self.title => {
                format!("{} › {}", self.title, group_title).into()
            }
            _ => self.title.clone(),
        };
        let resettable = self.resettable && displayed.iter().any(|group| group.is_resettable(cx));

        // In single-group layout the list is keyed per displayed group, so
        // switching subcategories starts at the top instead of inheriting
        // the previous group's scroll offset.
        let list_key = if single_group && query.is_empty() {
            format!(
                "list-state:{}:{}",
                ix,
                groups.first().map(|(group_ix, _)| *group_ix).unwrap_or(0)
            )
        } else {
            format!("list-state:{}", ix)
        };
        let list_state = window
            .use_keyed_state(SharedString::from(list_key), cx, |_, _| {
                ListState::new(groups_count, ListAlignment::Top, px(100.))
            })
            .read(cx)
            .clone();

        if list_state.item_count() != groups_count {
            list_state.reset(groups_count);
        }

        // Classic layout: a sidebar group click scrolls the full page to
        // that group.
        let deferred_scroll_group_ix = state.read(cx).deferred_scroll_group_ix;
        if let Some(scroll_ix) = deferred_scroll_group_ix {
            state.update(cx, |state, _| {
                state.deferred_scroll_group_ix = None;
            });
            if !single_group {
                list_state.scroll_to_reveal_item(scroll_ix);
            }
        }

        v_flex()
            .id(ix)
            .size_full()
            .child(
                v_flex()
                    .p_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .refine_style(&self.header_style)
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(title)
                                    .when_some(self.title_suffix.clone(), |this, suffix| {
                                        this.child(suffix(window, cx))
                                    }),
                            )
                            .when(resettable, |this| {
                                this.child(
                                    Button::new("reset")
                                        .icon(IconName::Undo2)
                                        .ghost()
                                        .small()
                                        .tooltip(t!("Settings.Reset All"))
                                        .on_click({
                                            let displayed = displayed.clone();
                                            move |_, window, cx| {
                                                for group in &displayed {
                                                    group.reset(window, cx);
                                                }
                                            }
                                        }),
                                )
                            }),
                    )
                    .when_some(self.description.clone(), |this, description| {
                        this.child(
                            Label::new(description)
                                .text_sm()
                                .text_color(cx.theme().muted_foreground),
                        )
                    }),
            )
            .child(
                div()
                    .px_4()
                    .relative()
                    .flex_1()
                    .w_full()
                    .child(
                        list(list_state.clone(), {
                            let query = query.clone();
                            let options = *options;
                            move |list_ix, window, cx| {
                                let (group_ix, group) = groups[list_ix].clone();
                                group
                                    .py_4()
                                    .render(
                                        &query,
                                        &RenderOptions {
                                            page_ix: ix,
                                            group_ix,
                                            ..options
                                        },
                                        window,
                                        cx,
                                    )
                                    .into_any_element()
                            }
                        })
                        .size_full(),
                    )
                    .vertical_scrollbar(&list_state),
            )
    }
}
