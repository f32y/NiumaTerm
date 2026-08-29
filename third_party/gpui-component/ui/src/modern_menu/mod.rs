//! A context menu drawn in a window of its own.
//!
//! [`crate::menu::PopupMenu`] is drawn inside the window that opens it, so it is
//! clipped to that window and can only blur what the application has already
//! painted. This one gets a window, which lets the platform give it the material
//! and the shape it gives its own menus, and lets it extend past the owner's
//! edges.
//!
//! The popup window never takes activation. Its logical owner goes on rendering
//! as the focused window — and on Windows, so its own backdrop material goes on
//! rendering at all, since DWM drops that for a window that is not active. The
//! cost is that the menu never receives a key press or a press outside itself:
//! the owner does, and is responsible for calling [`dismiss_modern_menu`] when it
//! sees one.
//!
//! ```ignore
//! ModernMenu::new()
//!     .item("Rename", move |window, cx| { /* runs against the owner window */ })
//!     .item_disabled("Close", !closeable, move |window, cx| { … })
//!     .show_at(event.position, window, cx);
//! ```

use std::rc::Rc;

use gpui::{
    Action, AnyWindowHandle, App, AppContext as _, Bounds, Context, Font, Global, KeyDownEvent,
    Pixels, Point, SharedString, TextRun, Window, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, font, point, px, size,
};

use crate::{ActiveTheme as _, Icon};

mod ext;
mod metrics;
#[cfg(test)]
mod tests;
mod view;

pub use ext::ModernMenuExt;

/// Runs against the window the menu was opened from, never the menu's own window.
type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The input that requested a menu, which selects the matching WinUI spacing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModernMenuInput {
    /// Mouse and pen menus use the compact row layout.
    #[default]
    Mouse,
    /// Keyboard menus use the same compact row layout as mouse menus.
    Keyboard,
    /// Touch menus use larger vertical targets and a wider minimum surface.
    Touch,
}

/// What choosing an item does.
///
/// Both are carried out against the owner window: a handler was written for the
/// window the menu was opened from, and an action has to reach the handlers
/// registered on that window's element tree, of which the menu's own window has
/// none.
#[derive(Clone)]
enum Activation {
    Handler(Handler),
    Action(Rc<dyn Action>),
}

#[derive(Clone)]
struct Item {
    label: SharedString,
    disabled: bool,
    activation: Activation,
    icon: Option<Icon>,
}

/// A row that opens a menu of its own beside it.
#[derive(Clone)]
struct Submenu {
    label: SharedString,
    icon: Option<Icon>,
    entries: Vec<Entry>,
}

#[derive(Clone)]
enum Entry {
    Separator,
    Item(Item),
    /// Items drawn side by side as icon buttons rather than stacked as rows.
    Commands(Vec<Item>),
    Submenu(Submenu),
}

impl Entry {
    /// The height this entry takes in a menu laid out for `input`.
    fn height(&self, input: ModernMenuInput) -> Pixels {
        match self {
            Entry::Separator => metrics::SEPARATOR_HEIGHT,
            Entry::Item(_) | Entry::Submenu(_) => metrics::item_height(input),
            Entry::Commands(_) => metrics::COMMAND_ROW_HEIGHT,
        }
    }

    /// Whether this entry is something the user can land on.
    fn selectable(&self) -> bool {
        match self {
            Entry::Item(item) => !item.disabled,
            Entry::Submenu(_) => true,
            Entry::Separator | Entry::Commands(_) => false,
        }
    }
}

/// The one menu window, built on first use and reused from then on.
///
/// Building it costs a graphics device, a swap chain and a composition tree,
/// which measured around 400ms — far too long to sit between a right click and a
/// menu. Showing the window that already exists costs a window move. Its HWND is
/// deliberately unowned so the same prewarmed surface can serve every application
/// window without transferring Win32 ownership or being destroyed with one of
/// those windows; `MenuView::owner` records only where commands are dispatched.
#[derive(Default)]
struct MenuWindow {
    /// One window per level of nesting, root first. A submenu is drawn beside
    /// its parent rather than over it, so both are on screen at once and each
    /// needs a surface of its own.
    windows: Vec<WindowHandle<MenuView>>,
    /// Whether the window has already been asked for ahead of time, so the
    /// request is only made once.
    prewarm_requested: bool,
}

