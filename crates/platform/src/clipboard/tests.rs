use std::error::Error;
use std::io;

use copypasta::ClipboardProvider;

use crate::clipboard::{Clipboard, ClipboardType};

struct MemoryClipboard(String);

impl ClipboardProvider for MemoryClipboard {
    fn get_contents(&mut self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok(self.0.clone())
    }

    fn set_contents(&mut self, value: String) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.0 = value;
        Ok(())
    }
}

struct UnavailableClipboard;

impl ClipboardProvider for UnavailableClipboard {
    fn get_contents(&mut self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Err(io::Error::other("clipboard unavailable").into())
    }

    fn set_contents(&mut self, _: String) -> Result<(), Box<dyn Error + Send + Sync>> {
        Err(io::Error::other("clipboard unavailable").into())
    }
}

#[test]
fn selection_and_clipboard_keep_separate_contents() {
    let mut clipboard = Clipboard {
        clipboard: Box::new(MemoryClipboard(String::new())),
        selection: Some(Box::new(MemoryClipboard(String::new()))),
    };
    assert!(clipboard.set(ClipboardType::Clipboard, "copied"));
    assert!(clipboard.set(ClipboardType::Selection, "selected"));
    assert_eq!(clipboard.get(ClipboardType::Clipboard), "copied");
    assert_eq!(clipboard.get(ClipboardType::Selection), "selected");
}

#[test]
fn missing_selection_reads_clipboard_without_accepting_selection_writes() {
    let mut clipboard = Clipboard {
        clipboard: Box::new(MemoryClipboard("copied".into())),
        selection: None,
    };
    assert!(!clipboard.set(ClipboardType::Selection, "selected"));
    assert_eq!(clipboard.get(ClipboardType::Selection), "copied");
    assert_eq!(clipboard.get(ClipboardType::Clipboard), "copied");
}

#[test]
fn provider_failure_returns_rejected_write_and_empty_read() {
    let mut clipboard = Clipboard {
        clipboard: Box::new(UnavailableClipboard),
        selection: None,
    };
    assert!(!clipboard.set(ClipboardType::Clipboard, "copied"));
    assert!(clipboard.get(ClipboardType::Clipboard).is_empty());
}
