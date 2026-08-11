use std::ops::Range;

use crate::{
    IconName, Sizable, Size, StyledExt,
    group_box::GroupBoxVariant,
    input::{Input, InputState},
    resizable::{h_resizable, resizable_panel},
    setting::{SettingGroup, SettingPage},
    sidebar::{Sidebar, SidebarMenu, SidebarMenuItem},
};
use gpui::{
    App, AppContext as _, Axis, ElementId, Entity, IntoElement, ParentElement as _, Pixels,
    RenderOnce, StyleRefinement, Styled, Window, canvas, div, prelude::FluentBuilder as _, px,
    relative,
};
use rust_i18n::t;

const STACKED_LAYOUT_MAX_WIDTH: Pixels = px(480.);

/// The settings structure containing multiple pages for app settings.
///
/// The hierarchy of settings is as follows:
///
/// ```ignore
/// Settings
///   SettingPage     <- The single active page displayed
///     SettingGroup
///       SettingItem
///         Label
///         SettingField (e.g., Switch, Dropdown, Input)
/// ```
#[derive(IntoElement)]
pub struct Settings {
    id: ElementId,
    pages: Vec<SettingPage>,
    group_variant: GroupBoxVariant,
    size: Size,
    sidebar_width: Pixels,
    sidebar_size_range: Range<Pixels>,
    sidebar_style: StyleRefinement,
    default_selected_index: SelectIndex,
    header_style: StyleRefinement,
    single_group_pages: bool,
    state: Option<Entity<SettingsState>>,
}