impl Global for MenuWindow {}

/// The font every menu draws its labels in unless it names its own.
#[derive(Default)]
struct DefaultFont(Option<Font>);

impl Global for DefaultFont {}

/// Draw menu labels in `font` from now on.
///
/// A menu has a window of its own and so inherits no text style from the window
/// it opens over. An application whose chrome font carries fallbacks it depends
/// on — a preferred CJK face, say — names it once here rather than at every call
/// site. Re-applying the same font costs nothing, so this can be called from a
/// render that reads the font from settings.
pub fn set_default_font(cx: &mut App, font: Font) {
    let current = cx.default_global::<DefaultFont>();
    if current.0.as_ref() == Some(&font) {
        return;
    }

    current.0 = Some(font);
}

/// Lets callers append entries conditionally, matching [`crate::menu::PopupMenu`].
impl gpui::prelude::FluentBuilder for ModernMenu {}

/// Builder for a context menu. Items are appended in display order.
#[derive(Default)]
pub struct ModernMenu {
    entries: Vec<Entry>,
    font: Option<Font>,
    input: ModernMenuInput,
    side: metrics::Side,
}

impl ModernMenu {
    /// Create an empty menu.
    pub fn new() -> Self {
        Self::default()
    }

    /// Draw the labels in `font` instead of the theme's UI font.
    ///
    /// The menu has a window of its own and so inherits no text style from the
    /// window it opens over; an application whose own font carries fallbacks it
    /// depends on has to name it here.
    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    /// Append an item that runs `handler` against the owner window when chosen.
    pub fn item(
        self,
        label: impl Into<SharedString>,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.item_disabled(label, false, handler)
    }

    /// Append an item that is greyed out and unclickable while `disabled`.
    pub fn item_disabled(
        self,
        label: impl Into<SharedString>,
        disabled: bool,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.push(label, disabled, Activation::Handler(Rc::new(handler)))
    }

    /// Append an item that dispatches `action` to the owner window when chosen.
    pub fn action(self, label: impl Into<SharedString>, action: Box<dyn Action>) -> Self {
        self.action_disabled(label, false, action)
    }

    /// Append an action item that is greyed out and unclickable while `disabled`.
    pub fn action_disabled(
        self,
        label: impl Into<SharedString>,
        disabled: bool,
        action: Box<dyn Action>,
    ) -> Self {
        self.push(label, disabled, Activation::Action(Rc::from(action)))
    }

