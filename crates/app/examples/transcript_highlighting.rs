//! Local preview of command and output rendering. No commands are executed.

use std::env;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Application, Bounds, Context, Entity, Render, Window, WindowBounds, WindowOptions, div,
    px, size,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeMode};
#[cfg(target_os = "macos")]
use gpui_macos::MacPlatform as Platform;
#[cfg(windows)]
use gpui_windows::WindowsPlatform as Platform;
use nmt_agent_utils::chat::Item;
use nmt_app_agent::profile::AgentKind;
use nmt_app_agent::settings::AgentSettings;
use nmt_app_agent::transcript::TranscriptView;
use nmt_config::agent::CollapseRows;

#[path = "../src/ui/assets.rs"]
mod assets;
#[path = "../src/syntax/mod.rs"]
mod syntax;
#[allow(dead_code)]
#[path = "../src/utils.rs"]
mod utils;

use crate::assets::AppAssets;

struct Preview {
    transcript: Entity<TranscriptView>,
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_4()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child("Transcript highlighting — expand a command to inspect its output")
                    .child(
                        Button::new("theme")
                            .label("Light / dark")
                            .on_click(|_, window, cx| {
                                let mode = if cx.theme().is_dark() {
                                    ThemeMode::Light
                                } else {
                                    ThemeMode::Dark
                                };
                                Theme::change(mode, Some(window), cx);
                                window.refresh();
                            }),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.transcript.clone()))
    }
}

fn command(id: &str, purpose: &str, command: &str, output: String) -> Item {
    Item::CommandExecution {
        id: id.into(),
        command: command.into(),
        purpose: Some(purpose.into()),
        aggregated_output: Some(output),
        status: Some("completed".into()),
        exit_code: Some(0),
    }
}

fn samples() -> Vec<Item> {
    vec![
        command("bash", "Bash command + JSON", "count=3\nprintf '{\"files\":%s,\"ok\":true}\\n' \"$count\"",
            "{\"files\":3,\"ok\":true,\"paths\":[\"src/main.rs\",\"src/lib.rs\"]}".into()),
        command("powershell", "PowerShell command + JSON", "pwsh -NoProfile -Command '$items = Get-ChildItem src; $items | Select-Object Name, Length | ConvertTo-Json'",
            "[\n  {\"Name\": \"main.rs\", \"Length\": 1234},\n  {\"Name\": \"lib.rs\", \"Length\": 5678}\n]".into()),
        command("ansi", "ANSI colors, bold, underline, and truecolor", "cargo test --color always",
            "\x1b[1;32m    Finished\x1b[0m test profile\n\x1b[36m     Running\x1b[0m transcript tests\n\x1b[33mwarning:\x1b[0m preview message\n\x1b[38;5;208m256-color orange\x1b[0m\n\x1b[38;2;180;120;255mTruecolor violet\x1b[0m\n\x1b[4mUnderlined text\x1b[0m; **literal markdown** and ``` remain text\n\x1b[32mtest result: ok.\x1b[0m 9 passed; 0 failed".into()),
        command("diff", "Unified diff", "git diff -- src/config.rs",
            "diff --git a/src/config.rs b/src/config.rs\n--- a/src/config.rs\n+++ b/src/config.rs\n@@ -1,3 +1,3 @@\n fn default_limit() -> usize {\n-    128\n+    256\n }".into()),
        command("source", "Source from a literal file read", "cat src/main.rs",
            "// Display a greeting.\nfn main() {\n    let greeting = \"Hello, 世界\";\n    println!(\"{greeting}\");\n}\n".into()),
        command("large", "Large JSON output — scroll to inspect later rows", "cat results.json",
            format!("[\n{}\n]", (0..2000).map(|row| format!("  {{\"row\": {row}, \"status\": \"ok\", \"label\": \"entry {row}\"}}"))
                .collect::<Vec<_>>().join(",\n"))),
    ]
}

fn main() {
    assert!(
        env::args().any(|arg| arg == "--testing"),
        "launch this preview with --testing"
    );
    nmt_config::enable_testing_mode();
    #[cfg(windows)]
    let platform = Rc::new(Platform::new(false).expect("initialize preview platform"));
    #[cfg(target_os = "macos")]
    let platform = Rc::new(Platform::new(false));
    Application::with_platform(platform)
        .with_assets(AppAssets)
        .run(|cx: &mut App| {
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            gpui_component::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            syntax::register_languages()
                .expect("place the preview beside the built syntax library");
            cx.set_global(AgentSettings {
                font_family: ".SystemUIFont".into(),
                transcript_font_family: if cfg!(windows) {
                    "Cascadia Mono"
                } else {
                    "Menlo"
                }
                .into(),
                collapse_tool_calls: CollapseRows::Off,
                reduce_motion: true,
                ..Default::default()
            });
            let bounds = Bounds::centered(None, size(px(1100.), px(850.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("NiumaTerm — Highlight preview".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let transcript = cx.new(|cx| {
                        let mut transcript = TranscriptView::new(AgentKind::Codex, None);
                        transcript.show_items(&samples(), 1, cx);
                        transcript
                    });
                    let preview = cx.new(|_| Preview { transcript });
                    cx.new(|cx| Root::new(preview, window, cx))
                },
            )
            .expect("open highlight preview");
            cx.activate(true);
        });
}
