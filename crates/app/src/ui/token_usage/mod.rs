//! Titlebar widget showing today's Claude token usage from `ccusage`.
//!
//! The background refresh reads the daily JSON report. The titlebar keeps a
//! compact total while the hover card shows exact totals and per-model input,
//! output, cache creation, cache-read counts, and prices.

use std::time::Duration;

use chrono::Local;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, Pixels, SharedString, Window, div,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::hover_card::HoverCard;
use gpui_component::list::{List, ListDelegate, ListItem, ListState};
use gpui_component::{ActiveTheme as _, IconNamed, IndexPath, Sizable as _, h_flex, v_flex};
use nmt_i18n::i18n;
use nmt_platform::windows::process::hidden_cmd_command;
use serde::Deserialize;
use serde_json::from_slice;
use tracing::warn;

use crate::ui::auto_refresh::{self, AutoRefresh, RefreshState};
use crate::ui::{AppSettings, UI_RADIUS};

/// Shown before the first successful fetch and retained after fetch errors.
const PLACEHOLDER: &str = "-";

const USAGE_PANEL_WIDTH: Pixels = px(800.0);
const MODEL_ROW_HEIGHT: f32 = 40.0;
const MODEL_HEADER_HEIGHT: f32 = 32.0;
const MAX_VISIBLE_ROWS: f32 = 8.0;
const INPUT_COLUMN: Pixels = px(80.0);
const OUTPUT_COLUMN: Pixels = px(80.0);
const CACHE_CREATION_COLUMN: Pixels = px(104.0);
const CACHE_READ_COLUMN: Pixels = px(96.0);
const TOTAL_COLUMN: Pixels = px(96.0);
const PRICE_COLUMN: Pixels = px(88.0);

struct TokenIcon;