    /// Give the item just appended an icon.
    ///
    /// Applies to the most recent item rather than being taken by every
    /// constructor above, which would double each of them. A call with no item
    /// behind it is ignored.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        match self.entries.last_mut() {
            Some(Entry::Item(item)) => item.icon = Some(icon.into()),
            Some(Entry::Submenu(submenu)) => submenu.icon = Some(icon.into()),
            Some(Entry::Separator | Entry::Commands(_)) | None => {}
        }

        self
    }

    /// Append a row that opens `build`'s menu beside it.
    ///
    /// Nesting has no depth limit here; each level is drawn in a window of its
    /// own, and a level opens only while the row above it is the one being
    /// pointed at. A submenu that `build` leaves empty is dropped, so a caller
    /// can offer one without first checking whether it has anything to list.
    pub fn submenu(
        mut self,
        label: impl Into<SharedString>,
        build: impl FnOnce(ModernMenu) -> ModernMenu,
    ) -> Self {
        let entries = normalize_separators(build(ModernMenu::new()).entries);
        if entries.is_empty() {
            return self;
        }

        self.entries.push(Entry::Submenu(Submenu {
            label: label.into(),
            icon: None,
            entries,
        }));
        self
    }

    /// Append a row of icon buttons, laid out side by side.
    ///
    /// For the few actions worth reaching without reading, the way a system
    /// context menu opens with cut, copy and the like. `build` appends to a menu
    /// of its own using the same builders as anywhere else, and what it appends
    /// becomes one row; separators inside it are dropped.
    ///
    /// Command buttons are reachable by pointer only. Everything that belongs in
    /// one has a keyboard shortcut of its own, and giving arrow keys two axes to
    /// walk would cost more than it returns.
    pub fn commands(mut self, build: impl FnOnce(ModernMenu) -> ModernMenu) -> Self {
        let items: Vec<Item> = build(ModernMenu::new())
            .entries
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Item(item) => Some(item),
                Entry::Separator | Entry::Commands(_) | Entry::Submenu(_) => None,
            })
            .collect();

        if !items.is_empty() {
            self.entries.push(Entry::Commands(items));
        }

        self
    }

    /// Append a rule between the items around it.
    ///
    /// Leading, trailing and consecutive separators are dropped when the menu is
    /// shown, so a caller can append one after a group that turns out to be empty
    /// without having to track whether anything preceded it.
    pub fn separator(mut self) -> Self {
        self.entries.push(Entry::Separator);
        self
    }

    fn push(
        mut self,
        label: impl Into<SharedString>,
        disabled: bool,
        activation: Activation,
    ) -> Self {
        self.entries.push(Entry::Item(Item {
            label: label.into(),
            disabled,
            activation,
            icon: None,
        }));
        self
    }

    /// Whether the menu has nothing to show.
    pub fn is_empty(&self) -> bool {
        !self.entries.iter().any(|entry| {
            matches!(
                entry,
                Entry::Item(_) | Entry::Commands(_) | Entry::Submenu(_)
            )
        })
    }

    /// Open the menu at `position`, in the owner window's coordinates.
    pub fn show_at(self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        self.show_at_with_input(position, ModernMenuInput::Mouse, window, cx);
    }

    /// Open the menu with its bottom edge at `position` rather than its top.
    ///
    /// For a menu about a region of the window instead of the point that was
    /// pressed - a text selection, say, which the menu has to leave visible for
    /// the choice it offers to mean anything. It still opens below when there is
    /// no room above.
    pub fn show_above(mut self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        self.side = metrics::Side::Above;
        self.show_at_with_input(position, ModernMenuInput::Mouse, window, cx);
    }

    /// Open the menu using the spacing for the input that requested it.
    pub fn show_at_with_input(
        mut self,
        position: Point<Pixels>,
        input: ModernMenuInput,
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.is_empty() {
            return;
        }
        self.input = input;
        self.entries = normalize_separators(self.entries);

        #[cfg(target_os = "windows")]
        self.show_as_flyout(position, window, cx);
        #[cfg(not(target_os = "windows"))]
        self.show_as_native(position, window, cx);
    }

    /// Stand-in for platforms without a flyout window, which is what the drawn
    /// menu needs to reach beyond its owner and carry a backdrop material.
    ///
    /// [`NativeMenu`] dispatches actions and nothing else, so items carrying a
    /// closure cannot be expressed and are dropped. Every menu that has to work
    /// off Windows is built from actions for that reason.
    #[cfg(not(target_os = "windows"))]
    fn show_as_native(self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        native_menu(self.entries).show(position, window, cx);
    }
}

/// The same entries as a menu the platform draws itself.
#[cfg(not(target_os = "windows"))]
fn native_menu(entries: Vec<Entry>) -> crate::native_menu::NativeMenu {
    let mut native = crate::native_menu::NativeMenu::new();
    {
        for entry in entries {
            native = match entry {
                Entry::Separator => native.separator(),
                Entry::Item(item) => push_native(native, item),
                Entry::Commands(items) => items.into_iter().fold(native, push_native),
                Entry::Submenu(submenu) => {
                    native.submenu(submenu.label, native_menu(submenu.entries))
                }
            };
        }
    }

    native
}

