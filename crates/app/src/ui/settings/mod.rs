//! Persisted to `config.toml`: seeded via [`AppSettings::load`] at startup,
//! written back patch-style via [`AppSettings::save`] once when the settings
//! dialog closes (see `Shell::on_show_settings`). Field edits mutate the global
//! live for preview; only closing the dialog persists them.

mod opacity;
mod state;
mod theme;

use std::{io, path};

#[cfg(test)]
use gpui::WindowBackgroundAppearance;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Div, Entity, FileDialogFilter, Global,
    IntoElement as _, ParentElement as _, PathPromptOptions, SharedString, StyleRefinement,
    Styled as _, Subscription, Window, div, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DialogClose, DialogFooter};
use gpui_component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, AxisExt as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use nmt_agent_utils::HookInstallStatus;
use nmt_agent_utils::claude_code::hook as claude_hook;
use nmt_agent_utils::codex::hook as codex_hook;
use nmt_agent_utils::update::{DiscoverySupport, InstallationKey, ProviderKind, UpdatePhase};
#[cfg(test)]
use nmt_config::CursorShape;
use nmt_config::appearance::SmoothScrollingMode;
use nmt_config::remote_session::RemoteSessionConfig;
use nmt_config::system::WarnBeforeTerminatingShell;
use nmt_platform::{
    is_shell_integration_registered, register_shell_integration, set_system_notification_enabled,
    shell_integration_dll_mismatched, system_notification_enabled, unregister_shell_integration,
};
#[allow(unused_imports)]
pub(crate) use opacity::{
    background_image_layer_opacity, main_view_background_opacity, surface_background_opacity,
    window_background_appearance,
};
#[cfg(test)]
use opacity::{
    effective_background_image_layer_opacity, effective_background_opacity,
    effective_main_view_background_opacity, effective_surface_background_opacity,
    window_background_appearance_for,
};
pub(crate) use state::builtin_agent_profile;
pub use state::{AgentProfile, AgentProfileKind, AppSettings, EnvVar, InputStyle, Profile};
#[cfg(test)]
use state::{
    DEFAULT_BACKGROUND_IMAGE_OPACITY, clamp_terminal_font_size, clamp_terminal_line_height,
    terminal_font_or_default, ui_font_or_default,
};
#[allow(unused_imports)]
pub use state::{
    DEFAULT_FONT_FAMILY, DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, DEFAULT_SHELL, DEFAULT_TAB_WIDTH,
    DEFAULT_UI_FONT,
};
use state::{
    agent_kind_label, clamp_background_image_opacity, clamp_background_opacity, clamp_git_interval,
    clamp_tab_width, cursor_shape_from_value, input_style_from_value, input_style_label,
};
#[cfg(test)]
use theme::tab_background_opacity;
use theme::theme_list;
pub(crate) use theme::{apply_ui_theme, apply_window_translucency, watch_themes};
use tracing::warn;

use crate::agent_pane::updates as agent_updates;
use crate::ui::UI_RADIUS;
use crate::{PlatformHandle, remote, ui};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASE_PAGE_URL: &str = "https://github.com/f32y/NiumaTerm/releases";

pub const MAX_TAB_WIDTH: f64 = DEFAULT_TAB_WIDTH * 3.0;

/// Draft edited in the agent-profile dialog: `target` is the list index in
/// edit mode, `None` while adding. Inputs write here; only Save commits the
/// draft into `AppSettings`, so Cancel is a plain close.
#[derive(Default)]
struct AgentProfileDraft {
    target: Option<usize>,
    profile: AgentProfile,
}

impl Global for AgentProfileDraft {}

/// Persistent input state for a text field inside a profile card, created
/// via `window.use_keyed_state` so it survives the per-frame settings-view
/// rebuild. The subscription writes edits back into the `AppSettings` global.
struct CardInputState {
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// A window-keyed text input bound to a value in the `AppSettings` global.
/// `apply` receives the new text on every change. When the backing value
/// changes underneath a reused key (e.g. a profile removal shifts indices),
/// the sync below rewrites the input to match.
fn card_text_input(
    key: String,
    value: SharedString,
    masked: bool,
    apply: impl Fn(String, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let state = window.use_keyed_state(SharedString::from(key), cx, {
        let value = value.clone();

        move |window, cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(masked)
                    .default_value(value)
            });

            let _subscription = cx.subscribe(&input, move |_, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    apply(value, cx);
                }
            });

            CardInputState {
                input,
                _subscription,
            }
        }
    });

    let input = state.read(cx).input.clone();

    if input.read(cx).value() != value {
        input.update(cx, |input, cx| {
            input.set_value(value.clone(), window, cx);
        });
    }

    input
}

#[derive(Clone, Copy)]
enum OpacityTarget {
    Window,
    Image,
}

impl OpacityTarget {
    fn value(self, settings: &AppSettings) -> f64 {
        match self {
            Self::Window => settings.background_opacity,
            Self::Image => settings.background_image_opacity,
        }
    }

    fn min(self) -> f32 {
        match self {
            Self::Window => 0.2,
            Self::Image => 0.0,
        }
    }

    fn set(self, value: f64, settings: &mut AppSettings) {
        match self {
            Self::Window => settings.background_opacity = clamp_background_opacity(value),
            Self::Image => {
                settings.background_image_opacity = clamp_background_image_opacity(value)
            }
        }
    }
}

/// Both opacity fields share persistent slider entities because the settings
/// view and its field closures are rebuilt every render.
struct OpacitySliderState {
    window: Entity<SliderState>,
    image: Entity<SliderState>,
    _subscriptions: [Subscription; 2],
}

impl Global for OpacitySliderState {}

