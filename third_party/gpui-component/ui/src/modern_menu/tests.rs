use std::rc::Rc;

use gpui::{Bounds, Pixels, point, px, size};

use crate::modern_menu::{
    Activation, Entry, Item, ModernMenu, ModernMenuInput, normalize_separators,
};

use crate::modern_menu::metrics::{
    BORDER_WIDTH, CHEVRON_GAP, CHEVRON_SIZE, COMMAND_BUTTON_WIDTH, COMMAND_ROW_HEIGHT, Content,
    ICON_GAP, ICON_SIZE, ITEM_MARGIN_X, ITEM_PADDING_X, MIN_MENU_WIDTH, PRESENTER_PADDING_Y,
    SEPARATOR_HEIGHT, Side, TOUCH_MIN_MENU_WIDTH, item_height, menu_size, place, place_submenu,
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

fn compact_menu_size(widest_label: Pixels, content: Content) -> gpui::Size<Pixels> {
    menu_size(widest_label, content, ModernMenuInput::Mouse)
}

#[test]
fn menu_height_follows_item_count() {
    let one = compact_menu_size(px(200.0), rows(1, 0));
    let three = compact_menu_size(px(200.0), rows(3, 0));

    assert_eq!(
        one.height,
        item_height(ModernMenuInput::Mouse) + (PRESENTER_PADDING_Y + BORDER_WIDTH) * 2.0
    );
    assert_eq!(
        three.height - one.height,
        item_height(ModernMenuInput::Mouse) * 2.0
    );
}

#[test]
fn touch_rows_and_minimum_width_are_larger() {
    let compact = menu_size(px(1.0), rows(2, 0), ModernMenuInput::Mouse);
    let keyboard = menu_size(px(1.0), rows(2, 0), ModernMenuInput::Keyboard);
    let touch = menu_size(px(1.0), rows(2, 0), ModernMenuInput::Touch);

    assert_eq!(keyboard, compact);
    assert_eq!(compact.width, MIN_MENU_WIDTH);
    assert_eq!(touch.width, TOUCH_MIN_MENU_WIDTH);
    assert_eq!(
        touch.height - compact.height,
        (item_height(ModernMenuInput::Touch) - item_height(ModernMenuInput::Mouse)) * 2.0
    );
}

#[test]
fn separators_add_their_own_height() {
    let plain = compact_menu_size(px(200.0), rows(3, 0));
    let divided = compact_menu_size(px(200.0), rows(3, 2));

    assert_eq!(divided.height - plain.height, SEPARATOR_HEIGHT * 2.0);
    assert_eq!(divided.width, plain.width);
}

#[test]
fn a_label_gets_exactly_the_room_it_was_measured_for() {
    // Everything around the label must be included or the widest label is
    // clipped by the popup edge.
    let inset = (ITEM_MARGIN_X + ITEM_PADDING_X + BORDER_WIDTH) * 2.0 + ICON_SIZE + ICON_GAP;

    assert_eq!(
        compact_menu_size(px(200.0), rows(1, 0)).width - px(200.0),
        inset
    );
}

#[test]
fn minimum_width_applies_to_the_complete_menu() {
    assert_eq!(compact_menu_size(px(1.0), rows(1, 0)).width, MIN_MENU_WIDTH);
}

#[test]
fn a_command_row_adds_its_height_and_can_widen_the_menu() {
    let plain = compact_menu_size(px(200.0), rows(2, 0));
    let with_row = compact_menu_size(
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
    let expected = COMMAND_BUTTON_WIDTH * 6.0 + ITEM_MARGIN_X * 2.0 + BORDER_WIDTH * 2.0;

    assert_eq!(compact_menu_size(px(96.0), content).width, expected);
}

#[test]
fn menu_opens_down_and_right_when_it_fits() {
    let menu = size(px(200.0), px(100.0));

    assert_eq!(
        place(point(px(100.0), px(100.0)), menu, work_area(), Side::Below),
        point(px(100.0), px(100.0))
    );
}

#[test]
fn an_above_menu_ends_at_its_anchor() {
    let menu = size(px(200.0), px(100.0));

    assert_eq!(
        place(point(px(100.0), px(300.0)), menu, work_area(), Side::Above),
        point(px(100.0), px(200.0))
    );
}

#[test]
fn an_above_menu_with_no_room_opens_below_instead() {
    let menu = size(px(200.0), px(100.0));

    assert_eq!(
        place(point(px(100.0), px(20.0)), menu, work_area(), Side::Above),
        point(px(100.0), px(20.0))
    );
}

#[test]
fn menu_flips_to_the_other_side_of_a_crowded_anchor() {
    let menu = size(px(200.0), px(100.0));
    let placed = place(point(px(950.0), px(760.0)), menu, work_area(), Side::Below);

    assert_eq!(placed, point(px(750.0), px(660.0)));
}

#[test]
fn a_menu_taller_than_the_work_area_keeps_its_top_on_screen() {
    let menu = size(px(200.0), px(900.0));
    let placed = place(point(px(100.0), px(700.0)), menu, work_area(), Side::Below);

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
            Entry::Submenu(_) => "submenu",
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

#[test]
fn a_menu_that_opens_submenus_keeps_a_chevron_column() {
    let plain = compact_menu_size(px(200.0), rows(2, 0));
    let nesting = compact_menu_size(
        px(200.0),
        Content {
            items: 2,
            chevrons: true,
            ..Default::default()
        },
    );

    assert_eq!(nesting.width - plain.width, CHEVRON_SIZE + CHEVRON_GAP);
    assert_eq!(nesting.height, plain.height);
}

fn parent() -> Bounds<Pixels> {
    Bounds {
        origin: point(px(100.0), px(100.0)),
        size: size(px(200.0), px(300.0)),
    }
}

#[test]
fn a_submenu_opens_beside_the_row_it_belongs_to() {
    let placed = place_submenu(parent(), px(64.0), size(px(150.0), px(100.0)), work_area());

    // Both menus inset their first row by the same amount, so aligning the
    // surfaces this way puts the submenu's first row against its own row.
    assert_eq!(placed.y, px(164.0));
    assert!(
        placed.x < px(300.0) && placed.x > px(280.0),
        "the submenu shares its parent's right edge, got {placed:?}"
    );
}

#[test]
fn a_submenu_with_no_room_to_its_right_opens_to_the_left() {
    let crowded = Bounds {
        origin: point(px(700.0), px(100.0)),
        size: size(px(280.0), px(300.0)),
    };
    let placed = place_submenu(crowded, px(0.0), size(px(260.0), px(100.0)), work_area());

    assert!(
        placed.x < crowded.origin.x,
        "a submenu that would leave the work area takes the other side, got {placed:?}"
    );
}

#[test]
fn a_submenu_taller_than_the_room_below_its_row_slides_up() {
    let placed = place_submenu(parent(), px(280.0), size(px(150.0), px(600.0)), work_area());

    assert_eq!(
        placed.y,
        px(800.0) - px(4.0) - px(600.0),
        "it stays inside the work area rather than running off the bottom"
    );
}

#[test]
fn a_submenu_with_nothing_in_it_is_dropped() {
    let menu = ModernMenu::new()
        .item("visible", |_, _| {})
        .submenu("more", |menu| menu);

    assert_eq!(kinds(&menu.entries), ["item"]);
}

#[test]
fn a_submenu_row_carries_its_own_entries() {
    let menu = ModernMenu::new().submenu("more", |menu| {
        menu.separator()
            .item("first", |_, _| {})
            .item("second", |_, _| {})
    });

    let Some(Entry::Submenu(submenu)) = menu.entries.first() else {
        panic!("the submenu row is kept");
    };
    assert_eq!(
        kinds(&submenu.entries),
        ["item", "item"],
        "a submenu settles its own separators as any menu does"
    );
}
