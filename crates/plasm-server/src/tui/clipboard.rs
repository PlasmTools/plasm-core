//! Session-scoped system clipboard access.

use std::cell::RefCell;

const CLIPBOARD_INIT_ERROR: &str =
    "Could not access the system clipboard. Check clipboard access and the desktop session.";
const CLIPBOARD_WRITE_ERROR: &str =
    "Could not write to the system clipboard. Check clipboard access and the desktop session.";

/// Lazily initializes and retains the clipboard owner for one control-station run.
pub(crate) struct ClipboardService {
    clipboard: RefCell<Option<arboard::Clipboard>>,
}

impl ClipboardService {
    pub(crate) fn new() -> Self {
        Self {
            clipboard: RefCell::new(None),
        }
    }

    pub(crate) fn copy_text(&self, text: &str) -> Result<(), String> {
        let mut clipboard = self.clipboard.borrow_mut();
        if clipboard.is_none() {
            let initialized =
                arboard::Clipboard::new().map_err(|_| CLIPBOARD_INIT_ERROR.to_string())?;
            *clipboard = Some(initialized);
        }

        clipboard
            .as_mut()
            .expect("clipboard was initialized above")
            .set_text(text)
            .map_err(|_| CLIPBOARD_WRITE_ERROR.to_string())
    }
}