fn opacity_slider_field(target: OpacityTarget) -> SettingField<SharedString> {
    SettingField::render(move |options, window, cx| {
        if !cx.has_global::<OpacitySliderState>() {
            let make_slider = |target: OpacityTarget, cx: &mut App| {
                let value = target.value(cx.global::<AppSettings>()) as f32;
                let slider = cx.new(|_| {
                    SliderState::new()
                        .min(target.min())
                        .max(1.0)
                        .step(0.05)
                        .default_value(value)
                });

                let subscription = cx.subscribe(&slider, move |_, event: &SliderEvent, cx| {
                    let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                    target.set(value.end() as f64, cx.global_mut::<AppSettings>());
                });

                (slider, subscription)
            };

            let (window_slider, window_subscription) = make_slider(OpacityTarget::Window, cx);
            let (image_slider, image_subscription) = make_slider(OpacityTarget::Image, cx);

            cx.set_global(OpacitySliderState {
                window: window_slider,
                image: image_slider,
                _subscriptions: [window_subscription, image_subscription],
            });
        }

        let sliders = cx.global::<OpacitySliderState>();
        let slider = match target {
            OpacityTarget::Window => &sliders.window,
            OpacityTarget::Image => &sliders.image,
        }
        .clone();

        let current = target.value(cx.global::<AppSettings>()) as f32;

        if (slider.read(cx).value().end() - current).abs() > 0.001 {
            slider.update(cx, |state, cx| state.set_value(current, window, cx));
        }

        h_flex()
            // The setting row's field slot is auto-sized, so a percentage
            // width resolves to the content width (zero for the slider bar)
            // and the whole control collapses; horizontal layout needs a
            // fixed width, like NumberField's `w_32`.
            .map(|this| {
                if options.layout.is_horizontal() {
                    this.w_56()
                } else {
                    this.w_full()
                }
            })
            .gap_2()
            // The thumb (16px, centered on the track position) overhangs the
            // track by 8px at either end; pad so it stays inside the setting
            // row's overflow_hidden instead of being clipped at min/max.
            //
            // Thumb color: the dark theme leaves `slider.thumb` unset and its
            // `primary_foreground` fallback (neutral-900) vanishes against the
            // neutral-950 panel, so use `primary`, which contrasts with the
            // panel in both modes.
            .child(
                div().flex_1().px_2().child(
                    Slider::new(&slider)
                        .disabled(options.disabled)
                        .text_color(cx.theme().primary),
                ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .child(SharedString::from(format!("{current:.2}"))),
            )
    })
}

fn background_opacity_field() -> SettingField<SharedString> {
    opacity_slider_field(OpacityTarget::Window)
}

fn background_image_opacity_field() -> SettingField<SharedString> {
    opacity_slider_field(OpacityTarget::Image)
}

fn background_image_field() -> SettingField<SharedString> {
    SettingField::render(|options, _window, cx| {
        let path = cx.global::<AppSettings>().background_image.clone();
        let label = SharedString::from(path.clone().unwrap_or_else(|| "None".to_string()));

        h_flex()
            .map(|this| {
                if options.layout.is_horizontal() {
                    this.w_64()
                } else {
                    this.w_full()
                }
            })
            .gap_2()
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                Button::new("background-image-browse")
                    .outline()
                    .label("Browse")
                    .disabled(options.disabled)
                    .on_click(|_, window, cx| {
                        let rx = cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some("Select background image".into()),
                            file_types: vec![FileDialogFilter {
                                name: "Images".into(),
                                extensions: ["png", "jpg", "jpeg", "webp", "bmp"]
                                    .into_iter()
                                    .map(Into::into)
                                    .collect(),
                            }],
                        });
                        window
                            .spawn(cx, async move |cx| {
                                if let Ok(Ok(Some(paths))) = rx.await
                                    && let Some(path) = paths.first()
                                {
                                    let path = path.display().to_string();
                                    let _ = cx.update_global(|settings: &mut AppSettings, _, _| {
                                        settings.background_image = Some(path);
                                    });
                                }
                            })
                            .detach();
                    }),
            )
            .children(path.is_some().then(|| {
                Button::new("background-image-clear")
                    .outline()
                    .label("Clear")
                    .disabled(options.disabled)
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AppSettings>().background_image = None;
                    })
            }))
    })
}

fn agent_hook_item(
    name: &'static str,
    detection_path: Option<path::PathBuf>,
    hooks_path: Option<path::PathBuf>,
    status: fn(&path::Path) -> HookInstallStatus,
    install: fn(&path::Path) -> io::Result<()>,
    uninstall: fn(&path::Path) -> io::Result<()>,
) -> SettingItem {
    let detected = detection_path.as_ref().is_some_and(|path| path.is_file());
    let status_path = hooks_path.clone();
    let action_path = hooks_path;

    SettingItem::new(
        name,
        SettingField::checkbox(
            // Settings renders only the active page, so a disk-backed getter
            // refreshes Hook state whenever the user enters the Agent page.
            move |_| {
                status_path
                    .as_deref()
                    .is_some_and(|path| status(path) == HookInstallStatus::Installed)
            },
            move |enabled, cx| {
                let Some(path) = action_path.as_deref() else {
                    return;
                };

                let result = if enabled {
                    install(path)
                } else {
                    uninstall(path)
                };

                if let Err(error) = result {
                    warn!("failed to update {name} hooks: {error}");
                }

                cx.refresh_windows();
            },
        ),
    )
    .disabled(!detected)
}

fn agent_page(agent_profiles: &[AgentProfile], cx: &App) -> SettingPage {
    let installations = agent_updates::installations_for_profiles(agent_profiles, cx);
    let mut general = SettingGroup::new()
        .title("General")
        .item(
            SettingItem::new(
                "Enable Agent Hooks",
                SettingField::switch(
                    |cx| cx.global::<AppSettings>().enable_agent_hooks,
                    |value, cx| {
                        cx.global_mut::<AppSettings>().enable_agent_hooks = value;
                    },
                ),
            )
            .description(
                "Process new lifecycle events from installed Agent Hooks. This does not change their installation state.",
            ),
        )
        .item(
            SettingItem::new(
                "Show Agent Usage",
                SettingField::switch(
                    |cx| cx.global::<AppSettings>().show_agent_usage,
                    |value, cx| {
                        cx.global_mut::<AppSettings>().show_agent_usage = value;
                    },
                ),
            )
            .description("Show Agent account usage in the workspace sidebar."),
        )
        .item(
            SettingItem::new(
                "Collapse Tool Call details by default",
                SettingField::switch(
                    |cx| cx.global::<AppSettings>().collapse_tool_calls,
                    |value, cx| {
                        cx.global_mut::<AppSettings>().collapse_tool_calls = value;
                    },
                ),
            )
            .description(
                "In agent tabs, show only the newest of consecutive tool calls; older \
                 ones sit behind a \"+N previous tool calls\" toggle.",
            ),
        )
        .item(agent_update_check_item());

    for (index, snapshot) in installations.iter().enumerate() {
        let provider = snapshot.identity.provider;
        let provider_total = installations
            .iter()
            .filter(|item| item.identity.provider == provider)
            .count();
        let provider_ordinal = installations[..=index]
            .iter()
            .filter(|item| item.identity.provider == provider)
            .count();
        general = general.item(agent_update_status_item(
            index,
            installation_update_title(provider, provider_ordinal, provider_total),
            snapshot.identity.key.clone(),
        ));
    }

    SettingPage::new("Agent")
        .default_open(true)
        .description("Configure Agent event handling and per-Agent Hook installation.")
        .group(general)
        .group(
            SettingGroup::new()
                .title("Installed Agents")
                .item(agent_hook_item(
                    "Claude Code",
                    claude_hook::settings_path(),
                    claude_hook::settings_path(),
                    claude_hook::hooks_status,
                    claude_hook::install_hooks,
                    claude_hook::uninstall_hooks,
                ))
                .item(agent_hook_item(
                    "Codex",
                    codex_hook::config_path(),
                    codex_hook::hooks_path(),
                    codex_hook::hooks_status,
                    codex_hook::install_hooks,
                    codex_hook::uninstall_hooks,
                )),
        )
}

fn remote_session_page() -> SettingPage {
    SettingPage::new("Remote Session")
        .default_open(true)
        .description(
            "Reach this machine's terminal sessions from other computers through a relay. \
             Traffic is end-to-end encrypted; the relay only ever sees ciphertext.",
        )
        .group(
            SettingGroup::new()
                .title("Host Service")
                .item(
                    SettingItem::new(
                        "Enable Host Service",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().remote_host_enabled,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_host_enabled = value;
                                reconcile_remote_host(cx);
                            },
                        ),
                    )
                    .description(
                        "Register with the relay so paired devices can attach to sessions on \
                         this machine. Sessions keep running while no client is connected.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Relay URL",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_relay_url.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_relay_url = value;
                            },
                        ),
                    )
                    .description(
                        "WebSocket endpoint, e.g. wss://relay.example.com/ws. Applied when you \
                         toggle the service or close settings.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Access Token",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_access_token.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_access_token = value;
                            },
                        ),
                    )
                    .description("Shared secret the relay requires to register this host."),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Pairing & Devices")
                .item(SettingItem::render(|_, _, cx| remote_host_status(cx))),
        )
        .group(
            SettingGroup::new()
                .title("Connect to a Host")
                .description(
                    "Pair with another machine's host service using the code it shows, then \
                     open remote tabs with Ctrl+Shift+R.",
                )
                .item(
                    SettingItem::new(
                        "Pairing Code",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().remote_pairing_input.clone(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().remote_pairing_input = value;
                            },
                        ),
                    )
                    .description("Paste the code from the host machine, then click Pair."),
                )
                .item(SettingItem::render(|_, _, cx| remote_client_status(cx))),
        )
}

