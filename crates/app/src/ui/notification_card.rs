use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{Anchor, App, IntoElement as _, ParentElement as _, SharedString, Styled as _, Window};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::{Notification, NotificationType};
use gpui_component::progress::Progress;
use gpui_component::{Icon, v_flex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum NotificationTone {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum NotificationProgress {
    #[default]
    None,
    Indeterminate,
    Determinate(f32),
}

type NotificationCallback = dyn Fn(&mut Window, &mut App);

pub(crate) struct NotificationAction {
    label: SharedString,
    on_click: Rc<NotificationCallback>,
}

impl NotificationAction {
    pub(crate) fn new(
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            on_click: Rc::new(on_click),
        }
    }

    fn button(&self, id: &'static str) -> Button {
        let on_click = self.on_click.clone();

        Button::new(id)
            .label(self.label.clone())
            .on_click(move |_, window, cx| on_click(window, cx))
    }
}

/// Persistent card presentation shared by application and provider updates.
/// Owners retain the entity and decide when to replace, hide, or dismiss it.
#[derive(Default)]
pub(crate) struct NotificationCard {
    pub(crate) title: SharedString,
    pub(crate) message: SharedString,
    pub(crate) tone: NotificationTone,
    pub(crate) icon: Option<Icon>,
    pub(crate) progress: NotificationProgress,
    pub(crate) primary: Option<NotificationAction>,
    pub(crate) secondary: Option<NotificationAction>,
}

impl NotificationCard {
    pub(crate) fn build(self) -> Notification {
        let tone = match self.tone {
            NotificationTone::Info => NotificationType::Info,
            NotificationTone::Success => NotificationType::Success,
            NotificationTone::Warning => NotificationType::Warning,
            NotificationTone::Error => NotificationType::Error,
        };

        let mut notification = Notification::new()
            .placement(Anchor::TopRight)
            .autohide(false)
            .with_type(tone)
            .title(self.title)
            .message(self.message)
            .content(move |_, _, _| {
                let progress = match self.progress {
                    NotificationProgress::None => None,
                    NotificationProgress::Indeterminate => {
                        Some(Progress::new("notification-progress").loading(true))
                    }
                    NotificationProgress::Determinate(value) => {
                        Some(Progress::new("notification-progress").value(value))
                    }
                };

                v_flex()
                    .w_full()
                    .when_some(progress, |this, progress| this.pt_2().child(progress))
                    .into_any_element()
            });

        if let Some(icon) = self.icon {
            notification = notification.icon(icon);
        }

        if let Some(action) = self.primary {
            notification =
                notification.action(move |_, _, _| action.button("notification-primary").primary());
        }

        if let Some(action) = self.secondary {
            notification = notification
                .secondary_action(move |_, _, _| action.button("notification-secondary").ghost());
        }

        notification
    }
}
