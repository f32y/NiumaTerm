//! The prompt the user sent, and what can be done with one.
//!
//! A user row is the only kind the reader can still act on once it has
//! scrolled by -- copy it, branch in front of it, restore the files it changed
//! -- so the row carries a menu the other kinds have no use for.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, ObjectFit, Window, div, img, px, relative,
};
use gpui_component::modern_menu::{ModernMenu, ModernMenuExt as _};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, text, v_flex};
use nmt_i18n::i18n;

use crate::composer::attachments::MAX_ATTACHMENTS;
use crate::composer::{annotation_count_label, parse_annotated_prompt};
use crate::settings::UI_RADIUS;
use crate::transcript::disclosure_row::{
    USER_ANNOTATION_PADDING_Y, USER_BUBBLE_PADDING_X, USER_BUBBLE_PADDING_Y, USER_BUBBLE_RADIUS,
    USER_BUBBLE_TAIL_RADIUS, USER_BUBBLE_WIDTH_FRACTION,
};
use crate::transcript::render::TRANSCRIPT_THUMBNAIL;
use crate::transcript::reveal::{RevealKey, revealed};
use crate::transcript::{TranscriptView, entry_copy_text, truncated_user_prompt};

impl TranscriptView {
    /// Hover-revealed timestamp; the row declares `.group("entry")`.
    pub(crate) fn hover_stamp(&self, index: usize, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .invisible()
            .group_hover("entry", |this| this.visible())
            .child(self.items[index].at.clone())
    }