impl Settings {
    /// Create a new settings with the given ID.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            pages: vec![],
            group_variant: GroupBoxVariant::default(),
            size: Size::default(),
            sidebar_width: px(250.0),
            sidebar_size_range: px(160.0)..px(360.0),
            sidebar_style: StyleRefinement::default(),
            default_selected_index: SelectIndex::default(),
            header_style: StyleRefinement::default(),
            single_group_pages: false,
            state: None,
        }
    }

    /// Render against a state the caller owns instead of the element state
    /// keyed by this view's id. Element state lives only while the view is
    /// rendered on consecutive frames, so a view that unmounts loses its
    /// selected page and search query; an owned state survives that. The
    /// caller is responsible for observing the state so selection changes
    /// repaint, and [`Settings::select`] addresses element state rather than
    /// this one.
    pub fn state(mut self, state: Entity<SettingsState>) -> Self {
        self.state = Some(state);
        self
    }

    /// Display one group at a time per page instead of scrolling through all
    /// groups. Selecting a group in the sidebar swaps the page content to
    /// that group alone (entering a multi-group page lands on its first
    /// group); a search query still lists every matching group. Default is
    /// false: the classic layout scrolls the whole page and sidebar group
    /// clicks jump to the group's position.
    pub fn single_group_pages(mut self, single: bool) -> Self {
        self.single_group_pages = single;
        self
    }

    /// Set the width of the sidebar, default is `250px`.
    pub fn sidebar_width(mut self, width: impl Into<Pixels>) -> Self {
        self.sidebar_width = width.into();
        self
    }

    /// Set the resize range of the sidebar, default is `160px..360px`.
    pub fn sidebar_size_range(mut self, range: impl Into<Range<Pixels>>) -> Self {
        self.sidebar_size_range = range.into();
        self
    }

    /// Add a page to the settings.
    pub fn page(mut self, page: SettingPage) -> Self {
        self.pages.push(page);
        self
    }

    /// Add pages to the settings.
    pub fn pages(mut self, pages: impl IntoIterator<Item = SettingPage>) -> Self {
        self.pages.extend(pages);
        self
    }

    /// Set the default variant for all setting groups.
    ///
    /// All setting groups will use this variant unless overridden individually.
    pub fn with_group_variant(mut self, variant: GroupBoxVariant) -> Self {
        self.group_variant = variant;
        self
    }

    /// Set the style refinement for the sidebar.
    pub fn sidebar_style(mut self, style: &StyleRefinement) -> Self {
        self.sidebar_style = style.clone();
        self
    }

    /// Set the default index of the page to be selected.
    pub fn default_selected_index(mut self, index: SelectIndex) -> Self {
        self.default_selected_index = index;
        self
    }

    /// Set the style refinement for the header.
    pub fn header_style(mut self, style: &StyleRefinement) -> Self {
        self.header_style = style.clone();
        self
    }

    /// Programmatically select a page (and optionally one of its groups) in
    /// the settings view addressed by the element id given to
    /// [`Settings::new`], as if its sidebar entry was clicked — for flows
    /// like "Add profile" that create a group and want the view to jump to
    /// it. Called before the first render, the selection simply becomes the
    /// initial one.
    pub fn select(id: impl Into<ElementId>, index: SelectIndex, window: &mut Window, cx: &mut App) {
        let state = window
            .use_keyed_state(id.into(), cx, |window, cx| {
                SettingsState::new(index, window, cx)
            })
            .clone();

        state.update(cx, |state, cx| {
            state.selected_index = index;
            // Classic scroll-through layout jumps to the group; the
            // single-group layout ignores this and shows the group alone.
            state.deferred_scroll_group_ix = index.group_ix;
            cx.notify();
        });
    }

    fn filtered_pages(&self, query: &str, cx: &App) -> Vec<SettingPage> {
        self.pages
            .iter()
            .filter_map(|page| {
                let filtered_groups: Vec<SettingGroup> = page
                    .groups
                    .iter()
                    .filter_map(|group| {
                        let mut group = group.clone();
                        group.items = group
                            .items
                            .iter()
                            .filter(|item| item.is_match(&query, cx))
                            .cloned()
                            .collect();
                        if group.items.is_empty() {
                            None
                        } else {
                            Some(group)
                        }
                    })
                    .collect();
                let mut page = page.clone();
                page.groups = filtered_groups;
                if page.groups.is_empty() {
                    None
                } else {
                    Some(page)
                }
            })
            .collect()
    }

    fn render_active_page(
        &self,
        state: &Entity<SettingsState>,
        pages: &Vec<SettingPage>,
        options: &RenderOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let selected_index = state.read(cx).selected_index;

        for (ix, page) in pages.into_iter().enumerate() {
            if selected_index.page_ix == ix {
                return page
                    .render(ix, self.single_group_pages, state, &options, window, cx)
                    .into_any_element();
            }
        }

        return div().into_any_element();
    }

    fn render_sidebar(
        &self,
        state: &Entity<SettingsState>,
        pages: &Vec<SettingPage>,
        _: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let selected_index = state.read(cx).selected_index;
        let search_input = state.read(cx).search_input.clone();

        Sidebar::new("settings-sidebar")
            .w(relative(1.))
            .border_0()
            .refine_style(&self.sidebar_style)
            .collapsible(false)
            .collapsed(false)
            .header(
                div()
                    .w_full()
                    .refine_style(&self.header_style)
                    .child(Input::new(&search_input).prefix(IconName::Search)),
            )
            .child(
                SidebarMenu::new().children(pages.iter().enumerate().map(|(page_ix, page)| {
                    let is_page_active =
                        selected_index.page_ix == page_ix && selected_index.group_ix.is_none();
                    // In single-group layout, pages render one group at a
                    // time, so entering a multi-group page selects its first
                    // group explicitly — the sidebar highlight then matches
                    // what is displayed.
                    let entry_group_ix =
                        (self.single_group_pages && page.groups.len() > 1).then_some(0);
                    let single_group_pages = self.single_group_pages;

                    SidebarMenuItem::new(page.title.clone())
                        .click_to_open(true)
                        .when_some(page.icon.clone(), |this, icon| this.icon(icon))
                        .default_open(page.default_open)
                        .active(is_page_active)
                        .on_click({
                            let state = state.clone();
                            move |_, _, cx| {
                                state.update(cx, |state, cx| {
                                    state.selected_index = SelectIndex {
                                        page_ix,
                                        group_ix: entry_group_ix,
                                    };
                                    cx.notify();
                                })
                            }
                        })
                        .when(page.groups.len() > 1, |this| {
                            this.children(
                                // Enumerate BEFORE filtering untitled groups
                                // so the stored index addresses `page.groups`
                                // (the page renders by that index).
                                page.groups
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, group)| group.title.is_some())
                                    .map(|(group_ix, group)| {
                                        let is_active = selected_index.page_ix == page_ix
                                            && selected_index.group_ix == Some(group_ix);
                                        let title = group.title.clone().unwrap_or_default();

                                        SidebarMenuItem::new(title).active(is_active).on_click({
                                            let state = state.clone();
                                            move |_, _, cx| {
                                                state.update(cx, |state, cx| {
                                                    state.selected_index = SelectIndex {
                                                        page_ix,
                                                        group_ix: Some(group_ix),
                                                    };
                                                    // Classic layout keeps
                                                    // every group on the page
                                                    // and jumps to the picked
                                                    // one.
                                                    if !single_group_pages {
                                                        state.deferred_scroll_group_ix =
                                                            Some(group_ix);
                                                    }
                                                    cx.notify();
                                                })
                                            }
                                        })
                                    }),
                            )
                        })
                })),
            )
    }
}