fn about_page() -> SettingPage {
    SettingPage::new("About").default_open(true).group(
        SettingGroup::new()
            .title("NiumaTerm")
            .item(SettingItem::new(
                "Version",
                SettingField::render(|_, _, _| Label::new(APP_VERSION).text_sm()),
            ))
            .item(SettingItem::new(
                "Releases",
                SettingField::render(|_, _, _| {
                    Button::new("go-to-release-page")
                        .outline()
                        .label("Go to Release Page")
                        .on_click(|_, _, cx: &mut App| cx.open_url(RELEASE_PAGE_URL))
                }),
            )),
    )
}

/// Start/stop/restart the background host service to match the live settings.
/// Called on discrete events (enable toggle, dialog close), never per keystroke.
#[cfg(windows)]
pub(crate) fn reconcile_remote_host(cx: &App) {
    let settings = cx.global::<AppSettings>();
    remote::reconcile(&RemoteSessionConfig {
        host_enabled: settings.remote_host_enabled,
        relay_url: settings.remote_relay_url.to_string(),
        access_token: settings.remote_access_token.to_string(),
    });
}

#[cfg(not(windows))]
pub(crate) fn reconcile_remote_host(_cx: &App) {}

#[cfg(windows)]
fn remote_host_status(cx: &mut App) -> Div {
    use crate::remote;

    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let surface = cx.theme().tokens.secondary;

    if !remote::is_running() {
        return v_flex().child(
            div()
                .py_2()
                .text_color(muted)
                .child("Enable the host service (with a relay URL and token) to pair devices."),
        );
    }

    let host_id = remote::host_id().unwrap_or_default();
    let pairing = cx.global::<AppSettings>().remote_pairing_code.clone();
    let devices = remote::list_devices();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex().gap_2().child("Host ID").child(
                div()
                    .font_family("monospace")
                    .text_color(muted)
                    .child(host_id),
            ),
        )
        .child(
            h_flex().justify_between().child("Pair a new device").child(
                Button::new("remote-generate-pairing")
                    .outline()
                    .label("Generate Pairing Code")
                    .on_click(|_, _, cx: &mut App| {
                        if let Some(code) = remote::begin_pairing() {
                            cx.global_mut::<AppSettings>().remote_pairing_code =
                                Some(code.encode());
                        }
                    }),
            ),
        )
        .when_some(pairing, |this, code| {
            this.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(UI_RADIUS)
                    .border_1()
                    .border_color(border)
                    .bg(surface)
                    .child(
                        div()
                            .text_color(muted)
                            .child("Enter this code on the other computer within 5 minutes:"),
                    )
                    .child(div().font_family("monospace").child(code.clone()))
                    .child(
                        Button::new("remote-copy-pairing")
                            .outline()
                            .label("Copy")
                            .on_click(move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                            }),
                    ),
            )
        })
        .child(div().mt_2().text_color(muted).child("Authorized Devices"))
        .when(devices.is_empty(), |this| {
            this.child(
                div()
                    .py_2()
                    .text_color(muted)
                    .child("No devices paired yet."),
            )
        })
        .children(devices.into_iter().enumerate().map(|(index, device)| {
            let key = device.public_key.clone();
            h_flex()
                .w_full()
                .py_2()
                .justify_between()
                .border_b_1()
                .border_color(border)
                .child(device.name)
                .child(
                    Button::new(("remote-revoke", index))
                        .outline()
                        .label("Revoke")
                        .on_click(move |_, _, cx: &mut App| {
                            remote::revoke_device(&key);
                            cx.refresh_windows();
                        }),
                )
        }))
}

#[cfg(not(windows))]
fn remote_host_status(_cx: &mut App) -> Div {
    v_flex().child(div().child("Remote sessions are only available on Windows."))
}

#[cfg(windows)]
fn remote_client_status(cx: &mut App) -> Div {
    use crate::remote;

    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let status = cx.global::<AppSettings>().remote_client_status.clone();
    let hosts = remote::known_hosts();

    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .child("Pair with this code")
                .child(Button::new("remote-pair").outline().label("Pair").on_click(
                    |_, _, cx: &mut App| {
                        let code = cx.global::<AppSettings>().remote_pairing_input.to_string();
                        if code.trim().is_empty() {
                            cx.global_mut::<AppSettings>().remote_client_status =
                                Some("Enter a pairing code first.".to_owned());
                            return;
                        }
                        cx.global_mut::<AppSettings>().remote_client_status =
                            Some("Pairing…".to_owned());
                        // Pairing is a network round trip: running it inline
                        // would freeze the window until the relay answers or
                        // the attempt times out.
                        cx.spawn(async move |cx| {
                            let paired = cx
                                .background_executor()
                                .spawn(async move { remote::pair_with_code(&code, "remote host") })
                                .await;
                            cx.update_global(|settings: &mut AppSettings, _| {
                                let message = match paired {
                                    Ok(host) => {
                                        settings.remote_pairing_input = SharedString::default();
                                        format!("Paired with {} ({}).", host.name, host.host_id)
                                    }
                                    Err(e) => format!("Pairing failed: {e}"),
                                };
                                settings.remote_client_status = Some(message);
                            })
                        })
                        .detach();
                    },
                )),
        )
        .when_some(status, |this, message| {
            this.child(div().text_color(muted).child(message))
        })
        .child(div().mt_2().text_color(muted).child("Paired Hosts"))
        .when(hosts.is_empty(), |this| {
            this.child(div().py_2().text_color(muted).child("No hosts paired yet."))
        })
        .children(hosts.into_iter().enumerate().map(|(index, host)| {
            let host_id = host.host_id.clone();
            h_flex()
                .w_full()
                .py_2()
                .justify_between()
                .border_b_1()
                .border_color(border)
                .child(
                    v_flex().child(host.name.clone()).child(
                        div()
                            .font_family("monospace")
                            .text_color(muted)
                            .child(host.host_id.clone()),
                    ),
                )
                .child(
                    Button::new(("remote-forget", index))
                        .outline()
                        .label("Forget")
                        .on_click(move |_, _, cx: &mut App| {
                            remote::forget_host(&host_id);
                            cx.refresh_windows();
                        }),
                )
        }))
}

#[cfg(not(windows))]
fn remote_client_status(_cx: &mut App) -> Div {
    v_flex()
}

