//! Session-scoped system clipboard access.

use std::cell::RefCell;

const CLIPBOARD_INIT_ERROR: &str =
    "Could not access the system clipboard. Check clipboard access and the desktop session.";
const CLIPBOARD_WRITE_ERROR: &str =
    "Could not write to the system clipboard. Check clipboard access and the desktop session.";

type SystemClipboardInitializer = fn() -> Result<arboard::Clipboard, String>;

pub(crate) trait ClipboardBackend {
    fn set_text(&mut self, text: &str) -> Result<(), String>;
}

impl ClipboardBackend for arboard::Clipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        arboard::Clipboard::set_text(self, text).map_err(|error| error.to_string())
    }
}

fn initialize_system_clipboard() -> Result<arboard::Clipboard, String> {
    arboard::Clipboard::new().map_err(|error| error.to_string())
}

/// Lazily initializes and retains the clipboard owner for one control-station run.
pub(crate) struct ClipboardService<
    Backend = arboard::Clipboard,
    Initialize = SystemClipboardInitializer,
> {
    clipboard: RefCell<Option<Backend>>,
    initialize: RefCell<Initialize>,
}

impl ClipboardService {
    pub(crate) fn new() -> Self {
        Self {
            clipboard: RefCell::new(None),
            initialize: RefCell::new(initialize_system_clipboard),
        }
    }
}

impl<Backend, Initialize> ClipboardService<Backend, Initialize>
where
    Backend: ClipboardBackend,
    Initialize: FnMut() -> Result<Backend, String>,
{
    #[cfg(test)]
    fn with_initializer(initialize: Initialize) -> Self {
        Self {
            clipboard: RefCell::new(None),
            initialize: RefCell::new(initialize),
        }
    }

    pub(crate) fn copy_text(&self, text: &str) -> Result<(), String> {
        let mut clipboard = self.clipboard.borrow_mut();
        if clipboard.is_none() {
            let initialized =
                (self.initialize.borrow_mut())().map_err(|_| CLIPBOARD_INIT_ERROR.to_string())?;
            *clipboard = Some(initialized);
        }

        clipboard
            .as_mut()
            .expect("clipboard was initialized above")
            .set_text(text)
            .map_err(|_| CLIPBOARD_WRITE_ERROR.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    struct RecordingClipboard {
        writes: Rc<RefCell<Vec<String>>>,
        error: Option<String>,
    }

    impl ClipboardBackend for RecordingClipboard {
        fn set_text(&mut self, text: &str) -> Result<(), String> {
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            self.writes.borrow_mut().push(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn initialization_is_lazy_retryable_and_reused() {
        let attempts = Rc::new(RefCell::new(0));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let service = ClipboardService::with_initializer({
            let attempts = Rc::clone(&attempts);
            let writes = Rc::clone(&writes);
            move || {
                *attempts.borrow_mut() += 1;
                if *attempts.borrow() == 1 {
                    return Err("temporary initialization failure".into());
                }
                Ok(RecordingClipboard {
                    writes: Rc::clone(&writes),
                    error: None,
                })
            }
        });

        assert_eq!(*attempts.borrow(), 0, "construction must be headless");
        assert!(service.copy_text("first-attempt").is_err());
        assert_eq!(*attempts.borrow(), 1);

        service.copy_text("opaque-payload").expect("retry succeeds");
        service.copy_text("replacement").expect("backend is reused");

        assert_eq!(*attempts.borrow(), 2);
        assert_eq!(
            writes.borrow().as_slice(),
            ["opaque-payload", "replacement"]
        );
    }

    #[test]
    fn backend_error_and_notice_do_not_expose_secret() {
        let secret = "plasm-sentinel-secret";
        let service = ClipboardService::with_initializer(|| {
            Ok(RecordingClipboard {
                writes: Rc::new(RefCell::new(Vec::new())),
                error: Some(format!("backend rejected {secret}")),
            })
        });

        let error = service.copy_text(secret).expect_err("copy must fail");
        assert!(!error.contains(secret));

        let notice = super::super::copy_notice("copied", "copy failed", Err(error));
        assert!(!notice.title.contains(secret));
        assert!(!notice.summary.contains(secret));
        assert!(notice.details.iter().all(|line| !line.contains(secret)));
        assert!(notice
            .action_hint
            .as_ref()
            .is_none_or(|hint| !hint.contains(secret)));
    }
}
