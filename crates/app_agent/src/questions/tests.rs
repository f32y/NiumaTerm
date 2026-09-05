use nmt_agent_utils::chat::{Question, QuestionOption};

use crate::questions::QuestionPrompt;

fn question(text: &str, multi_select: bool, labels: &[&str]) -> Question {
    Question {
        input: Default::default(),
        header: None,
        question: text.to_owned(),
        multi_select,
        options: labels
            .iter()
            .map(|label| QuestionOption {
                label: (*label).to_owned(),
                description: None,
            })
            .collect(),
    }
}

#[test]
fn single_select_replaces_and_multi_select_toggles_in_option_order() {
    let mut prompt = QuestionPrompt::new(vec![
        question("Which database?", false, &["Postgres", "SQLite"]),
        question("Which extras?", true, &["Metrics", "Tracing", "Audit log"]),
    ]);

    assert!(!prompt.is_complete());

    prompt.toggle(0, 1);
    prompt.toggle(0, 0);
    assert!(prompt.is_selected(0, 0));
    assert!(!prompt.is_selected(0, 1));

    // Picked out of order; the answer still follows the visible order.
    prompt.toggle(1, 2);
    prompt.toggle(1, 0);
    assert!(prompt.is_complete());
    assert_eq!(
        prompt.answers(),
        vec![
            vec!["Postgres".to_owned()],
            vec!["Metrics".to_owned(), "Audit log".to_owned()],
        ]
    );

    // Re-clicking a multi-select option clears it, and clearing the last
    // pick of a question blocks submission again.
    prompt.toggle(1, 0);
    prompt.toggle(1, 2);
    assert!(!prompt.is_complete());
    assert_eq!(prompt.answers()[1], Vec::<String>::new());
}

#[test]
fn the_highlight_walks_every_option_across_questions_and_wraps() {
    let mut prompt = QuestionPrompt::new(vec![
        question("Which database?", false, &["Postgres", "SQLite"]),
        question("Which extras?", true, &["Metrics", "Tracing"]),
    ]);

    assert!(prompt.is_focused(0, 0));

    // Down crosses the question boundary rather than stopping at it, so one
    // pair of keys reaches every option on the card.
    let walked: Vec<(usize, usize)> = (0..4)
        .map(|_| {
            prompt.move_focus(true);
            prompt.focus
        })
        .collect();
    assert_eq!(walked, vec![(0, 1), (1, 0), (1, 1), (0, 0)]);

    // Up from the first option wraps to the last.
    prompt.move_focus(false);
    assert_eq!(prompt.focus, (1, 1));
}

#[test]
fn a_question_with_no_options_cannot_trap_the_highlight() {
    // The provider caps options at four but does not promise a minimum, and
    // a card that swallows the arrow keys would leave the user no way to
    // reach the options that do exist.
    let mut prompt = QuestionPrompt::new(vec![
        question("Nothing to pick", false, &[]),
        question("Which database?", false, &["Postgres", "SQLite"]),
    ]);

    // The first press reaches the first drawn option rather than stepping
    // over it, which is what an out-of-range starting highlight would do.
    assert!(prompt.move_focus(true));
    assert_eq!(prompt.focus, (1, 0));

    // A card with nothing to pick consumes no keys, so they still reach
    // whatever else is listening.
    let empty = &mut QuestionPrompt::new(vec![question("Nothing at all", false, &[])]);
    assert!(!empty.move_focus(true));
}