fn terminal_page() -> SettingPage {
    SettingPage::new("Terminal").default_open(true).group(
        SettingGroup::new()
            .title("Input")
            .item(
                SettingItem::new(
                    "Input Style",
                    SettingField::dropdown(
                        vec![
                            (
                                InputStyle::Waterfall.as_str().into(),
                                input_style_label(InputStyle::Waterfall).into(),
                            ),
                            (
                                InputStyle::FixedBottom.as_str().into(),
                                input_style_label(InputStyle::FixedBottom).into(),
                            ),
                        ],
                        |cx| cx.global::<AppSettings>().input_style.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().input_style =
                                input_style_from_value(&value);
                        },
                    )
                    .default_value(SharedString::from(InputStyle::Waterfall.as_str())),
                )
                .description("How the prompt input is presented."),
            )
            .item(
                SettingItem::new(
                    "Cursor Shape",
                    SettingField::dropdown(
                        vec![
                            ("block".into(), "Block".into()),
                            ("line".into(), "Line".into()),
                            ("underline".into(), "Underline".into()),
                        ],
                        |cx| cx.global::<AppSettings>().cursor_shape.as_str().into(),
                        |value, cx| {
                            cx.global_mut::<AppSettings>().cursor_shape =
                                cursor_shape_from_value(&value);
                        },
                    )
                    .default_value(SharedString::from("block")),
                )
                .description("Default cursor shape used by newly opened terminals."),
            )
            .item(
                SettingItem::new(
                    "Command Blocks",
                    SettingField::switch(
                        |cx| cx.global::<AppSettings>().command_blocks,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().command_blocks = value;
                        },
                    ),
                )
                .description(
                    "Group each command's output into a block with a separator, \
                                 exit status, and duration. Off: outputs run together like a \
                                 classic terminal.",
                ),
            ),
    )
}

fn appearance_page(transparency_enabled: bool, background_image_enabled: bool) -> SettingPage {
    SettingPage::new("Appearance")
        .default_open(true)
        .group(
            SettingGroup::new()
                .title("Theme")
                .description("Themes are loaded from the themes directory and applied immediately.")
                .item(
                    SettingItem::new(
                        "Make agent pane use terminal's background color",
                        SettingField::switch(
                            |cx| {
                                cx.global::<AppSettings>()
                                    .agent_pane_use_terminal_background
                            },
                            |value, cx| {
                                cx.global_mut::<AppSettings>()
                                    .agent_pane_use_terminal_background = value;
                            },
                        ),
                    )
                    .description("Use the terminal theme's background color for Agent Pane."),
                )
                .item(
                    SettingItem::new(
                        "Search",
                        SettingField::input(
                            |cx| cx.global::<AppSettings>().theme_filter.clone().into(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().theme_filter = value.to_string();
                            },
                        ),
                    )
                    .description("Filter themes by file name or UI theme name."),
                )
                .item(
                    SettingItem::render(|_, _, cx| theme_list(cx))
                        .keywords(["theme", "colors", "palette"]),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Window")
                .item(
                    SettingItem::new(
                        "Enable Window Transparency",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().window_transparency_enabled,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().window_transparency_enabled = value;
                            },
                        ),
                    )
                    .description(
                        "Use an acrylic backdrop and preserve window alpha for live transparency.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Transparent Main View",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().transparent_main_view,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().transparent_main_view = value;
                            },
                        ),
                    )
                    .description("Use a translucent background for Terminal View and Agent Pane."),
                )
                .item(
                    SettingItem::new(
                        "Smooth Scrolling",
                        SettingField::dropdown(
                            vec![
                                ("all".into(), "All".into()),
                                ("only-terminal".into(), "Only Terminal".into()),
                                ("only-agent".into(), "Only Agent".into()),
                                ("off".into(), "Off".into()),
                            ],
                            |cx| cx.global::<AppSettings>().smooth_scrolling.as_str().into(),
                            |value, cx| {
                                cx.global_mut::<AppSettings>().smooth_scrolling =
                                    SmoothScrollingMode::from_value(&value);
                            },
                        )
                        .default_value(SharedString::from("all")),
                    )
                    .description("Choose where traditional mouse-wheel scrolling is animated."),
                )
                .item(
                    SettingItem::new("Background Opacity", background_opacity_field())
                        .description("Whole-window opacity while window transparency is enabled.")
                        .disabled(!transparency_enabled),
                )
                .item(
                    SettingItem::new("Background Image", background_image_field())
                        .description("Local image stretched to cover the whole window."),
                )
                .item(
                    SettingItem::new("Background Image Opacity", background_image_opacity_field())
                        .description("How strongly the image shows through window surfaces.")
                        .disabled(!background_image_enabled),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Font")
                .item(
                    SettingItem::new(
                        "UI Font",
                        ui::font_picker::font_family_field(ui::font_picker::FontTarget::Ui),
                    )
                    .description("Font for the app chrome (titlebar, sidebar, tabs, dialogs)."),
                )
                .item(
                    SettingItem::new(
                        "Terminal Font",
                        ui::font_picker::font_family_field(ui::font_picker::FontTarget::Terminal),
                    )
                    .description("Font used by the terminal view."),
                )
                .item(
                    SettingItem::new(
                        "Terminal Font Size",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 6.0,
                                max: 72.0,
                                step: 0.1,
                            },
                            |cx| cx.global::<AppSettings>().terminal_font_size,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().terminal_font_size = value;
                            },
                        ),
                    )
                    .description("Font size in pixels."),
                )
                .item(
                    SettingItem::new(
                        "Terminal Line Height",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 0.8,
                                max: 3.0,
                                step: 0.1,
                            },
                            |cx| cx.global::<AppSettings>().terminal_line_height,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().terminal_line_height = value;
                            },
                        ),
                    )
                    .description("Line height as a multiplier on font size."),
                )
                .item(
                    SettingItem::new(
                        "Agent Font",
                        ui::font_picker::font_family_field(ui::font_picker::FontTarget::Agent),
                    )
                    .description("Font used by agent (chat) tabs."),
                )
                .item(
                    SettingItem::new(
                        "Agent Font Size",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 6.0,
                                max: 72.0,
                                step: 0.1,
                            },
                            |cx| cx.global::<AppSettings>().agent_font_size,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().agent_font_size = value;
                            },
                        ),
                    )
                    .description("Font size in pixels."),
                )
                .item(
                    SettingItem::new(
                        "Show monospace fonts only",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().monospace_only,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().monospace_only = value;
                            },
                        ),
                    )
                    .description("Filter the font list to fixed-width fonts."),
                ),
        )
        .group(
            SettingGroup::new().title("Tab Bar").item(
                SettingItem::new(
                    "Tab Width",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: DEFAULT_TAB_WIDTH,
                            max: MAX_TAB_WIDTH,
                            step: 1.0,
                        },
                        |cx| cx.global::<AppSettings>().tab_width,
                        |value, cx| {
                            cx.global_mut::<AppSettings>().tab_width = clamp_tab_width(value);
                        },
                    ),
                )
                .description("Fixed tab width in pixels; long titles are clipped."),
            ),
        )
        .group(
            SettingGroup::new()
                .title("Title Bar")
                .item(
                    SettingItem::new(
                        "Show daily token usage",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().show_daily_token_usage,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().show_daily_token_usage = value;
                            },
                        ),
                    )
                    .description(
                        "Show today's ccusage token totals in the titlebar, \
                             refreshed every 60 seconds (click to refresh now).",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Show Git Status on Title Bar",
                        SettingField::switch(
                            |cx| cx.global::<AppSettings>().show_git_status_on_title_bar,
                            |value, cx| {
                                cx.global_mut::<AppSettings>().show_git_status_on_title_bar = value;
                            },
                        ),
                    )
                    .description(
                        "Show the active repository's +added -removed line \
                             counts in the titlebar.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Git Status Refresh Interval",
                        SettingField::dropdown(
                            vec![
                                ("10".into(), "10s".into()),
                                ("15".into(), "15s".into()),
                                ("30".into(), "30s".into()),
                                ("60".into(), "60s".into()),
                            ],
                            |cx| {
                                cx.global::<AppSettings>()
                                    .git_status_refresh_interval
                                    .to_string()
                                    .into()
                            },
                            |value, cx| {
                                cx.global_mut::<AppSettings>().git_status_refresh_interval =
                                    clamp_git_interval(value.parse().unwrap_or(30));
                            },
                        )
                        .default_value(SharedString::from("30")),
                    )
                    .description("How often the git status is re-read."),
                ),
        )
}