impl ModernMenu {
    #[cfg(target_os = "windows")]
    fn show_as_flyout(self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let menu_font = self
            .font
            .clone()
            .or_else(|| cx.try_global::<DefaultFont>().and_then(|it| it.0.clone()))
            .unwrap_or_else(|| font(cx.theme().font_family.clone()));

        let work_area = window
            .display(cx)
            .map(|display| display.visible_bounds())
            .unwrap_or_else(|| Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(f32::MAX), px(f32::MAX)),
            });
        // `bounds` is the window's client rect in the global space the work area
        // is also expressed in, so this is the anchor as the screen sees it.
        let anchor = window.bounds().origin + position;

        present(
            Presentation {
                level: 0,
                entries: self.entries,
                placement: Placement::Anchored {
                    anchor,
                    side: self.side,
                },
                input: self.input,
                font: menu_font,
                work_area,
                owner: window.window_handle(),
                select_first: false,
            },
            cx,
        );
    }
}

/// Where a menu is put: against a point the caller named, or beside the row of
/// the menu that opened it.
enum Placement {
    Anchored {
        anchor: Point<Pixels>,
        side: metrics::Side,
    },
    Beside {
        parent: Bounds<Pixels>,
        row_offset: Pixels,
    },
}

/// One menu about to be drawn, at the level of nesting it belongs to.
struct Presentation {
    level: usize,
    entries: Vec<Entry>,
    placement: Placement,
    input: ModernMenuInput,
    font: Font,
    work_area: Bounds<Pixels>,
    owner: AnyWindowHandle,
    /// Start on the first item, for a menu opened from the keyboard. A menu the
    /// pointer opened starts with nothing selected, so the highlight follows the
    /// pointer rather than appearing under it.
    select_first: bool,
}

/// Draw `presentation` in the window that belongs to its level.
fn present(presentation: Presentation, cx: &mut App) {
    // Placed off screen: the real rect needs the labels shaped, and they are
    // shaped below in the window that will draw them.
    let seed = Bounds {
        origin: match &presentation.placement {
            Placement::Anchored { anchor, .. } => *anchor,
            Placement::Beside { parent, .. } => parent.origin,
        },
        size: size(px(1.0), px(1.0)),
    };
    let appearance = window_appearance(cx);
    let Some(menu) = menu_window(presentation.level, seed, appearance, cx) else {
        return;
    };

    let Presentation {
        level,
        entries,
        placement,
        input,
        font: menu_font,
        work_area,
        owner,
        select_first,
    } = presentation;
    let opens_submenu = entries
        .iter()
        .any(|entry| matches!(entry, Entry::Submenu(_)));

    let _ = menu.update(cx, |view, menu_window, cx| {
        // Shaped in the window that will render them rather than the one the
        // menu was opened from: the two have separate text systems, and a
        // label measured against the wrong one is given room that does not
        // match what it draws.
        let widest = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item(item) => Some(&item.label),
                Entry::Submenu(submenu) => Some(&submenu.label),
                // A command button's label is drawn small and centred under
                // its icon, inside a button of fixed width; it has no say in
                // how wide the menu is.
                Entry::Separator | Entry::Commands(_) => None,
            })
            .map(|label| {
                let run = TextRun {
                    len: label.len(),
                    font: menu_font.clone(),
                    color: gpui::black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                menu_window
                    .text_system()
                    .shape_line(label.clone(), metrics::FONT_SIZE, &[run], None)
                    .width
            })
            .fold(px(0.0), Pixels::max);

        let content = metrics::Content {
            items: entries
                .iter()
                .filter(|entry| matches!(entry, Entry::Item(_) | Entry::Submenu(_)))
                .count(),
            separators: entries
                .iter()
                .filter(|entry| matches!(entry, Entry::Separator))
                .count(),
            command_rows: entries
                .iter()
                .filter(|entry| matches!(entry, Entry::Commands(_)))
                .count(),
            chevrons: opens_submenu,
            widest_command_row: entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Commands(items) => Some(items.len()),
                    Entry::Item(_) | Entry::Separator | Entry::Submenu(_) => None,
                })
                .max()
                .unwrap_or_default(),
        };
        let menu_size = metrics::menu_size(widest, content, input);
        let bounds = Bounds {
            origin: match placement {
                Placement::Anchored { anchor, side } => {
                    metrics::place(anchor, menu_size, work_area, side)
                }
                Placement::Beside { parent, row_offset } => {
                    metrics::place_submenu(parent, row_offset, menu_size, work_area)
                }
            },
            size: menu_size,
        };

        view.selected = select_first.then(|| first_selectable(&entries)).flatten();
        view.entries = entries;
        view.owner = Some(owner);
        view.font = Some(menu_font);
        view.input = input;
        view.level = level;
        view.bounds = bounds;
        view.work_area = work_area;
        view.open_child = None;
        menu_window.show_flyout(bounds);
        cx.notify();
    });

    // Building a menu window costs the graphics setup described on
    // [`MenuWindow`], which under a hover would arrive long after the pointer
    // did. The level below is built while this one is on screen, so the first
    // submenu opens as fast as the ones after it.
    if opens_submenu {
        cx.defer(move |cx| {
            menu_window(level + 1, seed, appearance, cx);
        });
    }
}