    pub(crate) fn copy_menu(
        pane: gpui::WeakEntity<Self>,
        index: usize,
    ) -> impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static {
        move |menu, _, cx| {
            // Full transcript payloads can be very large. Resolve and clone the
            // text only after a right click opens the menu, keeping ordinary
            // list layout independent of the hidden message size.
            let copy_text = pane
                .read_with(cx, |pane, _| {
                    pane.items
                        .get(index)
                        .map(|entry| entry_copy_text(&entry.item))
                })
                .ok()
                .flatten();

            match copy_text {
                Some(copy_text) => menu
                    .item(i18n("agent-transcript-copy"), move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                    })
                    .icon(IconName::Copy),
                None => menu,
            }
        }
    }

    /// The copy item plus the actions a prompt offers over the conversation
    /// it opened: branching in front of it, or returning to it.
    ///
    /// Which of the two appears follows the backend. Where a branch is a
    /// request the harness answers, the prompt names a cut and nothing else;
    /// where the conversation is a transcript file this side rewrites, the
    /// same cut also decides what happens to the files that turn touched, so
    /// the rewind actions are what the prompt leads to.
    fn user_row_menu(
        &self,
        index: usize,
        cx: &Context<Self>,
    ) -> impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static {
        let copy = Self::copy_menu(cx.entity().downgrade(), index);
        let caps = self.kind.caps();
        // Resolved now rather than when the menu opens: a prompt's place among
        // the turns is a property of the transcript as it stands, and the rows
        // can move under a menu that is already up.
        let target = self
            .owner()
            .filter(|_| caps.session_fork || caps.file_rewind)
            .zip(self.prompt_target(index))
            .map(|(owner, target)| (owner.clone(), target));

        move |menu, window, cx| {
            let menu = copy(menu, window, cx);
            let Some((pane, target)) = target.clone() else {
                return menu;
            };

            if caps.session_fork {
                menu.separator()
                    .item(i18n("agent-transcript-fork-from-here"), move |_, cx| {
                        let target = target.clone();
                        pane.update(cx, |pane, cx| pane.fork_from_prompt(target, cx))
                            .ok();
                    })
                    .icon(IconName::GitBranch)
            } else {
                menu.separator()
                    .item(i18n("agent-transcript-rewind-to-here"), move |_, cx| {
                        let target = target.clone();
                        pane.update(cx, |pane, cx| pane.rewind_to_prompt(target, cx))
                            .ok();
                    })
                    .icon(IconName::Undo)
            }
        }
    }

    /// User prompt: right-aligned quiet bubble (muted surface, no border).
    ///
    /// Oversized prompts (huge pastes) collapse to their head by default:
    /// a visible row re-lays-out its full text every frame, so an unbounded
    /// prompt would make every frame O(paste size). Expansion is an explicit
    /// per-row choice, and the right-click Copy always carries the full text.
    pub(crate) fn render_user_row(
        &self,
        index: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let parsed = parse_annotated_prompt(text);
        let text = parsed.as_ref().map_or(text, |parsed| parsed.prompt);
        let head_len = truncated_user_prompt(text).map(str::len);
        let expanded = head_len.is_some() && self.disclosures.row_expanded(index);
        let shown = match (head_len, expanded) {
            (Some(len), false) => text[..len].to_string(),
            _ => text.to_string(),
        };
        // A prompt long enough to fold is a pasted block rather than a
        // sentence, and it takes the column's whole measure. Sized to its
        // content it would instead be as wide as the longest line of whichever
        // half is on screen, so opening it would move its edges as well as its
        // height; measuring the hidden half to avoid that is the layout pass
        // the fold exists to skip.
        let fills_column = head_len.is_some();
        let toggle = head_len.is_some().then(|| {
            div()
                .mt_1()
                .text_xs()
                .text_color(cx.theme().primary)
                .cursor_pointer()
                .child(if expanded {
                    i18n("agent-transcript-show-less").to_string()
                } else {
                    i18n("agent-transcript-show-full-message").to_string()
                })
                .id(("user-expand", index))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_disclosure(RevealKey::Row(index), cx)
                }))
        });

        // The prompt fold above swaps the text inside one bubble rather than
        // opening a block below it, so it takes no entrance of its own: fading
        // it in would fade the half of the prompt that was already on screen.
        // Its toggle still pins the reading position, which is what a paste
        // long enough to fold actually needs.
        let annotations_reveal = self
            .disclosures
            .progress(RevealKey::Annotation(index), Instant::now());
        // The quotations open a rounded bubble, and a clip box is a rectangle,
        // so they fade in place rather than growing by height: squaring off
        // the corner the bubble is known by would cost more than the height
        // ramp buys on a block this size. The card is shaped for as long as
        // they are on screen and the wording answers the click at once.
        let annotations_shown =
            self.disclosures.annotation_expanded(index) && annotations_reveal > 0.0;
        let annotations_disclosing = self.disclosures.is_disclosing(RevealKey::Annotation(index));
        let annotations = parsed.as_ref().and_then(|parsed| {
            (!parsed.annotations.is_empty()).then(|| {
                let action_label = if annotations_disclosing {
                    i18n("agent-transcript-annotations-collapse")
                } else {
                    i18n("agent-transcript-annotations-expand")
                };
                let content = annotations_shown.then(|| {
                    v_flex()
                        .w_full()
                        .gap_2()
                        .map(|this| revealed(this, annotations_reveal))
                        // Closes the bubble the header opens, and takes the
                        // header's own edge inset so a quotation starts on the
                        // same column the header's label does.
                        .rounded_b(px(USER_BUBBLE_RADIUS))
                        .bg(cx.theme().muted)
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .px(px(USER_BUBBLE_PADDING_X))
                        .py(px(USER_BUBBLE_PADDING_Y))
                        .children(parsed.annotations.iter().enumerate().map(
                            |(position, annotation)| {
                                h_flex()
                                    .w_full()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{}.", position + 1)),
                                    )
                                    .child(
                                        div().flex_1().min_w_0().child(
                                            text::TextView::plain(
                                                format!(
                                                    "entry-response-annotation-{index}-{position}"
                                                ),
                                                annotation.text.clone(),
                                            )
                                            .selectable(true),
                                        ),
                                    )
                            },
                        ))
                });

                v_flex()
                    // Sized to what it says, like the prompt below it, and
                    // right-aligned with it by the column both sit in. The
                    // quotations are the wider of the two, so opening them is
                    // what grows the bubble.
                    .min_w_0()
                    .child(
                        // A second bubble in the prompt's own language: same
                        // fill, same corner, same edge inset, quieter text.
                        // Its padding and inherited text size come from the
                        // bubble rather than from a button size, because the
                        // transcript's text size is a setting and a control
                        // with a fixed height would stop matching the bubble
                        // below it as soon as that setting moves.
                        h_flex()
                            .id(("entry-response-annotations", index))
                            .role(gpui::Role::Button)
                            .aria_label(action_label)
                            .w_full()
                            .px(px(USER_BUBBLE_PADDING_X))
                            .py(px(USER_ANNOTATION_PADDING_Y))
                            .bg(cx.theme().muted)
                            .text_color(cx.theme().muted_foreground)
                            // Squares off where the quotations meet it, and is
                            // a closed capsule while they are hidden.
                            .map(|this| match annotations_shown {
                                true => this.rounded_t(px(USER_BUBBLE_RADIUS)),
                                false => this.rounded(px(USER_BUBBLE_RADIUS)),
                            })
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().accent))
                            .child(Icon::new(IconName::TextSelect).xsmall())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(annotation_count_label(parsed.annotations.len())),
                            )
                            .child(
                                Icon::new(if annotations_disclosing {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .xsmall(),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_disclosure(RevealKey::Annotation(index), cx)
                            })),
                    )
                    .children(content)
            })
        });
        let message = div()
            // The width cap lives on the column below, which has a definite
            // width to take a fraction of. A fraction here would resolve
            // against this bubble's own shrink-to-fit parent instead, wrapping
            // every prompt at a fraction of its natural single-line width.
            .min_w_0()
            .when(fills_column, |this| this.w_full())
            .px(px(USER_BUBBLE_PADDING_X))
            .py(px(USER_BUBBLE_PADDING_Y))
            .rounded_tl(px(USER_BUBBLE_RADIUS))
            .rounded_tr(px(USER_BUBBLE_RADIUS))
            .rounded_bl(px(USER_BUBBLE_RADIUS))
            // The one square-ish corner faces the conversation the prompt was
            // sent into, which is what marks the bubble as this side of it.
            .rounded_br(px(USER_BUBBLE_TAIL_RADIUS))
            .bg(cx.theme().muted)
            // Plain, not markdown: the prompt is user-authored text and
            // must render verbatim, but stays drag-selectable.
            .child(text::TextView::plain(("user-text", index), shown).selectable(true))
            .children(toggle)
            .children(self.render_entry_images(index, cx));

        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .justify_end()
            .items_end()
            .gap_2()
            .modern_context_menu(self.user_row_menu(index, cx))
            .child(self.hover_stamp(index, cx))
            .child(
                v_flex()
                    // Both bubbles size to their own content and end on this
                    // column's trailing edge, so the cap that keeps a prompt
                    // off the full width lives here rather than on either. The
                    // row above is `w_full`, so the fraction has a definite
                    // width to resolve against and tracks the pane.
                    //
                    // A foldable prompt takes that measure as its width rather
                    // than as a ceiling: a bubble asking for the full width of
                    // a shrink-to-fit column would still be sized by its own
                    // longest line, since a percentage contributes nothing to
                    // what a column asks for.
                    .map(|this| match fills_column {
                        true => this.w(relative(USER_BUBBLE_WIDTH_FRACTION)),
                        false => this.max_w(relative(USER_BUBBLE_WIDTH_FRACTION)),
                    })
                    .min_w_0()
                    .items_end()
                    .gap_1()
                    .children(annotations)
                    .child(message),
            )
            .into_any_element()
    }

    /// The images a message carried, under its text. A reader who scrolls back
    /// should see what was sent, not the placeholder that stood in for it while
    /// the message was being written.
    fn render_entry_images(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let images = &self.items.get(index)?.images;
        if images.is_empty() {
            return None;
        }

        Some(
            h_flex()
                .mt_2()
                .gap_2()
                .flex_wrap()
                .justify_end()
                .children(images.iter().enumerate().map(|(position, image)| {
                    div()
                        .size(px(TRANSCRIPT_THUMBNAIL))
                        .flex_none()
                        .rounded(UI_RADIUS)
                        .overflow_hidden()
                        .border_1()
                        .border_color(cx.theme().border)
                        // Unique across rows: a row carries at most
                        // `MAX_ATTACHMENTS` images, so its band cannot overlap
                        // the next row's.
                        .id(("entry-image", index * MAX_ATTACHMENTS + position))
                        // A thumbnail is cropped to a square and small enough
                        // to only recognize the image by, so opening it is the
                        // only way to read what was sent.
                        .cursor_pointer()
                        .aria_label(i18n("agent-transcript-image-open"))
                        .on_click(cx.listener({
                            let image = image.clone();
                            move |this, _, _, cx| this.zoom_image(image.clone(), cx)
                        }))
                        .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                }))
                .into_any_element(),
        )
    }
}