impl IconNamed for TokenIcon {
    fn path(self) -> SharedString {
        "icons/coins.svg".into()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TokenCounts {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

impl TokenCounts {
    fn total(self) -> u64 {
        if self.total_tokens > 0 {
            self.total_tokens
        } else {
            self.input_tokens
                .saturating_add(self.output_tokens)
                .saturating_add(self.cache_creation_tokens)
                .saturating_add(self.cache_read_tokens)
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ModelTokenUsage {
    model_name: String,
    #[serde(flatten)]
    counts: TokenCounts,
    #[serde(default, rename = "cost")]
    price_usd: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DailyTokenUsage {
    #[serde(alias = "period")]
    date: String,
    #[serde(flatten)]
    counts: TokenCounts,
    #[serde(default, rename = "totalCost")]
    price_usd: f64,
    #[serde(default)]
    model_breakdowns: Vec<ModelTokenUsage>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportTotals {
    #[serde(flatten)]
    counts: TokenCounts,
    #[serde(default, rename = "totalCost")]
    price_usd: f64,
}

#[derive(Deserialize)]
struct CcusageReport {
    #[serde(default)]
    daily: Vec<DailyTokenUsage>,
    #[serde(default)]
    totals: ReportTotals,
}

pub(crate) struct TokenUsageView {
    usage: Option<DailyTokenUsage>,
    state: RefreshState,
}

impl AutoRefresh for TokenUsageView {
    type Output = Result<DailyTokenUsage, String>;

    const INTERVAL: Duration = Duration::from_secs(60);

    fn enabled(settings: &AppSettings) -> bool {
        settings.show_daily_token_usage
    }

    fn state(&mut self) -> &mut RefreshState {
        &mut self.state
    }

    fn fetch() -> Self::Output {
        let now = Local::now();
        let since = now.format("%Y%m%d").to_string();
        let date = now.format("%Y-%m-%d").to_string();
        fetch_usage(&since, &date)
    }

    fn apply(&mut self, output: Self::Output) {
        match output {
            Ok(usage) => self.usage = Some(usage),
            Err(err) => warn!("token usage refresh failed: {err}"),
        }
    }
}

impl TokenUsageView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            usage: None,
            state: RefreshState {
                refreshing: false,
                user_requested: false,
                enabled: Self::enabled(cx.global::<AppSettings>()),
            },
        };

        auto_refresh::start(&mut this, cx);

        this
    }

    fn accessibility_label(&self) -> String {
        let Some(usage) = self.usage.as_ref() else {
            return i18n("usage-token-unavailable").to_string();
        };

        let mut label = i18n("usage-token-summary")
            .replace("{total}", &format_token_count(usage.counts.total()))
            .replace("{price}", &format_price(usage.price_usd));
        for model in &usage.model_breakdowns {
            label.push_str(&format!(
                "; {}: {}, {}",
                model.model_name,
                format_token_count(model.counts.total()),
                format_price(model.price_usd)
            ));
        }
        label
    }
}

impl Render for TokenUsageView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label: SharedString = self
            .usage
            .as_ref()
            .map(|usage| compact(usage.counts.total()))
            .unwrap_or_else(|| PLACEHOLDER.to_string())
            .into();
        let usage = self.usage.clone();

        let trigger = Button::new("token-usage")
            .ghost()
            .small()
            .icon(TokenIcon)
            .label(label)
            .aria_label(self.accessibility_label())
            .loading(self.state.user_requested)
            .on_click(cx.listener(|this, _, _, cx| auto_refresh::refresh_from_user(this, cx)));

        HoverCard::new("token-usage-details")
            .anchor(gpui::Anchor::BottomLeft)
            .open_delay(Duration::from_millis(250))
            .close_delay(Duration::from_millis(150))
            .trigger(trigger)
            .content(move |_, window, cx| render_usage_panel(usage.clone(), window, cx))
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ModelUsageRow {
    label: String,
    counts: TokenCounts,
    price_usd: f64,
    is_daily_total: bool,
}

fn model_usage_rows(usage: &DailyTokenUsage) -> Vec<ModelUsageRow> {
    let mut rows = Vec::with_capacity(usage.model_breakdowns.len() + 1);
    rows.push(ModelUsageRow {
        label: i18n("usage-token-today-total").to_string(),
        counts: usage.counts,
        price_usd: usage.price_usd,
        is_daily_total: true,
    });
    rows.extend(usage.model_breakdowns.iter().map(|model| ModelUsageRow {
        label: model.model_name.clone(),
        counts: model.counts,
        price_usd: model.price_usd,
        is_daily_total: false,
    }));
    rows
}

struct ModelUsageList {
    rows: Vec<ModelUsageRow>,
}

impl ListDelegate for ModelUsageList {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let row_index = ix.row;
        let row = self.rows.get(row_index)?;
        let label = row.label.clone();
        let counts = row.counts;
        let price_usd = row.price_usd;
        let is_daily_total = row.is_daily_total;
        let ruled = row_index + 1 < self.rows.len();
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let row_color = if is_daily_total {
            foreground
        } else {
            foreground.opacity(0.86)
        };

        Some(
            ListItem::new(("token-usage-model-row", row_index))
                .h(px(MODEL_ROW_HEIGHT))
                .text_size(px(14.0))
                .when(is_daily_total, |this| {
                    this.bg(cx.theme().muted.opacity(0.22))
                })
                .when(ruled, |this| {
                    this.border_b_1().border_color(cx.theme().border)
                })
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(is_daily_total, |this| {
                                    this.font_weight(FontWeight::SEMIBOLD)
                                })
                                .text_color(row_color)
                                .child(label),
                        )
                        .child(usage_value_cell(
                            counts.input_tokens,
                            INPUT_COLUMN,
                            row_color,
                        ))
                        .child(usage_value_cell(
                            counts.output_tokens,
                            OUTPUT_COLUMN,
                            row_color,
                        ))
                        .child(usage_value_cell(
                            counts.cache_creation_tokens,
                            CACHE_CREATION_COLUMN,
                            muted,
                        ))
                        .child(usage_value_cell(
                            counts.cache_read_tokens,
                            CACHE_READ_COLUMN,
                            muted,
                        ))
                        .child(
                            usage_value_cell(counts.total(), TOTAL_COLUMN, row_color)
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(
                            price_value_cell(price_usd, PRICE_COLUMN, row_color)
                                .font_weight(FontWeight::SEMIBOLD),
                        ),
                ),
        )
    }

    fn render_section_header(
        &mut self,
        _section: usize,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        Some(model_usage_header(cx))
    }

    fn set_selected_index(
        &mut self,
        _ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
    }
}

fn usage_value_cell(tokens: u64, width: Pixels, color: gpui::Hsla) -> gpui::Div {
    div()
        .w(width)
        .flex_none()
        .text_right()
        .text_color(color)
        .child(format_token_count(tokens))
}

fn price_value_cell(price_usd: f64, width: Pixels, color: gpui::Hsla) -> gpui::Div {
    div()
        .w(width)
        .flex_none()
        .text_right()
        .text_color(color)
        .child(format_price(price_usd))
}

fn usage_header_cell(label: &'static str, width: Pixels) -> gpui::Div {
    div().w(width).flex_none().text_right().child(label)
}

fn model_usage_header(cx: &App) -> gpui::Div {
    h_flex()
        .w_full()
        .h(px(MODEL_HEADER_HEIGHT))
        .px_3()
        .gap_2()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.4))
        .text_size(px(14.0))
        .text_color(cx.theme().muted_foreground)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(i18n("usage-token-header-model")),
        )
        .child(usage_header_cell(
            i18n("usage-token-header-input"),
            INPUT_COLUMN,
        ))
        .child(usage_header_cell(
            i18n("usage-token-header-output"),
            OUTPUT_COLUMN,
        ))
        .child(usage_header_cell(
            i18n("usage-token-header-cache-create"),
            CACHE_CREATION_COLUMN,
        ))
        .child(usage_header_cell(
            i18n("usage-token-header-cache-read"),
            CACHE_READ_COLUMN,
        ))
        .child(usage_header_cell(
            i18n("usage-token-header-total"),
            TOTAL_COLUMN,
        ))
        .child(usage_header_cell(
            i18n("usage-token-header-price"),
            PRICE_COLUMN,
        ))
}