fn first_selectable(entries: &[Entry]) -> Option<usize> {
    entries.iter().position(Entry::selectable)
}

/// Append `item` to a native menu, dropping it if it carries a closure rather
/// than an action.
#[cfg(not(target_os = "windows"))]
fn push_native(
    native: crate::native_menu::NativeMenu,
    item: Item,
) -> crate::native_menu::NativeMenu {
    let Activation::Action(action) = item.activation else {
        log::warn!(
            "modern menu item {:?} carries a closure, which a native menu cannot dispatch",
            item.label
        );
        return native;
    };

    match item.icon {
        Some(icon) => {
            native.menu_with_icon_disabled(item.label, icon, item.disabled, action.boxed_clone())
        }
        None => native.menu_with_disabled(item.label, item.disabled, action.boxed_clone()),
    }
}

/// Build the menu window before any menu needs it.
///
/// The first build costs hundreds of milliseconds of graphics setup. Paying that
/// while the user is still taking in a freshly opened window is better than
/// paying it under their first right click. Deferred, so it lands after the frame
/// that asked for it rather than delaying that frame.
pub fn prewarm_modern_menu(cx: &mut App) {
    let menu = cx.default_global::<MenuWindow>();
    if menu.prewarm_requested {
        return;
    }
    menu.prewarm_requested = true;

    let appearance = window_appearance(cx);
    // Never shown at these bounds; the flyout is placed when it is shown.
    let bounds = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(1.0), px(1.0)),
    };

    cx.defer(move |cx| {
        menu_window(0, bounds, appearance, cx);
    });
}

/// Offer a key press to the menu that is up, reporting whether it was used.
///
/// The menu never takes activation, so it never receives a key press of its own;
/// the window it was opened from does, and passes them here. A caller that gets
/// `true` back should stop the press going any further, since the menu has just
/// acted on it.
pub fn dispatch_modern_menu_key(event: &KeyDownEvent, cx: &mut App) -> bool {
    // Keys go to the innermost menu on screen: that is the one the user stepped
    // into, and the levels above it are showing the path taken to get there.
    let Some((level, window)) = deepest_menu(cx) else {
        return false;
    };

    let step = match event.keystroke.key.as_ref() {
        "escape" if level == 0 => {
            dismiss_modern_menu(cx);
            return true;
        }
        // Inside a submenu, Escape gives up that level rather than the whole
        // menu, so a wrong turn costs one press instead of reopening.
        "escape" | "left" => Step::Close,
        "up" => Step::Previous,
        "down" => Step::Next,
        "home" => Step::First,
        "end" => Step::Last,
        "enter" => Step::Confirm,
        "right" => Step::Open,
        _ => return false,
    };

    window
        .update(cx, |view, _, cx| view.walk(step, cx))
        .unwrap_or(false)
}