fn system_page(shell_integration_mismatched: bool) -> SettingPage {
    SettingPage::new("System")
                    .default_open(true)
                    .group(
                        SettingGroup::new().title("Session").item(
                            SettingItem::new(
                                "Restore last session when opening",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().restore_last_session_when_opening,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>()
                                            .restore_last_session_when_opening = value;
                                    },
                                ),
                            )
                            .description("Reopen saved workspaces and tabs on startup."),
                        ),
                    )
                    .group(
                        SettingGroup::new().title("Workspace").item(
                            SettingItem::new(
                                "Confirm before closing",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().confirm_before_closing,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().confirm_before_closing = value;
                                    },
                                ),
                            )
                            .description(
                                "Ask for confirmation when closing a workspace, Agent tab, or window.",
                            ),
                        ),
                    )
                    .group(
                        SettingGroup::new()
                            .title("Process")
                            .item(
                                SettingItem::new(
                                    "Manage subprocess by Windows Job API",
                                    SettingField::switch(
                                        |cx| cx.global::<AppSettings>().manage_subprocess_job,
                                        |value, cx| {
                                            cx.global_mut::<AppSettings>().manage_subprocess_job =
                                                value;
                                        },
                                    ),
                                )
                                .description(
                                    "Closing a tab kills the shell's entire process tree. \
                             Applies to newly opened tabs.",
                                ),
                            )
                            .item(
                                SettingItem::new(
                                    "Warn before terminating shell",
                                    SettingField::dropdown(
                                        vec![
                                            ("disabled".into(), "Disabled".into()),
                                            (
                                                "when-child-processes-running".into(),
                                                "When child processes running".into(),
                                            ),
                                            ("always".into(), "Always".into()),
                                        ],
                                        |cx| {
                                            cx.global::<AppSettings>()
                                                .warn_before_terminating_shell
                                                .as_str()
                                                .into()
                                        },
                                        |value, cx| {
                                            cx.global_mut::<AppSettings>()
                                                .warn_before_terminating_shell =
                                                WarnBeforeTerminatingShell::from_value(&value);
                                        },
                                    )
                                    .default_value(SharedString::from(
                                        WarnBeforeTerminatingShell::WhenChildProcessesRunning.as_str(),
                                    )),
                                )
                                .description(
                                    "Choose when closing a shell asks for confirmation. Detecting \
                             child processes requires Job management.",
                                ),
                            ),
                    )
                    .group(
                        SettingGroup::new()
                            .title("Windows")
                            .item(
                                SettingItem::new(
                                    if shell_integration_mismatched {
                                        "Enable Windows Context Menu  ⚠"
                                    } else {
                                        "Enable Windows Context Menu"
                                    },
                                    SettingField::switch(
                                        |_| is_shell_integration_registered(),
                                        |value, _| {
                                            let result = if value {
                                                register_shell_integration()
                                            } else {
                                                unregister_shell_integration()
                                            };

                                            if let Err(err) = result {
                                                warn!(
                                                    "failed to toggle Windows context menu: {err:#}"
                                                );
                                            }
                                        },
                                    ),
                                )
                                .description(if shell_integration_mismatched {
                                    "The registered shell extension does not point to the DLL beside the current NiumaTerm executable."
                                } else {
                                    "Add NiumaTerm actions to File Explorer directory menus."
                                }),
                            )
                            .item(
                                SettingItem::new(
                                    "Enable System Notification",
                                    SettingField::switch(
                                        |_| system_notification_enabled(),
                                        |value, _| {
                                            if let Err(err) =
                                                set_system_notification_enabled(value)
                                            {
                                                warn!(
                                                    "failed to toggle system notifications: {err:#}"
                                                );
                                            }
                                        },
                                    ),
                                )
                                .description(
                                    "Show Windows notifications for terminal and agent events.",
                                ),
                            ),
                    )
                    .group(
                        SettingGroup::new().title("Performance").item(
                            SettingItem::new(
                                "Prioritize UI threads",
                                SettingField::switch(
                                    |cx| cx.global::<AppSettings>().prioritize_ui_threads,
                                    |value, cx| {
                                        cx.global_mut::<AppSettings>().prioritize_ui_threads = value;

                                        cx.global::<PlatformHandle>()
                                            .0
                                            .set_ui_thread_priority(value);
                                    },
                                ),
                            )
                            .description("Raise the main and render thread priority to AboveNormal."),
                        ),
                    )
}

pub fn settings_view(cx: &App) -> Settings {
    let profiles = cx.global::<AppSettings>().profiles.clone();
    let agent_profiles = cx.global::<AppSettings>().agent_profiles.clone();
    let transparency_enabled = cx.global::<AppSettings>().window_transparency_enabled;
    let background_image_enabled = cx.global::<AppSettings>().background_image.is_some();
    let shell_integration_mismatched = shell_integration_dll_mismatched();

    let sidebar_style = StyleRefinement::default()
        .bg(cx.theme().sidebar)
        .border_t_1()
        .border_b_1()
        .border_l_1()
        .border_color(cx.theme().sidebar_border)
        .rounded(UI_RADIUS)
        .overflow_hidden();

    Settings::new("app-settings")
        .sidebar_width(px(240.0))
        .sidebar_style(&sidebar_style)
        // Each subcategory is its own page; the alternative scrolls the
        // whole category top to bottom.
        .single_group_pages(true)
        .page(terminal_page())
        .page(appearance_page(
            transparency_enabled,
            background_image_enabled,
        ))
        .page(profiles_page(&profiles, &agent_profiles))
        .page(agent_page(&agent_profiles, cx))
        .page(system_page(shell_integration_mismatched))
        .page(remote_session_page())
        .page(about_page())
}

/// The Profiles page: exactly two groups — Terminal Profile and Agent
/// Profile — so the sidebar shows two stable entries. Profile cards render
/// inside each group instead of as their own groups, which would otherwise
/// add one sidebar entry per profile under `single_group_pages`.
fn profiles_page(profiles: &[Profile], agent_profiles: &[AgentProfile]) -> SettingPage {
    SettingPage::new("Profiles")
        .default_open(true)
        .group(terminal_profiles_group(profiles))
        .group(agent_profiles_group(agent_profiles))
}

/// One labeled row inside a profile card: title and muted description on the
/// left, the control on the right (mirrors `SettingItem`'s horizontal
/// layout so cards read like regular setting rows).
fn card_row(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl gpui::IntoElement,
    cx: &App,
) -> Div {
    h_flex()
        .w_full()
        .justify_between()
        .items_start()
        .gap_3()
        .child(
            v_flex()
                .flex_1()
                .max_w_3_5()
                .gap_1()
                .child(Label::new(title.into()).text_sm())
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(description.into()),
                ),
        )
        .child(control.into_any_element())
}

