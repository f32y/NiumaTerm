pub(crate) mod frame_cache;
pub(crate) mod frame_record;
pub(crate) mod key_action;
mod settings;
pub(crate) use crate::pane_model::settings::{FrameTheme, PaneSettings};

mod blocks;
pub(crate) mod frozen_hit_map;
mod gutter_selection;
mod links;
pub(crate) mod list_mirror;
pub(crate) mod mouse;
pub(crate) mod scroll;
mod scrollbar_activity;
pub(crate) mod selection_drag;
pub(crate) mod viewport;

use nmt_terminal::session::{HostEvent, InFlightBlock};

use crate::block_list::chrome::DurationLabels;
use crate::dirty::DirtyState;
use crate::frame_source::TerminalFrameSource;
use crate::layout::bottom_anchor_offsets;
use crate::metrics::CellMetrics;
use crate::pane_model::frame_cache::TerminalFrameCache;
use crate::pane_model::frozen_hit_map::FrozenHitMap;
use crate::pane_model::gutter_selection::GutterSelection;
use crate::pane_model::links::LinkHover;
use crate::pane_model::list_mirror::BlockListMirror;
use crate::pane_model::scrollbar_activity::ScrollbarActivity;
use crate::pane_model::selection_drag::FrozenSelectionDrag;
use crate::pane_model::viewport::Viewport;

pub(crate) struct PaneController {
    pub source: TerminalFrameSource,
    pub settings: PaneSettings,
    pub theme: FrameTheme,
    pub duration_labels: DurationLabels,
    pub frame_cache: TerminalFrameCache,
    pub dirty: DirtyState,
    pub cell_metrics: Option<CellMetrics>,
    pub content_size: (f32, f32),
    pub in_flight: Option<InFlightBlock>,
    pub open_prompt: bool,
    pub block_list: BlockListMirror,
    pub frozen: FrozenHitMap,
    pub gutter: GutterSelection,
    pub frozen_drag: FrozenSelectionDrag,
    pub scrollbar: ScrollbarActivity,
    pub links: LinkHover,
    pub viewport: Viewport,
}

impl PaneController {
    pub(crate) fn new(
        source: TerminalFrameSource,
        settings: PaneSettings,
        theme: FrameTheme,
        duration_labels: DurationLabels,
    ) -> Self {
        Self {
            source,
            settings,
            theme,
            duration_labels,
            frame_cache: TerminalFrameCache::default(),
            dirty: DirtyState::default(),
            cell_metrics: None,
            content_size: (0.0, 0.0),
            in_flight: None,
            open_prompt: false,
            block_list: BlockListMirror::default(),
            frozen: FrozenHitMap::default(),
            gutter: GutterSelection::default(),
            frozen_drag: FrozenSelectionDrag::default(),
            scrollbar: ScrollbarActivity::default(),
            links: LinkHover::default(),
            viewport: Viewport::default(),
        }
    }

    pub(crate) fn block_list_mode(&self) -> bool {
        self.source.session.engine_blocks() && !self.source.session.alt_screen()
    }

    pub(crate) fn content_cols(&self) -> u32 {
        self.cell_metrics.map_or(80, |cell| {
            (self.content_size.0 / cell.width_px).floor().max(1.0) as u32
        })
    }

    pub(crate) fn refresh_frame(&mut self) {
        let previous = self.frame_cache.reusable_frame();
        self.frame_cache
            .rebuild(self.source.frame(previous.as_ref(), &self.theme));
        self.update_viewport();
    }

    pub(crate) fn update_viewport(&mut self) {
        self.viewport = if self.block_list_mode() {
            Viewport::BlockList {
                scroll_px: self.block_list.scrollbar.0,
                max_scroll_px: self.block_list.scrollbar.1,
                active_top: self.frozen.active_top(),
                viewport_px: self.content_size.1,
            }
        } else {
            let frame = self.frame_cache.current().unwrap_or_default();
            Viewport::Grid {
                scrollbar: frame.scrollbar(),
                row_offsets: self.cell_metrics.map_or_else(Vec::new, |cell| {
                    bottom_anchor_offsets(&frame, cell.height_px, self.settings.fixed_bottom)
                }),
            }
        };
    }

    pub(crate) fn drain_host_events(&mut self) -> Vec<HostEvent> {
        let events = self.source.session.poll_events();
        for event in &events {
            match event {
                HostEvent::CommandFinished { .. } => {
                    self.frame_cache.invalidate();
                    self.refresh_blocks();
                }
                HostEvent::CommandStarted
                | HostEvent::PromptStarted
                | HostEvent::PromptBoundaryTrusted(false)
                | HostEvent::Exit => self.refresh_blocks(),
                _ => {}
            }
        }
        events
    }

    fn refresh_blocks(&mut self) {
        self.in_flight = self.source.session.in_flight_block();
        self.open_prompt = self.source.session.open_prompt_region();
    }
}

#[cfg(test)]
mod test_session;
#[cfg(test)]
mod tests;