/// The innermost menu that is up, with the level it is drawn at.
///
/// A menu window outlives the menu it drew — they are built once and reused — so
/// the entries are what say whether anything is on screen. Without that check
/// every key handled here would be swallowed application-wide.
fn deepest_menu(cx: &mut App) -> Option<(usize, WindowHandle<MenuView>)> {
    let windows = cx.default_global::<MenuWindow>().windows.clone();

    let mut deepest = None;
    for (level, window) in windows.into_iter().enumerate() {
        let showing = window
            .update(cx, |view, _, _| !view.entries.is_empty())
            .unwrap_or(false);
        if !showing {
            break;
        }
        deepest = Some((level, window));
    }

    deepest
}

/// Take every menu from `level` inwards off the screen.
fn hide_menus_from(level: usize, cx: &mut App) {
    let windows = cx.default_global::<MenuWindow>().windows.clone();

    for window in windows.into_iter().skip(level).rev() {
        let _ = window.update(cx, |view, menu_window, _| {
            // The window outlives any one menu, so hiding it when nothing is up
            // would race a menu that is about to be shown.
            if view.entries.is_empty() {
                return;
            }

            // A handler can hold the entities the menu was built from alive, and
            // outliving the menu it belongs to would be surprising.
            view.entries.clear();
            view.selected = None;
            view.open_child = None;
            menu_window.hide_flyout();
        });
    }
}

/// How a key press moves through the menu.
#[derive(Clone, Copy)]
enum Step {
    Previous,
    Next,
    First,
    Last,
    Confirm,
    /// Step into the submenu the selection opens.
    Open,
    /// Leave this submenu for the row it was opened from.
    Close,
}

/// Take the menu off the screen. The windows themselves are kept for the next one.
pub fn dismiss_modern_menu(cx: &mut App) {
    hide_menus_from(0, cx);
}

/// The window that draws menus at `level`, built the first time that level is
/// asked for.
///
/// Levels are built in order: a level is only ever reached through the row of
/// the level above, which has to be on screen for that to happen.
fn menu_window(
    level: usize,
    bounds: Bounds<Pixels>,
    appearance: WindowAppearance,
    cx: &mut App,
) -> Option<WindowHandle<MenuView>> {
    let windows = &cx.default_global::<MenuWindow>().windows;
    if let Some(window) = windows.get(level) {
        return Some(*window);
    }
    if windows.len() != level {
        return None;
    }

    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            kind: WindowKind::PopUp,
            // Composed in the window's own tree rather than asked of the system:
            // the menu never takes activation, and a system backdrop is not drawn
            // for a window that is not active.
            window_background: WindowBackgroundAppearance::CompositedBlur,
            // Shown as a flyout instead, which does not activate it.
            show: false,
            // `None` is what drops the frame; the default carries a titlebar.
            titlebar: None,
            window_appearance_override: Some(appearance),
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            // Built once and reused by every menu, so it stays open for the
            // life of the process. Counting it as an open window would leave
            // an app that quits on its last window closed running forever.
            keeps_app_alive: false,
            ..Default::default()
        },
        |_, cx| cx.new(|_| MenuView::new()),
    );

    match opened {
        Ok(window) => {
            cx.default_global::<MenuWindow>().windows.push(window);
            Some(window)
        }
        Err(error) => {
            log::error!("failed to build the modern menu window: {error:#}");
            None
        }
    }
}

/// The menu follows the component theme rather than the system setting, so it
/// matches the surface it was opened from.
fn window_appearance(cx: &App) -> WindowAppearance {
    if cx.theme().is_dark() {
        WindowAppearance::Dark
    } else {
        WindowAppearance::Light
    }
}