fn terminal_profiles_group(profiles: &[Profile]) -> SettingGroup {
    // Selector options come from the current names; the settings view is
    // rebuilt per render, so renames refresh the list immediately.
    let options: Vec<(SharedString, SharedString)> = profiles
        .iter()
        .enumerate()
        .map(|(ix, p)| {
            let label = if p.name.is_empty() {
                format!("Profile {}", ix + 1)
            } else {
                p.name.clone()
            };

            (
                SharedString::from(p.name.clone()),
                SharedString::from(label),
            )
        })
        .collect();

    let mut group = SettingGroup::new()
        .title("Terminal Profile")
        .description("Shell profiles available to terminals.")
        .item(
            SettingItem::new(
                "Default Profile",
                SettingField::dropdown(
                    options,
                    |cx| cx.global::<AppSettings>().default_profile.clone().into(),
                    |value, cx| {
                        cx.global_mut::<AppSettings>().default_profile = value.to_string();
                    },
                ),
            )
            .description("Profile used by new terminals."),
        )
        .item(
            SettingItem::new(
                "Add Profile",
                SettingField::render(|_, _, _| {
                    Button::new("profile-add").outline().label("Add").on_click(
                        |_, _, cx: &mut App| {
                            cx.global_mut::<AppSettings>().add_profile();
                        },
                    )
                }),
            )
            .description("Create a new profile."),
        );

    let count = profiles.len();
    for ix in 0..count {
        group = group.item(terminal_profile_card(ix, count));
    }
    group
}

fn terminal_profile_card(ix: usize, count: usize) -> SettingItem {
    SettingItem::render(move |options, window, cx| {
        // get(ix): the render closure outlives profile removal, so a stale
        // index must read as empty, not panic.
        let profile = cx
            .global::<AppSettings>()
            .profiles
            .get(ix)
            .cloned()
            .unwrap_or_default();

        let title = if profile.name.is_empty() {
            format!("Profile {}", ix + 1)
        } else {
            profile.name.clone()
        };

        let disabled = options.disabled;
        let size = options.size;

        let name_input = card_text_input(
            format!("terminal-profile-name-{ix}"),
            profile.name.clone().into(),
            false,
            move |value, cx| cx.global_mut::<AppSettings>().rename_profile(ix, value),
            window,
            cx,
        );

        let shell_input = card_text_input(
            format!("terminal-profile-shell-{ix}"),
            profile.shell.clone().into(),
            false,
            move |value, cx| {
                if let Some(profile) = cx.global_mut::<AppSettings>().profiles.get_mut(ix) {
                    profile.shell = value;
                }
            },
            window,
            cx,
        );

        let args_input = card_text_input(
            format!("terminal-profile-args-{ix}"),
            profile.args.clone().into(),
            false,
            move |value, cx| {
                if let Some(profile) = cx.global_mut::<AppSettings>().profiles.get_mut(ix) {
                    profile.args = value;
                }
            },
            window,
            cx,
        );

        let browse_input = shell_input.clone();
        let shell_control = v_flex()
            .gap_2()
            .w_64()
            .child(
                Input::new(&shell_input)
                    .disabled(disabled)
                    .with_size(size)
                    .w_full(),
            )
            .child(
                h_flex().w_full().justify_end().child(
                    Button::new(("profile-shell-browse", ix))
                        .outline()
                        .label("Browse")
                        .disabled(disabled)
                        .w(relative(1. / 3.))
                        .on_click(move |_, window, cx| {
                            let rx = cx.prompt_for_paths(PathPromptOptions {
                                files: true,
                                directories: false,
                                multiple: false,
                                prompt: Some("Select shell executable".into()),
                                file_types: vec![FileDialogFilter {
                                    name: "Executables".into(),
                                    extensions: vec!["exe".into()],
                                }],
                            });

                            let input = browse_input.clone();

                            window
                                .spawn(cx, async move |cx| {
                                    if let Ok(Ok(Some(paths))) = rx.await
                                        && let Some(path) = paths.first()
                                    {
                                        let value = path.display().to_string();

                                        let _ =
                                            cx.update_global(|settings: &mut AppSettings, _, _| {
                                                if let Some(profile) = settings.profiles.get_mut(ix)
                                                {
                                                    profile.shell = value.clone();
                                                }
                                            });

                                        let _ = input.update_in(cx, |input, window, cx| {
                                            input.set_value(value, window, cx);
                                        });
                                    }
                                })
                                .detach();
                        }),
                ),
            );

        let remove_button = Button::new(("profile-remove", ix))
            .danger()
            .label("Remove")
            .disabled(disabled || count <= 1)
            .on_click(move |_, window, cx: &mut App| {
                let name = cx
                    .global::<AppSettings>()
                    .profiles
                    .get(ix)
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default();
                let subject = if name.is_empty() {
                    "this profile".to_string()
                } else {
                    format!("profile \"{name}\"")
                };

                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .confirm()
                        .title("Remove Profile")
                        .description(format!("Remove {subject}? This cannot be undone."))
                        .on_ok(move |_, _, cx| {
                            cx.global_mut::<AppSettings>().remove_profile(ix);
                            true
                        })
                });
            });

        GroupBox::new().outline().title(title).child(
            v_flex()
                .w_full()
                .gap_4()
                .child(card_row(
                    "Name",
                    "Display name; the card title and default selector follow it.",
                    Input::new(&name_input)
                        .disabled(disabled)
                        .with_size(size)
                        .w_64(),
                    cx,
                ))
                .child(card_row(
                    "Shell Path",
                    "Path to the shell executable.",
                    shell_control,
                    cx,
                ))
                .child(card_row(
                    "Arguments",
                    "Command-line arguments, space-separated.",
                    Input::new(&args_input)
                        .disabled(disabled)
                        .with_size(size)
                        .w_64(),
                    cx,
                ))
                .child(card_row(
                    "Remove Profile",
                    if count <= 1 {
                        "The last profile cannot be removed."
                    } else {
                        "Removing the default falls back to the first profile."
                    },
                    remove_button,
                    cx,
                )),
        )
    })
}

fn agent_profiles_group(agent_profiles: &[AgentProfile]) -> SettingGroup {
    let options: Vec<(SharedString, SharedString)> = agent_profiles
        .iter()
        .enumerate()
        .map(|(ix, p)| {
            let label = if p.name.is_empty() {
                format!("Agent Profile {}", ix + 1)
            } else {
                p.name.clone()
            };

            (
                SharedString::from(p.name.clone()),
                SharedString::from(label),
            )
        })
        .collect();

    let mut group = SettingGroup::new()
        .title("Agent Profile")
        .description("Launch profiles for agent tabs (Claude Code and Codex).")
        .item(
            SettingItem::new(
                "Default Profile",
                SettingField::dropdown(
                    options,
                    |cx| {
                        cx.global::<AppSettings>()
                            .default_agent_profile
                            .clone()
                            .into()
                    },
                    |value, cx| {
                        cx.global_mut::<AppSettings>().default_agent_profile = value.to_string();
                    },
                ),
            )
            .description("Profile used by new agent tabs."),
        )
        .item(
            SettingItem::new(
                "Add Profile",
                SettingField::render(|_, _, _| {
                    Button::new("agent-profile-add")
                        .outline()
                        .label("Add")
                        .on_click(|_, window, cx: &mut App| {
                            open_agent_profile_dialog(None, window, cx);
                        })
                }),
            )
            .description("Create a new agent profile."),
        );

    for (ix, profile) in agent_profiles.iter().enumerate() {
        let label = if profile.name.is_empty() {
            format!("Agent Profile {}", ix + 1)
        } else {
            profile.name.clone()
        };

        group = group.item(
            SettingItem::new(
                label,
                SettingField::render(move |_, _, _| {
                    Button::new(("agent-profile-edit", ix))
                        .outline()
                        .label("Edit")
                        .on_click(move |_, window, cx: &mut App| {
                            open_agent_profile_dialog(Some(ix), window, cx);
                        })
                }),
            )
            .description(agent_kind_label(profile.kind)),
        );
    }
    group
}

