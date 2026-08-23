//! A context menu drawn in a window of its own.
//!
//! [`crate::menu::PopupMenu`] is drawn inside the window that opens it, so it is
//! clipped to that window and can only blur what the application has already
//! painted. This one gets a window, which lets the platform give it the material
//! and the shape it gives its own menus, and lets it extend past the owner's
//! edges.
//!
//! The window never takes activation ([`Window::attach_as_flyout_of`]). Its owner
//! keeps that, so the owner goes on rendering as the focused window — and on
//! Windows, so its own backdrop material goes on rendering at all, since DWM
//! drops that for a window that is not active. The cost is that the menu never
//! receives a key press or a press outside itself: the owner does, and is
//! responsible for calling [`dismiss_modern_menu`] when it sees one.
//!
//! ```ignore
//! ModernMenu::new()
//!     .item("Rename", move |window, cx| { /* runs against the owner window */ })
//!     .item_disabled("Close", !closeable, move |window, cx| { … })
//!     .show_at(event.position, window, cx);
//! ```

use std::rc::Rc;

use gpui::{
    Action, AnyWindowHandle, App, AppContext as _, Bounds, Context, Font, Global,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Pixels,
    Point, Render, SharedString, Styled as _, TextRun, Window, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, font,
    point, px, size,
};

use crate::{ActiveTheme as _, Icon};
use gpui::prelude::FluentBuilder as _;

mod ext;
mod metrics;
#[cfg(test)]
mod tests;

pub use ext::ModernMenuExt;

/// Runs against the window the menu was opened from, never the menu's own window.
type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

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

struct Item {
    label: SharedString,
    disabled: bool,
    activation: Activation,
    icon: Option<Icon>,
}

enum Entry {
    Separator,
    Item(Item),
    /// Items drawn side by side as icon buttons rather than stacked as rows.
    Commands(Vec<Item>),
}

/// The one menu window, built on first use and reused from then on.
///
/// Building it costs a graphics device, a swap chain and a composition tree,
/// which measured around 400ms — far too long to sit between a right click and a
/// menu. Showing the window that already exists costs a window move.
#[derive(Default)]
struct MenuWindow {
    window: Option<WindowHandle<MenuView>>,
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
        if let Some(Entry::Item(item)) = self.entries.last_mut() {
            item.icon = Some(icon.into());
        }

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
                Entry::Separator | Entry::Commands(_) => None,
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
        !self
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::Item(_) | Entry::Commands(_)))
    }

    /// Open the menu at `position`, in the owner window's coordinates.
    pub fn show_at(mut self, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        if self.is_empty() {
            return;
        }
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
        let mut native = crate::native_menu::NativeMenu::new();
        for entry in self.entries {
            native = match entry {
                Entry::Separator => native.separator(),
                Entry::Item(item) => push_native(native, item),
                Entry::Commands(items) => items.into_iter().fold(native, push_native),
            };
        }

        native.show(position, window, cx);
    }

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
        let owner = window.window_handle();

        // Placed off screen: the real rect needs the labels shaped, and they are
        // shaped below in the window that will draw them.
        let Some(menu) = menu_window(
            Bounds {
                origin: anchor,
                size: size(px(1.0), px(1.0)),
            },
            window_appearance(cx),
            cx,
        ) else {
            return;
        };

        let entries = self.entries;
        let _ = menu.update(cx, |view, menu_window, cx| {
            // Shaped in the window that will render them rather than the one the
            // menu was opened from: the two have separate text systems, and a
            // label measured against the wrong one is given room that does not
            // match what it draws.
            let widest = entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Item(item) => Some(item),
                    // A command button's label is drawn small and centred under
                    // its icon, inside a button of fixed width; it has no say in
                    // how wide the menu is.
                    Entry::Separator | Entry::Commands(_) => None,
                })
                .map(|item| {
                    let run = TextRun {
                        len: item.label.len(),
                        font: menu_font.clone(),
                        color: gpui::black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    menu_window
                        .text_system()
                        .shape_line(item.label.clone(), metrics::FONT_SIZE, &[run], None)
                        .width
                })
                .fold(px(0.0), Pixels::max);

            let content = metrics::Content {
                items: entries
                    .iter()
                    .filter(|entry| matches!(entry, Entry::Item(_)))
                    .count(),
                separators: entries
                    .iter()
                    .filter(|entry| matches!(entry, Entry::Separator))
                    .count(),
                command_rows: entries
                    .iter()
                    .filter(|entry| matches!(entry, Entry::Commands(_)))
                    .count(),
                widest_command_row: entries
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::Commands(items) => Some(items.len()),
                        Entry::Item(_) | Entry::Separator => None,
                    })
                    .max()
                    .unwrap_or_default(),
            };
            let menu_size = metrics::menu_size(widest, content);
            let bounds = Bounds {
                origin: metrics::place(anchor, menu_size, work_area),
                size: menu_size,
            };

            view.entries = entries;
            view.owner = Some(owner);
            view.font = Some(menu_font);
            menu_window.attach_as_flyout_of(window);
            menu_window.show_flyout(bounds);
            cx.notify();
        });
    }
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
        menu_window(bounds, appearance, cx);
    });
}