/// What an item looks like once lifted out of the view for rendering, so that
/// the closures built from it own what they need rather than borrowing the view
/// they are being built inside.
type Snapshot = (SharedString, bool, Activation, Option<Icon>);

fn snapshot(item: &Item) -> Snapshot {
    (
        item.label.clone(),
        item.disabled,
        item.activation.clone(),
        item.icon.clone(),
    )
}

enum Row {
    Separator,
    Item(usize, Snapshot),
    Commands(Vec<(usize, Snapshot)>),
    /// A row that opens the menu beside it, with the label and icon it shows.
    Submenu(usize, SharedString, Option<Icon>),
}

/// Settle the rules between entries.
///
/// Drops separators with nothing to separate — one before anything, one after the
/// last, and any following another — and rules off a command row from whatever
/// comes after it, since that row reads as a band of its own rather than as more
/// of the list.
fn normalize_separators(entries: Vec<Entry>) -> Vec<Entry> {
    let mut normalized: Vec<Entry> = Vec::with_capacity(entries.len() + 1);
    for entry in entries {
        let redundant = matches!(entry, Entry::Separator)
            && matches!(normalized.last(), None | Some(Entry::Separator));
        if redundant {
            continue;
        }

        if !matches!(entry, Entry::Separator)
            && matches!(normalized.last(), Some(Entry::Commands(_)))
        {
            normalized.push(Entry::Separator);
        }
        normalized.push(entry);
    }
    while matches!(normalized.last(), Some(Entry::Separator)) {
        normalized.pop();
    }

    normalized
}

struct MenuView {
    entries: Vec<Entry>,
    /// Index into `entries` of the item the keyboard is on, if the keyboard has
    /// been used since the menu was opened. Separators are never selected.
    selected: Option<usize>,
    /// Resolved when the menu is shown, so that the font labels are measured
    /// with is the font they are drawn with. `None` before the first one.
    font: Option<Font>,
    /// The window the menu is currently open over, set on every open because the
    /// one menu window serves every application window. `None` until the window
    /// has been shown for the first time.
    owner: Option<AnyWindowHandle>,
    input: ModernMenuInput,
    /// How deep this menu is nested, which is also the index of the window it
    /// is drawn in.
    level: usize,
    /// Where this menu was placed, in the space the work area is expressed in.
    /// A submenu is placed against it, so it has to survive the frame that
    /// showed it.
    bounds: Bounds<Pixels>,
    work_area: Bounds<Pixels>,
    /// The entry whose submenu is currently drawn beside this menu.
    open_child: Option<usize>,
}

