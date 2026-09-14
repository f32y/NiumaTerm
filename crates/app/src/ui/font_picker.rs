//! Font metadata is shared, while each window owns its picker controls.
//! Keyed state keeps those controls stable across settings-view renders.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Entity, Global, IntoElement, ParentElement as _,
    SharedString, Styled as _, Subscription, TextRun, Window, black, div, font, px,
};
use gpui_component::AxisExt as _;
use gpui_component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::setting::SettingField;

use crate::ui::AppSettings;

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

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .font_family(self.name.clone())
            .child(self.name.clone())
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(
            div()
                .font_family(self.name.clone())
                .child(self.name.clone())
                .into_any_element(),
        )
    }
}

type FontSelectState = SelectState<SearchableVec<FontItem>>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FontTarget {
    Terminal,
    Ui,
    Agent,
    AgentTranscript,
}

struct FontPicker {
    select: Entity<FontSelectState>,

    /// The `monospace_only` value the current item set was built with.
    applied_monospace_only: bool,

    _confirm: Subscription,
}

struct FontCatalog(Vec<(SharedString, bool)>);

impl Global for FontCatalog {}

fn current_family(target: FontTarget, cx: &App) -> SharedString {
    let settings = cx.global::<AppSettings>();

    match target {
        FontTarget::Terminal => settings
            .config()
            .appearance
            .terminal_font_family
            .clone()
            .into(),
        FontTarget::Ui => settings.config().appearance.ui_font.clone().into(),
        FontTarget::Agent => settings
            .config()
            .appearance
            .agent_font_family
            .clone()
            .into(),
        FontTarget::AgentTranscript => settings
            .config()
            .appearance
            .agent_transcript_font_family
            .clone()
            .into(),
    }
}

fn monospace_filter(target: FontTarget, cx: &App) -> bool {
    matches!(target, FontTarget::Terminal | FontTarget::AgentTranscript)
        && cx
            .global::<AppSettings>()
            .config()
            .appearance
            .monospace_only
}

pub fn font_family_field(target: FontTarget) -> SettingField<SharedString> {
    SettingField::render(move |options, window, cx| {
        let select = ensure_picker(target, window, cx);

        Select::new(&select)
            .menu_width(px(320.))
            .when(options.layout().is_vertical(), |this| this.w_full())
    })
}

fn ensure_picker(target: FontTarget, window: &mut Window, cx: &mut App) -> Entity<FontSelectState> {
    if !cx.has_global::<FontCatalog>() {
        cx.set_global(FontCatalog(scan_fonts(window)));
    }

    let monospace_only = monospace_filter(target, cx);
    let family = current_family(target, cx);

    let picker = window.use_keyed_state(("font-picker", target as usize), cx, |window, cx| {
        let items = font_items(&cx.global::<FontCatalog>().0, monospace_only);

        let select = cx.new(|cx| {
            SelectState::new(SearchableVec::new(items), None, window, cx).searchable(true)
        });

        select.update(cx, |state, cx| {
            state.set_selected_value(&family, window, cx);
        });

        let confirm = cx.subscribe(&select, move |_, _, event: &SelectEvent<_>, cx| {
            if let SelectEvent::Confirm(Some(name)) = event {
                let settings = cx.global_mut::<AppSettings>();

                settings.edit_appearance(|appearance| match target {
                    FontTarget::Terminal => appearance.terminal_font_family = name.to_string(),
                    FontTarget::Ui => appearance.ui_font = name.to_string(),
                    FontTarget::Agent => appearance.agent_font_family = name.to_string(),
                    FontTarget::AgentTranscript => {
                        appearance.agent_transcript_font_family = name.to_string()
                    }
                });
            }
        });

        FontPicker {
            select,
            applied_monospace_only: monospace_only,
            _confirm: confirm,
        }
    });

    let select = picker.read(cx).select.clone();

    if picker.read(cx).applied_monospace_only != monospace_only {
        let items = font_items(&cx.global::<FontCatalog>().0, monospace_only);

        select.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(items), window, cx);

            state.set_selected_value(&family, window, cx);
        });

        picker.update(cx, |picker, _| {
            picker.applied_monospace_only = monospace_only
        });
    }

    if select.read(cx).selected_value() != Some(&family) {
        select.update(cx, |state, cx| {
            state.set_selected_value(&family, window, cx)
        });
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

            (name.into(), mono)
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
        font: font(family),
        color: black(),
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

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, IntoElement, Render, TestAppContext, VisualTestContext,
        WeakEntity, Window, div,
    };

    use crate::ui::AppSettings;
    use crate::ui::font_picker::{FontCatalog, FontSelectState, FontTarget, ensure_picker};

    #[derive(Default)]
    struct PickerHost {
        select: Option<WeakEntity<FontSelectState>>,
        hidden: bool,
    }

    impl Render for PickerHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if !self.hidden {
                self.select = Some(ensure_picker(FontTarget::Ui, window, cx).downgrade());
            }

            div()
        }
    }

    #[gpui::test]
    fn font_pickers_are_local_to_rendered_windows(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);

            cx.set_global(AppSettings::default());

            cx.set_global(FontCatalog(vec![
                ("First".into(), true),
                ("Second".into(), false),
            ]));

            cx.global_mut::<AppSettings>()
                .edit_appearance(|section| section.ui_font = "First".into());
        });

        let first = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| PickerHost::default())
            })
            .unwrap()
        });

        let second = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| PickerHost::default())
            })
            .unwrap()
        });

        cx.run_until_parked();

        let first_select = first
            .read_with(cx, |host, _| host.select.clone().unwrap())
            .unwrap();

        let second_select = second
            .read_with(cx, |host, _| host.select.clone().unwrap())
            .unwrap();

        assert_ne!(first_select.entity_id(), second_select.entity_id());

        cx.update(|cx| {
            cx.global_mut::<AppSettings>()
                .edit_appearance(|section| section.ui_font = "Second".into())
        });

        let mut cx = VisualTestContext::from_window(first.into(), cx);

        first.update(&mut cx, |_, _, cx| cx.notify()).unwrap();

        cx.run_until_parked();

        cx.refresh().unwrap();

        cx.update(|_, cx| {
            assert_eq!(
                first_select
                    .upgrade()
                    .unwrap()
                    .read(cx)
                    .selected_value()
                    .unwrap()
                    .as_ref(),
                "Second"
            );
            assert_eq!(
                second_select
                    .upgrade()
                    .unwrap()
                    .read(cx)
                    .selected_value()
                    .unwrap()
                    .as_ref(),
                "First"
            );
        });

        first
            .update(&mut cx, |host, _, cx| {
                host.hidden = true;

                cx.notify();
            })
            .unwrap();

        cx.run_until_parked();

        cx.refresh().unwrap();

        cx.run_until_parked();

        first.update(&mut cx, |_, _, cx| cx.notify()).unwrap();

        cx.run_until_parked();

        cx.refresh().unwrap();

        cx.run_until_parked();

        assert!(first_select.upgrade().is_none());
        assert!(second_select.upgrade().is_some());
    }
}
