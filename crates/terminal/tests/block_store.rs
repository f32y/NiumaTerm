use nmt_terminal::block_store::*;
use nmt_terminal::event::BlockEvent;
use nmt_terminal::ghostty::BlockHandle;

fn handle(id: u64, generation: u64) -> BlockHandle {
    BlockHandle { id, generation }
}

/// `EngineBlock` items are born complete, carry the handle + cached
/// rows, and marry stashed metadata by seq.
#[test]
fn engine_block_items_marry_meta() {
    let mut store = BlockStore::default();
    store.update_meta(1, |m| m.command = Some("cargo build".into()));
    store.apply([
        BlockEvent::EngineBlock {
            seq: 1,
            handle: handle(10, 1),
            rows: 42,
        },
        BlockEvent::EngineBlock {
            seq: 2,
            handle: handle(11, 1),
            rows: 3,
        },
    ]);
    store.update_meta(2, |m| m.exit_code = Some(0));

    let items = store.items();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].handle(), Some(handle(10, 1)));
    assert_eq!(items[0].engine_rows(), 42);
    assert_eq!(items[0].meta.command.as_deref(), Some("cargo build"));
    assert_eq!(items[1].meta.exit_code, Some(0), "late meta hits the item");
}

/// `EngineBlocksSync` prunes items whose engine block is gone (budget
/// eviction, oldest first — counted so list splicing stays aligned) and
/// refreshes rows + generation after an engine reflow.
#[test]
fn engine_blocks_sync_prunes_and_refreshes() {
    let mut store = BlockStore::default();
    store.apply([
        BlockEvent::EngineBlock {
            seq: 1,
            handle: handle(10, 1),
            rows: 5,
        },
        BlockEvent::EngineBlock {
            seq: 2,
            handle: handle(11, 1),
            rows: 7,
        },
    ]);

    // Block 10 evicted; block 11 reflowed to a new generation and row count.
    store.apply([BlockEvent::EngineBlocksSync(vec![(handle(11, 2), 9)])]);

    let items = store.items();
    assert_eq!(items.len(), 1, "evicted handle pruned");
    assert_eq!(store.evicted_items, 1, "eviction counted for the list");
    assert_eq!(items[0].seq, Some(2));
    assert_eq!(items[0].handle(), Some(handle(11, 2)), "generation follows");
    assert_eq!(items[0].engine_rows(), 9, "rows follow the reflow");
}

/// `HistoryCleared` empties the store (`;K` clears the engine blocks on
/// the PTY side).
#[test]
fn history_cleared_drops_items() {
    let mut store = BlockStore::default();
    store.apply([
        BlockEvent::EngineBlock {
            seq: 1,
            handle: handle(10, 1),
            rows: 5,
        },
        BlockEvent::HistoryCleared,
    ]);
    assert!(store.items().is_empty());
}
