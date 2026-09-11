use std::collections::HashMap;
use std::sync::Arc;

use futures::channel::oneshot;

use crate::event::{Msg, MsgSender};
use crate::ghostty::{BlockHandle, PlacementScreenPos, ScreenRowRead};
use crate::graphics::GraphicData;
use crate::session::request::{Query, Request};

pub const PAGE_ROWS: usize = 64;
const CACHED_PAGES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageSource {
    Screen {
        revision: u64,
    },

    Block {
        id: u64,
        generation: u64,
        theme: u64,
    },
}

/// Owned rows and image positions contain no engine references. Holding a page
/// therefore cannot delay reflow, eviction, or destruction of the terminal.
#[derive(Debug)]
pub struct RowPage {
    pub source: PageSource,
    pub start: usize,
    pub cols: u16,
    pub rows: Vec<ScreenRowRead>,
    pub placements: Vec<PlacementScreenPos>,
}

impl RowPage {
    pub fn row(&self, index: usize) -> Option<&ScreenRowRead> {
        self.rows.get(index.checked_sub(self.start)?)
    }
}

enum Entry {
    Pending(Request<RowPage>),
    Ready(Option<Arc<RowPage>>),
}

struct ImageRead {
    used: u64,
    pending: Option<Request<GraphicData>>,
}

#[derive(Default)]
pub(super) struct PageCache {
    clock: u64,
    pages: HashMap<(PageSource, usize), (u64, Entry)>,
    images: HashMap<(u64, u64, u32), ImageRead>,
}

impl PageCache {
    /// Pixel ownership transfers to the renderer once, independently of the
    /// number of cached row pages an image overlaps.
    pub(super) fn take_image(
        &mut self,
        handle: BlockHandle,
        image_id: u32,
        sender: &MsgSender,
    ) -> Option<GraphicData> {
        let key = (handle.id, handle.generation, image_id);

        self.clock = self.clock.wrapping_add(1);

        if let Some(entry) = self.images.get_mut(&key) {
            entry.used = self.clock;

            let request = entry.pending.as_mut()?;

            match request.try_recv() {
                Ok(Some(Ok(data))) => {
                    self.images.remove(&key);

                    return Some(data);
                }

                Ok(None) => {}
                _ => entry.pending = None,
            }

            return None;
        }

        if self.images.len() >= CACHED_PAGES
            && let Some(oldest) = self
                .images
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| *key)
        {
            self.images.remove(&oldest);
        }

        let (reply, request) = oneshot::channel();

        if sender
            .send(Msg::Query(Query::Image {
                handle,
                image_id,
                reply,
            }))
            .is_ok()
        {
            self.images.insert(
                key,
                ImageRead {
                    used: self.clock,
                    pending: Some(request),
                },
            );
        }

        None
    }

    pub(super) fn read(
        &mut self,
        source: PageSource,
        row: usize,
        sender: &MsgSender,
    ) -> Option<Arc<RowPage>> {
        let start = row / PAGE_ROWS * PAGE_ROWS;
        let key = (source, start);

        self.clock = self.clock.wrapping_add(1);

        if let Some((used, entry)) = self.pages.get_mut(&key) {
            *used = self.clock;

            if let Entry::Pending(request) = entry {
                match request.try_recv() {
                    Ok(Some(result)) => *entry = Entry::Ready(result.ok().map(Arc::new)),
                    Err(_) => *entry = Entry::Ready(None),
                    Ok(None) => return None,
                }
            }

            return match entry {
                Entry::Ready(page) => page.clone(),
                Entry::Pending(_) => None,
            };
        }

        if self.pages.len() >= CACHED_PAGES
            && let Some(oldest) = self
                .pages
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(key, _)| *key)
        {
            self.pages.remove(&oldest);
        }

        let (reply, request) = oneshot::channel();

        if sender
            .send(Msg::Query(Query::Rows {
                source,
                start,
                reply,
            }))
            .is_ok()
        {
            self.pages
                .insert(key, (self.clock, Entry::Pending(request)));
        }

        None
    }
}