impl Sizable for Settings {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

/// Selected page, deferred scroll target, and search query of one settings
/// view. [`Settings`] keeps this in element state by default, which lasts only
/// as long as the view is rendered on consecutive frames; [`SettingsState::owned`]
/// hands the caller an entity that outlives unmounting.
pub struct SettingsState {
    pub(super) selected_index: SelectIndex,
    /// If set, defer scrolling to this group index after rendering (classic
    /// scroll-through layout only).
    pub(super) deferred_scroll_group_ix: Option<usize>,
    pub(super) search_input: Entity<InputState>,
}

impl SettingsState {
    /// Build a state the caller owns, for a settings view that unmounts and
    /// remounts (a view behind a tab, or one switched away from) and should
    /// come back showing the page and query the user left it on. Pass it to
    /// [`Settings::state`], and observe it to repaint on selection changes.
    pub fn owned(default_selected: SelectIndex, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(default_selected, window, cx))
    }

    fn new(default_selected: SelectIndex, window: &mut Window, cx: &mut App) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("Settings.search_placeholder"))
                .default_value("")
        });

        SettingsState {
            search_input,
            selected_index: default_selected,
            deferred_scroll_group_ix: None,
        }
    }
}

/// Options for rendering setting item.
#[derive(Clone, Copy)]
pub struct RenderOptions {
    pub page_ix: usize,
    pub group_ix: usize,
    pub item_ix: usize,
    pub size: Size,
    pub group_variant: GroupBoxVariant,
    pub layout: Axis,
    pub disabled: bool,
}

#[derive(Clone, Copy, Default)]
pub struct SelectIndex {
    pub page_ix: usize,
    pub group_ix: Option<usize>,
}

impl RenderOnce for Settings {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state.clone().unwrap_or_else(|| {
            window.use_keyed_state(self.id.clone(), cx, |window, cx| {
                SettingsState::new(self.default_selected_index, window, cx)
            })
        });

        let query = state.read(cx).search_input.read(cx).value();
        let filtered_pages = self.filtered_pages(&query, cx);
        let options = RenderOptions {
            page_ix: 0,
            group_ix: 0,
            item_ix: 0,
            size: self.size,
            group_variant: self.group_variant,
            layout: Axis::Horizontal,
            disabled: false,
        };
        let sidebar_size_range = self.sidebar_size_range.clone();
        let sidebar = self
            .render_sidebar(&state, &filtered_pages, window, cx)
            .into_any_element();

        h_resizable(self.id.clone())
            .child(
                resizable_panel()
                    .size(self.sidebar_width)
                    .size_range(sidebar_size_range)
                    .child(sidebar),
            )
            .child(
                resizable_panel().divider_visible(false).child(
                    canvas(
                        move |bounds, window, cx| {
                            let options = RenderOptions {
                                layout: if bounds.size.width <= STACKED_LAYOUT_MAX_WIDTH {
                                    Axis::Vertical
                                } else {
                                    Axis::Horizontal
                                },
                                ..options
                            };
                            let mut page = self.render_active_page(
                                &state,
                                &filtered_pages,
                                &options,
                                window,
                                cx,
                            );
                            page.prepaint_as_root(bounds.origin, bounds.size.into(), window, cx);
                            page
                        },
                        |_, mut page, window, cx| page.paint(window, cx),
                    )
                    .size_full(),
                ),
            )
    }
}
