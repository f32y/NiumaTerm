/**
 * @file block.h
 *
 * Finished command blocks: frozen primary screens produced by
 * ghostty_terminal_finish_block.
 */

#ifndef GHOSTTY_VT_BLOCK_H
#define GHOSTTY_VT_BLOCK_H

#include <stddef.h>
#include <stdint.h>
#include <ghostty/vt/types.h>
#include <ghostty/vt/allocator.h>
#include <ghostty/vt/terminal.h>
#include <ghostty/vt/grid_ref.h>
#include <ghostty/vt/kitty_graphics.h>

#ifdef __cplusplus
extern "C" {
#endif

/** @defgroup block Command Blocks
 *
 * A command block is a primary screen frozen at a shell command boundary.
 * ghostty_terminal_finish_block() moves the current primary screen (its
 * pages, styles, and Kitty image storage) into the terminal's block set as
 * an O(1) ownership transfer and continues on a fresh primary screen with
 * the writer state (SGR, charsets, kitty keyboard, ...) carried over.
 *
 * Finished blocks are immutable: they never receive VT writes again. They
 * are ordered oldest-first and identified by a handle whose id is never
 * reused; a stale handle (removed block) always reports GHOSTTY_NO_VALUE.
 *
 * ## Thread contract
 *
 * Single writer, refcounted readers:
 *
 * - Every `ghostty_terminal_*` function in this header mutates or reads
 *   mutable state and must be called from the terminal's single writer
 *   thread (the thread that calls ghostty_terminal_vt_write).
 * - ghostty_terminal_block_acquire(), ghostty_block_ref_release(), and
 *   every `ghostty_block_ref_*` reader may be called from ANY thread. An
 *   acquired reference pins an immutable snapshot of the block: reads
 *   through it take no locks and never wait on the writer (e.g. a
 *   vt_write burst).
 * - While a reference is held the block cannot be freed: removal and
 *   eviction make the handle stale immediately but defer destruction to
 *   the last release.
 * - Reflow drains readers first: while a block is being reflowed,
 *   acquire returns GHOSTTY_NO_VALUE (retry next frame), and the reflow
 *   waits for outstanding references. Keep references short-lived (one
 *   read pass); a held reference blocks the writer's resize.
 * - All references must be released before ghostty_terminal_free().
 *
 * ## Reading block content
 *
 * On the writer thread, resolve a row with
 * ghostty_terminal_block_grid_ref(); from any thread, acquire a
 * reference and resolve with ghostty_block_ref_grid_ref(). Either way
 * the grid-ref readers (ghostty_grid_ref_cell, _row, _style, _graphemes,
 * _hyperlink_uri) work on the result. Rows at or beyond the block's row
 * count are not part of the block.
 *
 * @{
 */

/**
 * Identifies a finished block. A plain value type; copy freely.
 *
 * Lookup is by `id` alone; ids are never reused, so a stale id can never
 * resolve to another block. `generation` is the block's data version: it
 * changes when the block's content is rebuilt (reflow), making
 * (id, generation) a stable cache key for shaped/rendered rows. Fetch the
 * current generation with ghostty_terminal_block_at().
 *
 * @ingroup block
 */
typedef struct {
  uint64_t id;
  uint64_t generation;
} GhosttyBlockHandle;

/**
 * Finish the current command block.
 *
 * Freezes the primary screen into the block set and continues on a fresh
 * primary screen. The caller is responsible for only invoking this at
 * trusted command boundaries with the primary screen active (shell
 * integration gating).
 *
 * @param terminal The terminal handle
 * @param[out] out_handle On success, the handle of the frozen block (may be NULL)
 * @return GHOSTTY_SUCCESS when a block was created, GHOSTTY_NO_VALUE when
 *         the active screen has no content (no block is created),
 *         GHOSTTY_INVALID_VALUE when the alternate screen is active,
 *         GHOSTTY_OUT_OF_MEMORY on allocation failure (terminal unchanged)
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_finish_block(GhosttyTerminal terminal,
                                                        GhosttyBlockHandle *out_handle);

/**
 * Remove and destroy all finished blocks (e.g. a user-initiated clear).
 * Does not touch the active screen.
 *
 * @param terminal The terminal handle
 *
 * @ingroup block
 */
GHOSTTY_API void ghostty_terminal_clear_blocks(GhosttyTerminal terminal);

/**
 * Remove and destroy one finished block.
 *
 * @param terminal The terminal handle
 * @param handle The block to remove
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE if the handle is stale
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_remove_block(GhosttyTerminal terminal,
                                                        GhosttyBlockHandle handle);

/**
 * The number of finished blocks.
 *
 * @param terminal The terminal handle
 * @return The block count, 0 for a NULL terminal
 *
 * @ingroup block
 */
GHOSTTY_API size_t ghostty_terminal_block_count(GhosttyTerminal terminal);

/**
 * The handle of the block at the given index, oldest first.
 *
 * @param terminal The terminal handle
 * @param index Zero-based index, 0 is the oldest block
 * @param[out] out_handle On success, the handle at the index (may be NULL)
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE if index is out of range
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_block_at(GhosttyTerminal terminal,
                                                    size_t index,
                                                    GhosttyBlockHandle *out_handle);

/**
 * The logical row count of a block.
 *
 * This is the frozen screen's row count with blank rows after the
 * finish-time cursor truncated. Rows at or beyond this count are not part
 * of the block and cannot be resolved with
 * ghostty_terminal_block_grid_ref().
 *
 * @param terminal The terminal handle
 * @param handle The block
 * @param[out] out_rows On success, the logical row count (may be NULL)
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE if the handle is stale
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_block_row_count(GhosttyTerminal terminal,
                                                           GhosttyBlockHandle handle,
                                                           size_t *out_rows);

/**
 * The column count the block was frozen at. Blocks keep their frozen
 * width, which can differ from the live terminal width after a resize.
 *
 * @param terminal The terminal handle
 * @param handle The block
 * @param[out] out_cols On success, the column count (may be NULL)
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE if the handle is stale
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_block_cols(GhosttyTerminal terminal,
                                                      GhosttyBlockHandle handle,
                                                      uint16_t *out_cols);

/**
 * The memory retained by a block's page storage in bytes (page allocations
 * only; Kitty image bytes are budgeted separately).
 *
 * @param terminal The terminal handle
 * @param handle The block
 * @param[out] out_bytes On success, the retained page bytes (may be NULL)
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE if the handle is stale
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_block_bytes(GhosttyTerminal terminal,
                                                       GhosttyBlockHandle handle,
                                                       size_t *out_bytes);

/**
 * Reflow one finished block to the given width.
 *
 * This is the lazy-reflow driver: ghostty_terminal_resize() already
 * reflows every finished block eagerly, so calling this is only needed
 * when driving reflow selectively. Rebuilds the block's data and bumps
 * its generation (re-fetch via ghostty_terminal_block_at()); the logical
 * row count is recomputed, preserving intentional trailing blank rows.
 * Each block reflows independently: a block boundary is a hard wrap
 * boundary.
 *
 * @param terminal The terminal handle
 * @param handle The block
 * @param cols The new width; must be nonzero
 * @return GHOSTTY_SUCCESS on success (or already at the width),
 *         GHOSTTY_NO_VALUE if the handle is stale, GHOSTTY_INVALID_VALUE
 *         for cols == 0, GHOSTTY_OUT_OF_MEMORY on allocation failure (the
 *         block is destroyed rather than left in a garbage state)
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_reflow_block(GhosttyTerminal terminal,
                                                        GhosttyBlockHandle handle,
                                                        uint16_t cols);

/**
 * Total page-storage bytes retained by all finished blocks — the value
 * the block byte budget (GHOSTTY_TERMINAL_OPT_BLOCK_BUDGET_BYTES) is
 * enforced against.
 *
 * @param terminal The terminal handle
 * @return The combined page bytes, 0 for a NULL terminal
 *
 * @ingroup block
 */
GHOSTTY_API size_t ghostty_terminal_blocks_bytes(GhosttyTerminal terminal);

/**
 * Resolve a row of a finished block to a grid reference at x=0.
 *
 * The returned reference works with all grid-ref readers. Because the
 * block is immutable, the reference stays valid until the block is
 * removed (unlike active-screen refs, which any terminal mutation can
 * invalidate).
 *
 * @param terminal The terminal handle
 * @param handle The block
 * @param row The block-relative row, in [0, ghostty_terminal_block_row_count())
 * @param[out] out_ref On success, the resolved reference (may be NULL)
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE if the handle is
 *         stale, GHOSTTY_INVALID_VALUE if row is out of range
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_block_grid_ref(GhosttyTerminal terminal,
                                                          GhosttyBlockHandle handle,
                                                          size_t row,
                                                          GhosttyGridRef *out_ref);

/**
 * An acquired read reference to a finished block. Obtain with
 * ghostty_terminal_block_acquire(), release with
 * ghostty_block_ref_release(). Usable from any thread; see the thread
 * contract above.
 *
 * @ingroup block
 */
typedef void *GhosttyBlockRef;

/**
 * Take a read reference on a finished block. ANY THREAD.
 *
 * The reference pins an immutable snapshot readable without locks via the
 * ghostty_block_ref_* functions.
 *
 * @param terminal The terminal handle
 * @param handle The block
 * @param[out] out_ref On success, the acquired reference
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE for a stale handle
 *         or while the writer is reflowing the block (retry next frame)
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_terminal_block_acquire(GhosttyTerminal terminal,
                                                         GhosttyBlockHandle handle,
                                                         GhosttyBlockRef *out_ref);

/**
 * Release a reference from ghostty_terminal_block_acquire(). ANY THREAD.
 * Frees the block if it was removed/evicted and this was the last
 * reference. NULL is a no-op.
 *
 * @param ref The reference to release
 *
 * @ingroup block
 */
GHOSTTY_API void ghostty_block_ref_release(GhosttyBlockRef ref);

/**
 * The (id, generation) of the acquired snapshot — the stable cache key
 * for shaped/rendered rows. ANY THREAD (holding the ref).
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_block_ref_handle(GhosttyBlockRef ref,
                                                   GhosttyBlockHandle *out_handle);

/**
 * Logical row count of the snapshot. ANY THREAD (holding the ref).
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_block_ref_row_count(GhosttyBlockRef ref,
                                                      size_t *out_rows);

/**
 * Column count of the snapshot. ANY THREAD (holding the ref).
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_block_ref_cols(GhosttyBlockRef ref,
                                                 uint16_t *out_cols);

/**
 * Page-storage bytes of the snapshot. ANY THREAD (holding the ref).
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_block_ref_bytes(GhosttyBlockRef ref,
                                                  size_t *out_bytes);

/**
 * Resolve a row of the snapshot to a grid reference at x=0. All grid-ref
 * readers work on the result; it is valid while the reference is held.
 * ANY THREAD (holding the ref).
 *
 * @param ref The block reference
 * @param row The block-relative row, in [0, ghostty_block_ref_row_count())
 * @param[out] out_ref On success, the resolved grid reference
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_INVALID_VALUE if row is out
 *         of range
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_block_ref_grid_ref(GhosttyBlockRef ref,
                                                     size_t row,
                                                     GhosttyGridRef *out_ref);

/**
 * The snapshot's Kitty graphics storage (frozen images and placements).
 * All kitty_graphics_* readers (placement iterators, image pixel reads)
 * work on the handle; it is valid while the reference is held.
 * ANY THREAD (holding the ref).
 *
 * @param ref The block reference
 * @param[out] out_graphics On success, the graphics storage handle
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE if kitty graphics
 *         are disabled at build time
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_block_ref_kitty_graphics(GhosttyBlockRef ref,
                                                           GhosttyKittyGraphics *out_graphics);

/**
 * Block-relative grid position of the current placement's pin.
 *
 * ghostty_kitty_graphics_placement_screen_pos() resolves against the
 * ACTIVE screen's page list; a frozen placement's pin lives in the
 * block's own frozen pages, so it must resolve here instead. The
 * iterator must come from this block's kitty graphics storage
 * (ghostty_block_ref_kitty_graphics()). Row/col are block-relative —
 * the same space as ghostty_block_ref_grid_ref() rows.
 * ANY THREAD (holding the ref).
 *
 * @param ref The block reference
 * @param iterator The placement iterator positioned on a placement
 * @param[out] out_col On success, the block-relative column (may be NULL)
 * @param[out] out_row On success, the block-relative row (may be NULL)
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_NO_VALUE for a virtual
 *         (unicode placeholder) placement or when kitty graphics are
 *         disabled at build time, GHOSTTY_INVALID_VALUE if any handle is
 *         NULL or the iterator is not positioned
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_block_ref_placement_pos(
    GhosttyBlockRef ref,
    GhosttyKittyGraphicsPlacementIterator iterator,
    uint32_t *out_col,
    uint32_t *out_row);

/**
 * An inclusive cell range of a block for text export.
 *
 * This is a sized struct. Use GHOSTTY_INIT_SIZED() to initialize it.
 *
 * @ingroup block
 */
typedef struct {
  size_t size;
  size_t tl_row;
  uint16_t tl_col;
  size_t br_row;
  uint16_t br_col;
  /** Rejoin soft-wrapped lines (no newline at a wrap point). */
  bool unwrap;
  /** Trim trailing blanks from each line. */
  bool trim;
} GhosttyBlockFormatOptions;

/**
 * Export a cell range of the snapshot as plain UTF-8 text — the
 * copy/deep-search floor. Cross-block copy concatenates per-block
 * exports. ANY THREAD (holding the ref).
 *
 * The buffer is allocated with the given allocator (the default when
 * NULL) and ownership transfers to the caller: free it with
 * ghostty_free(allocator, ptr, len).
 *
 * @param ref The block reference
 * @param allocator The allocator (may be NULL for the default)
 * @param opts The cell range and formatting flags
 * @param[out] out_ptr On success, the UTF-8 buffer
 * @param[out] out_len On success, the buffer length in bytes
 * @return GHOSTTY_SUCCESS on success, GHOSTTY_INVALID_VALUE for an
 *         out-of-range or inverted row range, GHOSTTY_OUT_OF_MEMORY on
 *         allocation failure
 *
 * @ingroup block
 */
GHOSTTY_API GhosttyResult ghostty_block_ref_format_alloc(GhosttyBlockRef ref,
                                                         const GhosttyAllocator *allocator,
                                                         GhosttyBlockFormatOptions opts,
                                                         uint8_t **out_ptr,
                                                         size_t *out_len);

/** @} */

#ifdef __cplusplus
}
#endif

#endif /* GHOSTTY_VT_BLOCK_H */