fn render_model_usage_list(
    usage: &DailyTokenUsage,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let rows = model_usage_rows(usage);
    let row_count = rows.len() as f32;
    let state: Entity<ListState<ModelUsageList>> =
        window.use_keyed_state("token-usage-model-list", cx, |window, cx| {
            ListState::new(ModelUsageList { rows: Vec::new() }, window, cx).selectable(false)
        });

    state.update(cx, |state, cx| {
        if state.delegate().rows != rows {
            state.delegate_mut().rows = rows;
            cx.notify();
        }
    });

    let height = MODEL_HEADER_HEIGHT + MODEL_ROW_HEIGHT * row_count.clamp(1.0, MAX_VISIBLE_ROWS);

    div()
        .w_full()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(UI_RADIUS)
        .overflow_hidden()
        .child(
            List::new(&state)
                .h(px(height))
                .scrollbar_visible(row_count > MAX_VISIBLE_ROWS),
        )
        .into_any_element()
}

fn render_usage_panel(
    usage: Option<DailyTokenUsage>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let waiting = usage.is_none();

    v_flex()
        .w(USAGE_PANEL_WIDTH)
        .gap_2()
        .text_size(px(14.0))
        .when_some(usage, |this, usage| {
            let list = render_model_usage_list(&usage, window, cx);
            this.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(foreground)
                            .child(i18n("usage-token-panel-title")),
                    )
                    .child(div().flex_none().text_color(muted).child(usage.date)),
            )
            .child(list)
        })
        .when(waiting, |this| {
            this.child(div().text_color(muted).child(i18n("usage-token-waiting")))
        })
        .into_any_element()
}

/// Run ccusage for `since` (yyyymmdd) and parse the requested day.
fn fetch_usage(since: &str, date: &str) -> Result<DailyTokenUsage, String> {
    let output = hidden_cmd_command("npx")
        .args(["ccusage@latest", "-j", "--since", since])
        .output()
        .map_err(|err| format!("failed to run ccusage: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "ccusage exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    parse_usage(&output.stdout, date)
}

fn parse_usage(bytes: &[u8], date: &str) -> Result<DailyTokenUsage, String> {
    let mut report: CcusageReport =
        from_slice(bytes).map_err(|err| format!("ccusage output is not valid JSON: {err}"))?;

    if let Some(usage) = report.daily.drain(..).find(|usage| usage.date == date) {
        return Ok(usage);
    }

    Ok(DailyTokenUsage {
        date: date.to_string(),
        counts: report.totals.counts,
        price_usd: report.totals.price_usd,
        model_breakdowns: Vec::new(),
    })
}

fn format_price(price_usd: f64) -> String {
    format!("${price_usd:.2}")
}

fn format_token_count(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

/// Shorten large counts so the titlebar label stays narrow.
fn compact(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.2}B", n as f64 / 1e9),
    }
}

#[cfg(test)]
mod tests;