fn installation_update_title(
    provider: ProviderKind,
    provider_ordinal: usize,
    provider_total: usize,
) -> String {
    if provider_total > 1 {
        format!("{} Updates {provider_ordinal}", provider.display())
    } else {
        format!("{} Updates", provider.display())
    }
}

fn installation_version_text(phase: UpdatePhase, current: &str, available: &str) -> String {
    if phase == UpdatePhase::Unknown {
        "Not checked".to_string()
    } else {
        format!("{current} → {available}")
    }
}

fn agent_update_check_item() -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let profiles = cx.global::<AppSettings>().agent_profiles.clone();
        let installations = agent_updates::installations_for_profiles(&profiles, cx);
        let busy = installations.iter().any(|snapshot| snapshot.busy);
        let check_profiles = profiles.clone();
        let check = Button::new("agent-updates-check-all")
            .outline()
            .label(if busy { "Working…" } else { "Check" })
            .disabled(options.disabled || busy || installations.is_empty())
            .on_click(move |_, _, cx| {
                agent_updates::manual_check_profiles(&check_profiles, cx);
            });

        card_row(
            "Check for Updates",
            "Check each distinct Claude Code and Codex installation referenced by Agent Profiles.",
            check,
            cx,
        )
        .into_any_element()
    })
}