/// Offer a key press to the menu that is up, reporting whether it was used.
///
/// The menu never takes activation, so it never receives a key press of its own;
/// the window it was opened from does, and passes them here. A caller that gets
/// `true` back should stop the press going any further, since the menu has just
/// acted on it.
pub fn dispatch_modern_menu_key(event: &KeyDownEvent, cx: &mut App) -> bool {
    let Some(window) = cx.default_global::<MenuWindow>().window else {
        return false;
    };
    // The window outlives any one menu — it is built once and reused — so its
    // existence says nothing about whether a menu is up. Its entries do, and
    // without that check every key here would be swallowed application-wide.
    let showing = window
        .update(cx, |view, _, _| !view.entries.is_empty())
        .unwrap_or(false);
    if !showing {
        return false;
    }

    let step = match event.keystroke.key.as_ref() {
        "escape" => {
            dismiss_modern_menu(cx);
            return true;
        }
        "up" => Step::Previous,
        "down" => Step::Next,
        "home" => Step::First,
        "end" => Step::Last,
        "enter" => Step::Confirm,
        _ => return false,
    };

    window
        .update(cx, |view, _, cx| view.walk(step, cx))
        .unwrap_or(false)
}

/// How a key press moves through the menu.
#[derive(Clone, Copy)]
enum Step {
    Previous,
    Next,
    First,
    Last,
    Confirm,
}

/// Take the menu off the screen. The window itself is kept for the next one.
pub fn dismiss_modern_menu(cx: &mut App) {
    let Some(window) = cx.default_global::<MenuWindow>().window else {
        return;
    };

    let _ = window.update(cx, |view, menu_window, _| {
        // The window outlives any one menu, so hiding it when nothing is up would
        // race a menu that is about to be shown.
        if view.entries.is_empty() {
            return;
        }

        // A handler can hold the entities the menu was built from alive, and
        // outliving the menu it belongs to would be surprising.
        view.entries.clear();
        view.selected = None;
        menu_window.hide_flyout();
    });
}

/// The menu window, built the first time a menu is asked for.
fn menu_window(
    bounds: Bounds<Pixels>,
    appearance: WindowAppearance,
    cx: &mut App,
) -> Option<WindowHandle<MenuView>> {
    if let Some(window) = cx.default_global::<MenuWindow>().window {
        return Some(window);
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
            cx.default_global::<MenuWindow>().window = Some(window);
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
}

impl MenuView {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: None,
            font: None,
            owner: None,
        }
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
            .filter(|(_, entry)| matches!(entry, Entry::Item(item) if !item.disabled))
            .map(|(index, _)| index)
            .collect();
        let Some(&first) = selectable.first() else {
            return false;
        };
        let &last = selectable.last().expect("a non-empty selection has a last");

        if let Step::Confirm = step {
            let Some(selected) = self.selected else {
                return false;
            };
            let Some(Entry::Item(item)) = self.entries.get(selected) else {
                return false;
            };
            let activation = item.activation.clone();
            self.choose(activation, cx);
            return true;
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
            (Step::Confirm, _) => unreachable!("confirm returned above"),
        });
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

