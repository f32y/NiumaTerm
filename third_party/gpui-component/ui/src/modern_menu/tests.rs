use std::rc::Rc;

use gpui::{Bounds, Pixels, point, px, size};

use crate::modern_menu::{Activation, Entry, Item, normalize_separators};

use crate::modern_menu::metrics::{
    COMMAND_BUTTON_WIDTH, COMMAND_ROW_HEIGHT, Content, ICON_GAP, ICON_SIZE, ITEM_HEIGHT,
    ITEM_PADDING_X, MENU_PADDING, SEPARATOR_HEIGHT, STROKE_WIDTH, menu_size, place,
};

fn rows(items: usize, separators: usize) -> Content {
    Content {
        items,
        separators,
        ..Default::default()
    }
}

fn work_area() -> Bounds<Pixels> {
    Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(1000.0), px(800.0)),
    }
}

#[test]
fn menu_height_follows_item_count() {
    let one = menu_size(px(200.0), rows(1, 0));
    let three = menu_size(px(200.0), rows(3, 0));

    assert_eq!(one.height, ITEM_HEIGHT + MENU_PADDING * 2.0);
    assert_eq!(three.height - one.height, ITEM_HEIGHT * 2.0);
}

#[test]
fn separators_add_their_own_height() {
    let plain = menu_size(px(200.0), rows(3, 0));
    let divided = menu_size(px(200.0), rows(3, 2));

    assert_eq!(divided.height - plain.height, SEPARATOR_HEIGHT * 2.0);
    assert_eq!(divided.width, plain.width);
}

#[test]
fn a_label_gets_exactly_the_room_it_was_measured_for() {
    // Everything a row is inset by: the outline, the menu's own padding, the
    // item's, and the icon column every row keeps. Miss one and the widest label
    // is clipped by the window edge.
    let inset = (STROKE_WIDTH + MENU_PADDING + ITEM_PADDING_X) * 2.0 + ICON_SIZE + ICON_GAP;

    assert_eq!(menu_size(px(200.0), rows(1, 0)).width - px(200.0), inset);
}

#[test]
fn narrow_labels_do_not_shrink_the_menu_below_its_floor() {
    assert_eq!(
        menu_size(px(1.0), rows(1, 0)).width,
        menu_size(px(96.0), rows(1, 0)).width
    );
}

#[test]
fn a_command_row_adds_its_height_and_can_widen_the_menu() {
    let plain = menu_size(px(200.0), rows(2, 0));
    let with_row = menu_size(
        px(200.0),
        Content {
            items: 2,
            command_rows: 1,
            widest_command_row: 3,
            ..Default::default()
        },
    );

    assert_eq!(with_row.height - plain.height, COMMAND_ROW_HEIGHT);
    // Three buttons still fit inside what a 200 wide label already asks for.
    assert_eq!(with_row.width, plain.width);
}

#[test]
fn a_command_row_wider_than_the_labels_sets_the_width() {
    let content = Content {
        items: 1,
        command_rows: 1,
        widest_command_row: 6,
        ..Default::default()
    };
    let expected = COMMAND_BUTTON_WIDTH * 6.0 + (MENU_PADDING + STROKE_WIDTH) * 2.0;

    assert_eq!(menu_size(px(96.0), content).width, expected);
}

#[test]
fn menu_opens_down_and_right_when_it_fits() {
    let menu = size(px(200.0), px(100.0));

    assert_eq!(
        place(point(px(100.0), px(100.0)), menu, work_area()),
        point(px(100.0), px(100.0))
    );
}

#[test]
fn menu_flips_to_the_other_side_of_a_crowded_anchor() {
    let menu = size(px(200.0), px(100.0));
    let placed = place(point(px(950.0), px(760.0)), menu, work_area());

    assert_eq!(placed, point(px(750.0), px(660.0)));
}

#[test]
fn a_menu_taller_than_the_work_area_keeps_its_top_on_screen() {
    let menu = size(px(200.0), px(900.0));
    let placed = place(point(px(100.0), px(700.0)), menu, work_area());

    assert_eq!(placed.y, px(4.0));
}

/// Build an item whose activation does nothing; these exercise layout, not what
/// choosing an entry runs.
fn item() -> Entry {
    Entry::Item(Item {
        label: "item".into(),
        disabled: false,
        activation: Activation::Handler(Rc::new(|_, _| {})),
        icon: None,
    })
}

fn kinds(entries: &[Entry]) -> Vec<&'static str> {
    entries
        .iter()
        .map(|entry| match entry {
            Entry::Separator => "rule",
            Entry::Item(_) => "item",
            Entry::Commands(_) => "commands",
        })
        .collect()
}

#[test]
fn separators_with_nothing_to_separate_are_dropped() {
    let entries = normalize_separators(vec![
        Entry::Separator,
        item(),
        Entry::Separator,
        Entry::Separator,
        item(),
        Entry::Separator,
    ]);

    assert_eq!(kinds(&entries), ["item", "rule", "item"]);
}

#[test]
fn a_command_row_is_ruled_off_from_what_follows_it() {
    let entries = normalize_separators(vec![Entry::Commands(vec![]), item()]);

    assert_eq!(kinds(&entries), ["commands", "rule", "item"]);
}

#[test]
fn a_command_row_with_nothing_after_it_gains_no_rule() {
    let entries = normalize_separators(vec![Entry::Commands(vec![])]);

    assert_eq!(kinds(&entries), ["commands"]);
}
