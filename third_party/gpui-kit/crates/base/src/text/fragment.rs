use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, EntityId, WeakEntity, WindowId};

use crate::TextSelectionCoverage;
use crate::TextSelectionSnapshot;
use crate::text::SelectionFormat;
use crate::text::document::ParsedDocument;
use crate::text::state::{ParsedContent, TextViewState};

#[derive(Clone)]
pub struct TextFragment {
    pub(crate) content: Arc<ParsedContent>,
    pub(crate) index: usize,
    pub(crate) key: (usize, usize, usize, bool),
    pub(crate) group: Rc<RefCell<FragmentSelection>>,
}

pub(crate) struct FragmentView {
    pub(crate) index: usize,
    pub(crate) key: (usize, usize, usize, bool),
    pub(crate) group: Rc<RefCell<FragmentSelection>>,
}

impl TextFragment {
    /// The source range and replacement revision identify an unchanged block.
    pub fn key(&self) -> (usize, usize, usize, bool) {
        self.key
    }

    pub(crate) fn content(&self) -> ParsedContent {
        let mut content = self.content.as_ref().clone();

        content.document.blocks = vec![content.document.blocks[self.index].clone()].into();
        content.document.source = Default::default();

        content
    }

    pub(crate) fn into_view(self) -> FragmentView {
        FragmentView {
            index: self.index,
            key: self.key,
            group: self.group,
        }
    }
}

pub(crate) struct FragmentSelection {
    pub(crate) id: Option<EntityId>,
    pub(crate) document: ParsedDocument,
    pub(crate) members: HashMap<EntityId, (usize, WeakEntity<TextViewState>)>,
    pub(crate) snapshots: HashMap<EntityId, TextSelectionSnapshot>,
    pub(crate) select_all: Option<usize>,
    pub(crate) format: SelectionFormat,
    pub(crate) window: Option<WindowId>,
}

impl FragmentSelection {
    pub(crate) fn new(document: ParsedDocument) -> Self {
        Self {
            id: None,
            document,
            members: HashMap::new(),
            snapshots: HashMap::new(),
            select_all: None,
            format: SelectionFormat::default(),
            window: None,
        }
    }

    pub(crate) fn copy(&self, id: EntityId, cx: &App) -> String {
        let format = self.format;

        if let Some(member) = self
            .members
            .get(&id)
            .and_then(|(_, member)| member.upgrade())
        {
            let member = member.read(cx);

            if member.has_view_selection() {
                return member.selected_text_as(format, None);
            }
        }

        let representative = self
            .snapshots
            .keys()
            .filter_map(|id| self.members.get(id).map(|(index, _)| (*index, *id)))
            .min_by_key(|(index, _)| *index);

        if representative.map(|(_, member)| member) != Some(id) {
            return String::new();
        }

        let Some(snapshot) = self.snapshots.get(&id) else {
            return String::new();
        };

        let anchor = snapshot
            .anchor()
            .entity_id()
            .and_then(|id| self.members.get(&id).map(|(index, _)| *index));

        let cursor = snapshot
            .cursor()
            .entity_id()
            .and_then(|id| self.members.get(&id).map(|(index, _)| *index));

        let last = self.document.blocks.len().saturating_sub(1);

        let range = match (anchor, cursor) {
            (Some(a), Some(b)) => a.min(b)..=a.max(b),
            (Some(at), None) | (None, Some(at)) => match snapshot.coverage() {
                TextSelectionCoverage::FromStart => 0..=at,
                _ => at..=last,
            },
            (None, None) => 0..=last,
        };

        self.document.selected_text(format, Some(range))
    }
}