impl MenuView {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: None,
            font: None,
            owner: None,
            input: ModernMenuInput::Mouse,
            level: 0,
            bounds: Bounds::default(),
            work_area: Bounds::default(),
            open_child: None,
        }
    }

    /// How far below the top of this menu the row at `index` starts.
    fn row_offset(&self, index: usize) -> Pixels {
        self.entries[..index]
            .iter()
            .map(|entry| entry.height(self.input))
            .fold(px(0.0), |total, height| total + height)
    }

    /// Draw the submenu at `index` beside this menu, replacing whatever was open.
    ///
    /// Deferred because it shows another window while this one is mid-update,
    /// and because a hover that arrives during a frame would otherwise place the
    /// submenu against bounds that frame is still changing.
    fn open_submenu(&mut self, index: usize, select_first: bool, cx: &mut App) {
        if self.open_child == Some(index) {
            return;
        }
        let Some(Entry::Submenu(submenu)) = self.entries.get(index) else {
            return;
        };
        let (Some(owner), Some(font)) = (self.owner, self.font.clone()) else {
            return;
        };

        let presentation = Presentation {
            level: self.level + 1,
            entries: submenu.entries.clone(),
            placement: Placement::Beside {
                parent: self.bounds,
                row_offset: self.row_offset(index),
            },
            input: self.input,
            font,
            work_area: self.work_area,
            owner,
            select_first,
        };
        self.open_child = Some(index);

        cx.defer(move |cx| {
            // Anything the previous row opened deeper down belongs to a path the
            // pointer has left.
            hide_menus_from(presentation.level + 1, cx);
            present(presentation, cx);
        });
    }

    /// Close whatever this menu has open beside it.
    fn close_submenu(&mut self, cx: &mut App) {
        if self.open_child.take().is_none() {
            return;
        }

        let from = self.level + 1;
        cx.defer(move |cx| hide_menus_from(from, cx));
    }

    /// Act on a key press, reporting whether it was used.
    ///
    /// Nothing is selected until a key arrives, so the first press lands on the
    /// end the user pressed towards: Down starts at the top, Up at the bottom.
    /// Confirming without a selection does nothing rather than guessing.
    fn walk(&mut self, step: Step, cx: &mut Context<Self>) -> bool {
        let selectable: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.selectable())
            .map(|(index, _)| index)
            .collect();
        let Some(&first) = selectable.first() else {
            return false;
        };
        let &last = selectable.last().expect("a non-empty selection has a last");

        match step {
            // Both confirm a row, and on a row that opens a submenu both mean
            // stepping into it, landing on its first entry so the keyboard has
            // somewhere to go from there.
            Step::Confirm | Step::Open => {
                let Some(selected) = self.selected else {
                    return false;
                };
                match self.entries.get(selected) {
                    Some(Entry::Item(item)) => {
                        let activation = item.activation.clone();
                        self.choose(activation, cx);
                    }
                    Some(Entry::Submenu(_)) => self.open_submenu(selected, true, cx),
                    Some(Entry::Separator | Entry::Commands(_)) | None => return false,
                }
                return true;
            }
            // The row this menu was opened from goes on showing the path, so
            // leaving is only ever a level at a time.
            Step::Close => {
                let level = self.level;
                cx.defer(move |cx| {
                    hide_menus_from(level, cx);
                    if let Some((_, parent)) = deepest_menu(cx) {
                        let _ = parent.update(cx, |view, _, cx| {
                            view.open_child = None;
                            cx.notify();
                        });
                    }
                });
                return true;
            }
            Step::Previous | Step::Next | Step::First | Step::Last => {}
        }

        // `position` is where the selection sits among the selectable entries,
        // which is what makes stepping skip separators and disabled items
        // without having to walk past them one at a time.
        let position = self
            .selected
            .and_then(|selected| selectable.iter().position(|&index| index == selected));
        self.selected = Some(match (step, position) {
            (Step::First, _) => first,
            (Step::Last, _) => last,
            (Step::Next, None) => first,
            (Step::Previous, None) => last,
            (Step::Next, Some(position)) => selectable[(position + 1) % selectable.len()],
            (Step::Previous, Some(position)) => {
                selectable[(position + selectable.len() - 1) % selectable.len()]
            }
            (Step::Confirm | Step::Open | Step::Close, _) => {
                unreachable!("stepping into and out of a menu returned above")
            }
        });
        // A submenu belongs to the row the pointer or the keyboard is on, so it
        // closes as soon as the selection moves off that row.
        self.close_submenu(cx);
        cx.notify();

        true
    }

    /// Close the menu, then carry out the chosen item against the owner window.
    ///
    /// Against the owner rather than the menu's own window because a handler was
    /// written for the window the menu was opened from, and an action has to
    /// reach that window's element tree to find anything listening for it.
    /// Deferring keeps both off the stack of the view being updated.
    fn choose(&self, activation: Activation, cx: &mut App) {
        let Some(owner) = self.owner else {
            return;
        };

        cx.defer(move |cx| {
            dismiss_modern_menu(cx);
            let _ = owner.update(cx, |_, window, cx| match &activation {
                Activation::Handler(handler) => handler(window, cx),
                Activation::Action(action) => window.dispatch_action(action.boxed_clone(), cx),
            });
        });
    }
}