impl Render for MenuView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let enabled_color = theme.popover_foreground;
        let disabled_color = theme.muted_foreground;
        let hover_color = theme.accent;
        let stroke_color = gpui::black().opacity(metrics::stroke_alpha(theme.is_dark()));
        let tint_color = theme.tokens.popover.opacity(metrics::TINT_ALPHA);

        let selected = self.selected;
        // Snapshotted so the closures below can move what they need without
        // borrowing the view they are being built inside. Command buttons carry
        // an id of their own because their entry holds several of them.
        let mut next_button = 0;
        let rows: Vec<Row> = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                Entry::Separator => Row::Separator,
                Entry::Item(item) => Row::Item(index, snapshot(item)),
                Entry::Commands(items) => Row::Commands(
                    items
                        .iter()
                        .map(|item| {
                            next_button += 1;
                            (next_button, snapshot(item))
                        })
                        .collect(),
                ),
            })
            .collect();

        div()
            .size_full()
            // The material itself comes from underneath this window; what is
            // painted here is the tint over it and the hairline that traces the
            // rounded frame. That hairline, with the shadow the platform puts
            // around the window, is what lifts the menu off its background.
            .bg(tint_color)
            .p(metrics::MENU_PADDING)
            .rounded(metrics::CORNER_RADIUS)
            .border(metrics::STROKE_WIDTH)
            .border_color(stroke_color)
            .font(
                self.font
                    .clone()
                    .unwrap_or_else(|| font(cx.theme().font_family.clone())),
            )
            .text_size(metrics::FONT_SIZE)
            .flex()
            .flex_col()
            .children(rows.into_iter().map(|row| {
                let (index, (label, disabled, activation, icon)) = match row {
                    Row::Separator => {
                        return div()
                            .h(metrics::SEPARATOR_HEIGHT)
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .h(metrics::SEPARATOR_THICKNESS)
                                    .w_full()
                                    .bg(stroke_color),
                            )
                            .into_any_element();
                    }
                    Row::Commands(buttons) => {
                        return div()
                            .h(metrics::COMMAND_ROW_HEIGHT)
                            .flex()
                            .items_center()
                            // Centred rather than left-aligned: the buttons keep
                            // a fixed width, so in a menu made wide by its labels
                            // a left-aligned row sits off to one side.
                            .justify_center()
                            .children(buttons.into_iter().map(
                                |(id, (label, disabled, activation, icon))| {
                                    let button = div()
                                        .id(("modern-menu-command", id))
                                        .w(metrics::COMMAND_BUTTON_WIDTH)
                                        .h_full()
                                        .rounded(metrics::ITEM_RADIUS)
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .gap(metrics::COMMAND_LABEL_GAP)
                                        .child(div().flex_none().size(metrics::ICON_SIZE).children(
                                            icon.map(|icon| icon.size(metrics::ICON_SIZE)),
                                        ))
                                        .child(
                                            div()
                                                .text_size(metrics::COMMAND_LABEL_SIZE)
                                                .child(label),
                                        );

                                    if disabled {
                                        return button.text_color(disabled_color);
                                    }

                                    button
                                        .text_color(enabled_color)
                                        .hover(|this| this.bg(hover_color))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.choose(activation.clone(), cx);
                                            }),
                                        )
                                },
                            ))
                            .into_any_element();
                    }
                    Row::Item(index, snapshot) => (index, snapshot),
                };

                let row = div()
                    .id(("modern-menu-item", index))
                    .h(metrics::ITEM_HEIGHT)
                    .px(metrics::ITEM_PADDING_X)
                    .rounded(metrics::ITEM_RADIUS)
                    .flex()
                    .items_center()
                    .gap(metrics::ICON_GAP)
                    // Kept even when the item has no icon, so the labels of one
                    // menu line up with each other.
                    .child(
                        div()
                            .flex_none()
                            .size(metrics::ICON_SIZE)
                            .children(icon.map(|icon| icon.size(metrics::ICON_SIZE))),
                    )
                    .child(label);

                if disabled {
                    return row.text_color(disabled_color).into_any_element();
                }

                row.text_color(enabled_color)
                    .when(selected == Some(index), |this| this.bg(hover_color))
                    .hover(|this| this.bg(hover_color))
                    // Moving the pointer takes the highlight back from the
                    // keyboard, so the two never show a selection at once.
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.selected != Some(index) {
                            this.selected = Some(index);
                            cx.notify();
                        }
                    }))
                    // Menus commit on release, so a press that started outside
                    // and ended on an item still chooses it.
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.choose(activation.clone(), cx);
                        }),
                    )
                    .into_any_element()
            }))
    }
}
