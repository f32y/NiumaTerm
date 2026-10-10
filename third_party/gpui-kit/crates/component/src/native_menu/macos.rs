//! macOS native menu implementation (AppKit `NSMenu` via objc2).

use std::{cell::Cell, sync::Arc};

use gpui::{App, AssetSource, KeybindingKeystroke, Pixels, Point, SharedString, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{NSEventModifierFlags, NSImage, NSMenu, NSMenuItem, NSView};
use objc2_foundation::{NSData, NSPoint, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::native_menu::{NativeMenuActivation, NativeMenuItem, resolve_icon_image};

/// Side length (in points) menu item images are scaled to. AppKit does not resize the image to fit
/// the row, so a large file would otherwise overflow it.
const MENU_IMAGE_SIZE: f64 = 16.0;

/// Ivars for [`MenuTarget`]: the tag of the selected item, or `-1` if none.
struct MenuTargetIvars {
    selected: Cell<isize>,
}

define_class!(
    // A throwaway Objective-C object that receives the menu item action and
    // records which item (by tag) was clicked.
    #[unsafe(super(NSObject))]
    #[name = "GPUIComponentNativeMenuTarget"]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(menuItemClicked:))]
        fn menu_item_clicked(&self, sender: &NSMenuItem) {
            self.ivars().selected.set(sender.tag());
        }
    }
);

impl MenuTarget {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(MenuTargetIvars {
            selected: Cell::new(-1),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// Show a native popup menu and carry out the selected item's activation.
///
/// The AppKit tracking loop is run from a foreground task so that GPUI is not
/// borrowed while the menu is open.
pub(super) fn show(
    items: Vec<NativeMenuItem>,
    asset_source: Arc<dyn AssetSource>,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(view_ptr) = ns_view_ptr(window) else {
        return;
    };
    // Inherent `Window::window_handle` (GPUI's `AnyWindowHandle`), not the
    // `raw_window_handle::HasWindowHandle` trait method in scope below.
    let handle = Window::window_handle(window);
    let mut keystrokes = Vec::new();
    collect_action_keystrokes(&items, window, &mut keystrokes);

    cx.spawn(async move |cx| {
        let activation = run_menu(
            view_ptr,
            &items,
            asset_source.as_ref(),
            position,
            &keystrokes,
        );
        let _ = cx.update(move |app| {
            let _ = handle.update(app, move |_, window, app| {
                if let Some(activation) = activation {
                    activation.perform(window, app);
                }
                // Wake GPUI after the AppKit tracking loop returns so the window
                // resumes painting and re-registers its mouse handlers. Without
                // this, a dismissed menu (especially when nothing is selected)
                // leaves the window idle and unresponsive to a second
                // right-click.
                window.refresh();
            });
        });
    })
    .detach();
}

fn collect_action_keystrokes(
    items: &[NativeMenuItem],
    window: &Window,
    keystrokes: &mut Vec<Option<KeybindingKeystroke>>,
) {
    for item in items {
        match item {
            NativeMenuItem::Item {
                disabled: false,
                activation: Some(activation),
                ..
            } => {
                let keystroke = match activation {
                    NativeMenuActivation::Action(action) => window
                        .highest_precedence_binding_for_action(action.as_ref())
                        .and_then(|binding| match binding.keystrokes() {
                            [stroke] if stroke.key().chars().count() == 1 => Some(stroke.clone()),
                            _ => None,
                        }),
                    NativeMenuActivation::Handler(_) => None,
                };
                keystrokes.push(keystroke);
            }
            NativeMenuItem::Submenu { items, .. } => {
                collect_action_keystrokes(items, window, keystrokes);
            }
            _ => {}
        }
    }
}

/// Build the menu (recursively, including submenus), show it, and return what
/// the selected item does.
fn run_menu(
    view_ptr: usize,
    items: &[NativeMenuItem],
    asset_source: &dyn AssetSource,
    position: Point<Pixels>,
    keystrokes: &[Option<KeybindingKeystroke>],
) -> Option<NativeMenuActivation> {
    let mtm = MainThreadMarker::new()?;
    // SAFETY: `view_ptr` came from the window's AppKit handle, and the window
    // outlives this synchronous call.
    let view: &NSView = unsafe { &*(view_ptr as *const NSView) };

    let target = MenuTarget::new();
    let mut activations: Vec<&NativeMenuActivation> = Vec::new();
    let ns_menu = build_menu(
        items,
        asset_source,
        &target,
        mtm,
        &mut activations,
        keystrokes,
    );

    // `position` is window-relative, logical pixels, origin top-left (GPUI).
    // AppKit view coordinates have their origin at the bottom-left, so flip y.
    let height = view.bounds().size.height;
    let location = NSPoint::new(
        f32::from(position.x) as f64,
        height - f32::from(position.y) as f64,
    );
    ns_menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(view));

    let tag = target.ivars().selected.get();
    if tag >= 0 {
        activations
            .get(tag as usize)
            .map(|activation| (*activation).clone())
    } else {
        None
    }
}

/// Recursively build an `NSMenu`. Each actionable leaf item is given a tag equal
/// to its index in `activations`, so the selected tag maps back to what the item
/// does.
fn build_menu<'a>(
    items: &'a [NativeMenuItem],
    asset_source: &dyn AssetSource,
    target: &MenuTarget,
    mtm: MainThreadMarker,
    activations: &mut Vec<&'a NativeMenuActivation>,
    keystrokes: &[Option<KeybindingKeystroke>],
) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    // Items are configured explicitly, so disable AppKit's automatic enabling.
    menu.setAutoenablesItems(false);

    for item in items {
        match item {
            NativeMenuItem::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            NativeMenuItem::Item {
                label,
                disabled,
                checked,
                icon,
                activation,
            } => {
                let ns_item = NSMenuItem::new(mtm);
                unsafe {
                    ns_item.setTitle(&NSString::from_str(label));
                    ns_item.setEnabled(!*disabled);
                    if let Some(icon) = icon {
                        if let Some(ns_image) = ns_image_for_icon(icon.path_ref(), asset_source) {
                            ns_image.setSize(NSSize::new(MENU_IMAGE_SIZE, MENU_IMAGE_SIZE));
                            ns_image.setTemplate(true);
                            ns_item.setImage(Some(&ns_image));
                        }
                    }

                    if *checked {
                        // `NSControlStateValueOn`
                        ns_item.setState(1);
                    }
                    if let Some(activation) = activation {
                        if !*disabled {
                            // AppKit processes shortcuts while tracking a popup,
                            // before GPUI can dispatch them to the owner window.
                            if let Some(Some(stroke)) = keystrokes.get(activations.len()) {
                                ns_item.setKeyEquivalent(&NSString::from_str(stroke.key()));
                                let modifiers = stroke.modifiers();
                                let mut mask = NSEventModifierFlags::empty();
                                for (enabled, flag) in [
                                    (modifiers.platform, NSEventModifierFlags::Command),
                                    (modifiers.control, NSEventModifierFlags::Control),
                                    (modifiers.alt, NSEventModifierFlags::Option),
                                    (modifiers.shift, NSEventModifierFlags::Shift),
                                ] {
                                    if enabled {
                                        mask |= flag;
                                    }
                                }
                                ns_item.setKeyEquivalentModifierMask(mask);
                            }
                            ns_item.setTag(activations.len() as isize);
                            activations.push(activation);
                            ns_item.setTarget(Some(target as &AnyObject));
                            ns_item.setAction(Some(sel!(menuItemClicked:)));
                        }
                    }
                }
                menu.addItem(&ns_item);
            }
            NativeMenuItem::Submenu {
                label,
                disabled,
                items,
            } => {
                let ns_item = NSMenuItem::new(mtm);
                let submenu = build_menu(items, asset_source, target, mtm, activations, keystrokes);
                ns_item.setTitle(&NSString::from_str(label));
                ns_item.setEnabled(!*disabled);
                ns_item.setSubmenu(Some(&submenu));
                menu.addItem(&ns_item);
            }
        }
    }

    menu
}

fn ns_image_for_icon(
    path: &SharedString,
    asset_source: &dyn AssetSource,
) -> Option<Retained<NSImage>> {
    let image = resolve_icon_image(path, asset_source)?;
    if image.bytes.is_empty() {
        return None;
    }

    let data = unsafe {
        NSData::dataWithBytes_length(image.bytes.as_ptr().cast(), image.bytes.len() as _)
    };
    NSImage::initWithData(NSImage::alloc(), &data)
}

/// Extract the AppKit `NSView` pointer from the window's raw handle.
fn ns_view_ptr(window: &Window) -> Option<usize> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.ns_view.as_ptr() as usize)
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, FocusHandle, InteractiveElement as _, IntoElement, KeyBinding, Render,
        TestAppContext, Window, div,
    };

    use crate::input::Copy;
    use crate::native_menu::NativeMenu;
    use crate::native_menu::macos::collect_action_keystrokes;

    struct ShortcutTestView {
        focus: FocusHandle,
    }

    impl Render for ShortcutTestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().key_context("PopupTest").track_focus(&self.focus)
        }
    }

    #[gpui::test]
    fn popup_copy_shortcut_matches_its_activation_after_handlers_and_submenus(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.bind_keys([KeyBinding::new("cmd-c", Copy, Some("PopupTest"))]));
        let (_, cx) = cx.add_window_view(|window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            ShortcutTestView { focus }
        });
        cx.run_until_parked();
        let menu = NativeMenu::new()
            .item("Handler", |_, _| {})
            .menu_with_disabled("Disabled copy", true, Box::new(Copy))
            .separator()
            .submenu("Actions", NativeMenu::new().menu("Copy", Box::new(Copy)));

        cx.update(|window, _| {
            let mut strokes = Vec::new();
            collect_action_keystrokes(&menu.items, window, &mut strokes);
            assert_eq!(strokes.len(), 2);
            assert!(strokes[0].is_none());
            let copy = strokes[1].as_ref().expect("copy popup shortcut");
            assert_eq!(copy.key(), "c");
            assert!(copy.modifiers().platform);
        });
    }
}