fn agent_update_status_item(ix: usize, title: String, key: InstallationKey) -> SettingItem {
    SettingItem::render(move |options, _window, cx| {
        let snapshot = agent_updates::installation(&key, cx);
        let (detail, busy, can_update) = snapshot.map_or_else(
            || ("Update status unavailable".to_string(), false, false),
            |snapshot| {
                let versions = snapshot.state.versions.as_ref();
                let current = versions
                    .and_then(|status| status.current.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string());
                let available = versions
                    .and_then(|status| status.available.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string());
                let labels = versions
                    .map(|status| {
                        [status.install_method.as_deref(), status.channel.as_deref()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ")
                    })
                    .filter(|labels| !labels.is_empty())
                    .map(|labels| format!(" · {labels}"))
                    .unwrap_or_default();
                let checked = snapshot
                    .last_checked
                    .map(|time| format!(" · checked {}", time.format("%Y-%m-%d %H:%M")))
                    .unwrap_or_default();
                let diagnostic = snapshot
                    .state
                    .error
                    .as_ref()
                    .map(|error| error.message())
                    .or_else(|| {
                        versions.and_then(|status| match &status.support {
                            DiscoverySupport::Supported => None,
                            DiscoverySupport::Unsupported { reason } => Some(reason.as_str()),
                        })
                    })
                    .map(|message| format!(" · {}", message.chars().take(256).collect::<String>()))
                    .unwrap_or_default();
                let phase = match snapshot.state.phase {
                    UpdatePhase::Unknown => "not checked",
                    UpdatePhase::Checking => "checking",
                    UpdatePhase::Current => "current",
                    UpdatePhase::Available => "update available",
                    UpdatePhase::WaitingForIdle => "waiting for idle",
                    UpdatePhase::Suspending => "stopping agents",
                    UpdatePhase::Updating => "updating",
                    UpdatePhase::Verifying => "verifying",
                    UpdatePhase::Restoring => "restoring tabs",
                    UpdatePhase::Updated => "updated",
                    UpdatePhase::Unchanged => "version unchanged",
                    UpdatePhase::Unsupported => "automatic discovery unsupported",
                    UpdatePhase::Failed => "failed",
                };
                let can_update =
                    versions.is_some_and(|status| status.can_update && status.update_available());
                let version = installation_version_text(snapshot.state.phase, &current, &available);
                let detail = if snapshot.state.phase == UpdatePhase::Unknown {
                    version
                } else {
                    format!("{version} · {phase}{labels}{checked}{diagnostic}")
                };
                (detail, snapshot.busy, can_update)
            },
        );

        let update_key = key.clone();
        let update = Button::new(("agent-update-install", ix))
            .primary()
            .label("Update")
            .disabled(options.disabled || busy || !can_update)
            .on_click(move |_, window, cx| {
                agent_updates::request_update(update_key.clone(), window, cx);
            });

        card_row(title.clone(), detail, update, cx).into_any_element()
    })
}

/// Open the add/edit dialog for an agent profile. `target` is the index in
/// `AppSettings::agent_profiles` for edit mode, `None` for a new profile.
/// The dialog edits an [`AgentProfileDraft`]; Save commits, Cancel discards.
fn open_agent_profile_dialog(target: Option<usize>, window: &mut Window, cx: &mut App) {
    let profile = match target {
        Some(ix) => cx
            .global::<AppSettings>()
            .agent_profiles
            .get(ix)
            .cloned()
            .unwrap_or_default(),
        // A new profile starts from the Claude Code built-in with a blank
        // name; Save fills in a unique placeholder.
        None => AgentProfile {
            name: String::new(),
            ..builtin_agent_profile(AgentProfileKind::ClaudeCode)
        },
    };
    cx.set_global(AgentProfileDraft { target, profile });

    window.open_dialog(cx, move |dialog, window, _| {
        let title = if target.is_some() {
            "Edit Agent Profile"
        } else {
            "Add Agent Profile"
        };
        let settings_height = window.viewport_size().height;
        let dialog_height = settings_height * 0.6;
        let dialog_top = (settings_height - dialog_height) * 0.5;

        let mut footer = DialogFooter::new()
            .child(DialogClose::new().child(Button::new("agent-profile-cancel").label("Cancel")));

        if let Some(ix) = target {
            footer = footer.child(
                Button::new("agent-profile-delete")
                    .danger()
                    .label("Delete")
                    .on_click(move |_, window, cx: &mut App| {
                        let name = cx.global::<AgentProfileDraft>().profile.name.clone();
                        let subject = if name.is_empty() {
                            "this profile".to_string()
                        } else {
                            format!("profile \"{name}\"")
                        };

                        window.open_alert_dialog(cx, move |alert, _, _| {
                            alert
                                .confirm()
                                .title("Delete Agent Profile")
                                .description(format!("Delete {subject}? This cannot be undone."))
                                .on_ok(move |_, window, cx| {
                                    cx.global_mut::<AppSettings>().remove_agent_profile(ix);
                                    // Pop the confirm and the edit dialog
                                    // explicitly, then return false so the
                                    // alert's own close path does not pop a
                                    // third dialog (the settings one).
                                    window.close_dialog(cx);
                                    window.close_dialog(cx);
                                    false
                                })
                        });
                    }),
            );
        }

        footer = footer.child(
            Button::new("agent-profile-save")
                .primary()
                .label("Save")
                .on_click(|_, window, cx: &mut App| {
                    save_agent_profile_draft(cx);
                    window.close_dialog(cx);
                }),
        );

        dialog
            .title(title)
            .overlay_closable(false)
            .margin_top(dialog_top)
            .w(px(560.))
            .h(dialog_height)
            .content(|content, window, cx| {
                content.overflow_hidden().child(
                    div().flex_1().overflow_hidden().child(
                        v_flex()
                            .size_full()
                            .overflow_y_scrollbar()
                            .child(div().pr_2().child(agent_profile_dialog_content(window, cx))),
                    ),
                )
            })
            .footer(footer)
    });
}

/// Commit the dialog draft into `AppSettings`: dedupe the name, then update
/// the edited entry or append a new one.
fn save_agent_profile_draft(cx: &mut App) {
    let target = cx.global::<AgentProfileDraft>().target;
    let mut profile = cx.global::<AgentProfileDraft>().profile.clone();

    let settings = cx.global_mut::<AppSettings>();
    profile.name = settings.unique_agent_profile_name(&profile.name, profile.kind, target);

    match target {
        Some(ix) => settings.update_agent_profile(ix, profile),
        None => {
            settings.agent_profiles.push(profile);

            // Adding to a previously empty list makes the new profile the
            // default, so NewAgentTab immediately uses it.
            if settings.default_agent_profile.is_empty() {
                settings.default_agent_profile = settings
                    .agent_profiles
                    .last()
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
            }
        }
    }
}

/// One of the two Base Agent choice buttons in the add dialog; the selected
/// kind renders as the primary variant.
fn kind_choice_button(
    id: &'static str,
    kind: AgentProfileKind,
    current: AgentProfileKind,
) -> Button {
    let button = Button::new(id).label(agent_kind_label(kind));
    let button = if kind == current {
        button.primary()
    } else {
        button.outline()
    };

    button.on_click(move |_, _, cx: &mut App| {
        let draft = cx.global_mut::<AgentProfileDraft>();
        if draft.profile.kind == kind {
            return;
        }

        // The executable follows the kind while it still holds a built-in
        // default; a hand-typed path survives the switch.
        let executable = draft.profile.executable.trim();
        if executable.is_empty() || executable == "claude" || executable == "codex" {
            draft.profile.executable = builtin_agent_profile(kind).executable;
        }
        draft.profile.kind = kind;
    })
}

fn agent_profile_dialog_content(window: &mut Window, cx: &mut App) -> Div {
    let profile = cx.global::<AgentProfileDraft>().profile.clone();
    let is_edit = cx.global::<AgentProfileDraft>().target.is_some();

    let kind_label = agent_kind_label(profile.kind);
    let key_env = match profile.kind {
        AgentProfileKind::ClaudeCode => "ANTHROPIC_API_KEY",
        AgentProfileKind::Codex => "OPENAI_API_KEY",
    };
    let endpoint_on = profile.use_custom_endpoint;

    let name_input = card_text_input(
        "agent-profile-dialog-name".to_string(),
        profile.name.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.name = value,
        window,
        cx,
    );

    let exe_input = card_text_input(
        "agent-profile-dialog-exe".to_string(),
        profile.executable.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.executable = value,
        window,
        cx,
    );

    let model_input = card_text_input(
        "agent-profile-dialog-model".to_string(),
        profile.model.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.model = value,
        window,
        cx,
    );

    let url_input = card_text_input(
        "agent-profile-dialog-url".to_string(),
        profile.api_base_url.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.api_base_url = value,
        window,
        cx,
    );

    let key_input = card_text_input(
        "agent-profile-dialog-key".to_string(),
        profile.api_key.clone().into(),
        false,
        |value, cx| cx.global_mut::<AgentProfileDraft>().profile.api_key = value,
        window,
        cx,
    );

    let kind_control: AnyElement = if is_edit {
        // The kind decides the backend protocol; changing it under an existing
        // profile would silently repurpose tabs and persisted state, so it
        // is fixed after creation.
        Label::new(kind_label).text_sm().into_any_element()
    } else {
        h_flex()
            .gap_2()
            .child(kind_choice_button(
                "agent-profile-kind-claude",
                AgentProfileKind::ClaudeCode,
                profile.kind,
            ))
            .child(kind_choice_button(
                "agent-profile-kind-codex",
                AgentProfileKind::Codex,
                profile.kind,
            ))
            .into_any_element()
    };

    let endpoint_switch = Switch::new("agent-profile-dialog-endpoint")
        .checked(endpoint_on)
        .on_click(|checked: &bool, _, cx: &mut App| {
            cx.global_mut::<AgentProfileDraft>()
                .profile
                .use_custom_endpoint = *checked;
        });

    let mut env_rows = v_flex().w_full().gap_2();
    for (row, var) in profile.env.iter().enumerate() {
        let env_name_input = card_text_input(
            format!("agent-profile-dialog-env-{row}-name"),
            var.name.clone().into(),
            false,
            move |value, cx| {
                if let Some(var) = cx
                    .global_mut::<AgentProfileDraft>()
                    .profile
                    .env
                    .get_mut(row)
                {
                    var.name = value;
                }
            },
            window,
            cx,
        );

        let env_value_input = card_text_input(
            format!("agent-profile-dialog-env-{row}-value"),
            var.value.clone().into(),
            false,
            move |value, cx| {
                if let Some(var) = cx
                    .global_mut::<AgentProfileDraft>()
                    .profile
                    .env
                    .get_mut(row)
                {
                    var.value = value;
                }
            },
            window,
            cx,
        );

        env_rows = env_rows.child(
            h_flex()
                .w_full()
                .gap_2()
                .child(Input::new(&env_name_input).flex_1())
                .child(Input::new(&env_value_input).flex_1())
                .child(
                    Button::new(SharedString::from(format!(
                        "agent-profile-dialog-env-remove-{row}"
                    )))
                    .outline()
                    .label("Remove")
                    .on_click(move |_, _, cx: &mut App| {
                        let env = &mut cx.global_mut::<AgentProfileDraft>().profile.env;
                        if row < env.len() {
                            env.remove(row);
                        }
                    }),
                ),
        );
    }

    let env_section = v_flex()
        .w_full()
        .gap_2()
        .child(Label::new("Environment Variables").text_sm())
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Extra environment variables applied to the agent process."),
        )
        .child(env_rows)
        .child(
            h_flex().child(
                Button::new("agent-profile-dialog-env-add")
                    .outline()
                    .label("Add Variable")
                    .on_click(|_, _, cx: &mut App| {
                        cx.global_mut::<AgentProfileDraft>()
                            .profile
                            .env
                            .push(EnvVar::default());
                    }),
            ),
        );

    v_flex()
        .w_full()
        .gap_4()
        .child(card_row(
            "Name",
            "Display name; it keys the default selector and per-profile settings.",
            Input::new(&name_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Base Agent",
            "Which agent CLI this profile launches.",
            kind_control,
            cx,
        ))
        .child(card_row(
            "Executable Path",
            "Executable name or full path; a bare name resolves via PATH.",
            Input::new(&exe_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Model",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    "Initial model; passed to Claude Code as ANTHROPIC_MODEL."
                }
                AgentProfileKind::Codex => {
                    "Initial model; passed to Codex when its app-server thread starts."
                }
            },
            Input::new(&model_input).w_64(),
            cx,
        ))
        .child(card_row(
            "Use Custom API Endpoint",
            "Route this agent through your own API endpoint.",
            endpoint_switch,
            cx,
        ))
        .child(card_row(
            "API URL",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    "Exported as ANTHROPIC_BASE_URL while the custom endpoint is enabled."
                        .to_string()
                }
                AgentProfileKind::Codex => {
                    "Injected as a profile-scoped Codex model provider base URL.".to_string()
                }
            },
            Input::new(&url_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(card_row(
            "API Key",
            match profile.kind {
                AgentProfileKind::ClaudeCode => {
                    format!("Exported as {key_env} while the custom endpoint is enabled.")
                }
                AgentProfileKind::Codex => {
                    format!("Exported as {key_env} and referenced by the profile-scoped provider.")
                }
            },
            Input::new(&key_input).disabled(!endpoint_on).w_64(),
            cx,
        ))
        .child(env_section)
}

#[cfg(test)]
mod tests;
