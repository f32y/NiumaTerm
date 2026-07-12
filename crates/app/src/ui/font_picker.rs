//! Terminal font picker for the settings dialog: a searchable `Select` whose
//! rows render each font name in that font, with a monospace-only filter.
//!
//! The `Select` entity and the font scan are cached in a gpui global for the
//! app lifetime, because the settings view (and its field closures) is rebuilt
//! on every render.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Entity, Global, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, TextRun, Window, div, px,
};
use gpui_component::AxisExt as _;
use gpui_component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::setting::SettingField;

use crate::ui::AppSettings;

/// One installed font family in the picker list.
#[derive(Clone)]
struct FontItem {
    name: SharedString,
}

impl SearchableListItem for FontItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn value(&self) -> &SharedString {
        &self.name
    }

    /// Dropdown row: the font name rendered in the font itself.
    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .font_family(self.name.clone())
            .child(self.name.clone())
    }

    /// Trigger button: also rendered in the font itself.
    fn display_title(&self) -> Option<gpui::AnyElement> {
        Some(
            div()
                .font_family(self.name.clone())
                .child(self.name.clone())
                .into_any_element(),
        )
    }
}

type FontSelectState = SelectState<SearchableVec<FontItem>>;

/// Which font setting a picker edits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FontTarget {
    /// The terminal view font; the monospace-only filter applies.
    Terminal,
    /// The app chrome font; all installed families are shown.
    Ui,
}

/// Picker state cached across settings-dialog rebuilds.
struct FontPicker {
    select: Entity<FontSelectState>,
    /// All installed families with their measured monospace flag.
    fonts: Vec<(SharedString, bool)>,
    /// The `monospace_only` value the current item set was built with.
    applied_monospace_only: bool,
    _confirm: Subscription,
}

/// One cached picker per target (built lazily on first use of each).
#[derive(Default)]
struct FontPickerGlobal {
    terminal: Option<FontPicker>,
    ui: Option<FontPicker>,
}

impl Global for FontPickerGlobal {}

/// The current family for `target`, read from `AppSettings`.
fn current_family(target: FontTarget, cx: &App) -> SharedString {
    let settings = cx.global::<AppSettings>();
    match target {
        FontTarget::Terminal => settings.terminal_font_family.clone(),
        FontTarget::Ui => settings.ui_font_family.clone(),
    }
}

/// Whether `target`'s list is filtered to monospace fonts (terminal only).
fn monospace_filter(target: FontTarget, cx: &App) -> bool {
    matches!(target, FontTarget::Terminal) && cx.global::<AppSettings>().monospace_only
}

fn slot(target: FontTarget, cx: &App) -> &Option<FontPicker> {
    let global = cx.global::<FontPickerGlobal>();
    match target {
        FontTarget::Terminal => &global.terminal,
        FontTarget::Ui => &global.ui,
    }
}

fn slot_mut(target: FontTarget, cx: &mut App) -> &mut Option<FontPicker> {
    let global = cx.global_mut::<FontPickerGlobal>();
    match target {
        FontTarget::Terminal => &mut global.terminal,
        FontTarget::Ui => &mut global.ui,
    }
}

/// A "Font Family" setting field: a searchable font `Select` bound to
/// `target`'s font setting.
pub fn font_family_field(target: FontTarget) -> SettingField<SharedString> {
    SettingField::render(move |options, window, cx| {
        let select = ensure_picker(target, window, cx);
        Select::new(&select)
            .menu_width(px(320.))
            .when(options.layout.is_vertical(), |this| this.w_full())
    })
}

/// Build (once) or resync the cached picker for `target`, returning its Select.
fn ensure_picker(target: FontTarget, window: &mut Window, cx: &mut App) -> Entity<FontSelectState> {
    cx.default_global::<FontPickerGlobal>();

    let monospace_only = monospace_filter(target, cx);
    let family = current_family(target, cx);

    if slot(target, cx).is_none() {
        let fonts = scan_fonts(window);
        let items = font_items(&fonts, monospace_only);
        let select = cx.new(|cx| {
            SelectState::new(SearchableVec::new(items), None, window, cx).searchable(true)
        });
        select.update(cx, |state, cx| {
            state.set_selected_value(&family, window, cx);
        });
        let confirm = cx.subscribe(&select, move |_, event: &SelectEvent<_>, cx| {
            if let SelectEvent::Confirm(Some(name)) = event {
                let settings = cx.global_mut::<AppSettings>();
                match target {
                    FontTarget::Terminal => settings.terminal_font_family = name.clone(),
                    FontTarget::Ui => settings.ui_font_family = name.clone(),
                }
            }
        });
        *slot_mut(target, cx) = Some(FontPicker {
            select,
            fonts,
            applied_monospace_only: monospace_only,
            _confirm: confirm,
        });
    }

    let picker = slot(target, cx).as_ref().expect("set above");
    let select = picker.select.clone();

    // The monospace switch toggled since the item set was built: refilter.
    if picker.applied_monospace_only != monospace_only {
        let items = font_items(&picker.fonts, monospace_only);
        select.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(items), window, cx);
            state.set_selected_value(&family, window, cx);
        });
        slot_mut(target, cx)
            .as_mut()
            .expect("set above")
            .applied_monospace_only = monospace_only;
    }

    select
}

fn font_items(fonts: &[(SharedString, bool)], monospace_only: bool) -> Vec<FontItem> {
    fonts
        .iter()
        .filter(|(_, mono)| !monospace_only || *mono)
        .map(|(name, _)| FontItem { name: name.clone() })
        .collect()
}

/// Enumerate installed families and measure each for monospace. One-time cost
/// on first opening the Appearance page; cached for the app lifetime.
fn scan_fonts(window: &mut Window) -> Vec<(SharedString, bool)> {
    window
        .text_system()
        .all_font_names()
        .into_iter()
        .map(|name| {
            let mono = is_monospace(&name, window);
            (SharedString::from(name), mono)
        })
        .collect()
}

/// Monospace heuristic: 'i' and 'M' have the same advance. Fonts without
/// those glyphs shape via fallback (proportional UI font) and read as
/// proportional, which is fine for a list filter.
fn is_monospace(family: &str, window: &mut Window) -> bool {
    const SAMPLE: &str = "iM";
    let run = TextRun {
        len: SAMPLE.len(),
        font: gpui::font(family),
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(SAMPLE.into(), px(14.), &[run], None);
    let i_width = line.x_for_index(1).as_f32();
    let m_width = (line.x_for_index(2) - line.x_for_index(1)).as_f32();
    i_width > 0.0 && (i_width - m_width).abs() < 0.5
}
