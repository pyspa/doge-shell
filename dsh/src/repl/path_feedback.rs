//! A Tab request owns inline feedback, while the shared cache owns readiness.
use crate::completion::generators::system::SystemCommandScanStatus;

#[derive(Default)]
pub(crate) struct PathFeedback {
    request: Option<Request>,
}
struct Request {
    input: String,
    cursor: usize,
    path_generation: u64,
    generation: u64,
    scan: u64,
}
impl PathFeedback {
    pub(crate) fn clear(&mut self) {
        self.request = None;
    }
    pub(crate) fn is_active(&self) -> bool {
        self.request.is_some()
    }
    pub(crate) fn request(
        &mut self,
        input: &str,
        cursor: usize,
        path_generation: u64,
        receipt: SystemCommandScanStatus,
    ) {
        self.request = receipt.pending.map(|scan| Request {
            input: input.to_owned(),
            cursor,
            path_generation,
            generation: receipt.generation,
            scan,
        });
    }
    pub(crate) fn text(
        &mut self,
        input: &str,
        cursor: usize,
        path_generation: u64,
        status: SystemCommandScanStatus,
    ) -> Option<&'static str> {
        let request = self.request.as_ref()?;
        let valid = request.input == input
            && request.cursor == cursor
            && request.path_generation == path_generation
            && request.generation == status.generation
            && request.scan == status.latest_scan;
        let hint = if valid && status.pending == Some(request.scan) {
            Some("PATH scanning…")
        } else if valid && status.published == request.scan && status.complete {
            Some("PATH ready · Tab again")
        } else {
            None
        };
        if hint.is_none() {
            self.clear();
        }
        hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending() -> SystemCommandScanStatus {
        SystemCommandScanStatus {
            generation: 4,
            latest_scan: 9,
            pending: Some(9),
            published: 8,
            complete: true,
        }
    }
    fn feedback() -> PathFeedback {
        let mut feedback = PathFeedback::default();
        feedback.request("zz", 2, 3, pending());
        feedback
    }
    #[test]
    fn path_feedback_empty_success_ready_persists_and_provider_event_is_not_readiness() {
        let mut feedback = feedback();
        assert_eq!(feedback.text("zz", 2, 3, pending()), Some("PATH scanning…"));
        assert_eq!(feedback.text("zz", 2, 3, pending()), Some("PATH scanning…"));
        let ready = SystemCommandScanStatus {
            pending: None,
            published: 9,
            ..pending()
        };
        assert_eq!(
            feedback.text("zz", 2, 3, ready),
            Some("PATH ready · Tab again")
        );
        assert_eq!(
            feedback.text("zz", 2, 3, ready),
            Some("PATH ready · Tab again")
        );
    }
    #[test]
    fn path_feedback_edit_cursor_and_logical_path_generation_invalidate() {
        for (input, cursor, generation) in [("zza", 2, 3), ("zz", 1, 3), ("zz", 2, 4)] {
            let mut feedback = feedback();
            assert_eq!(feedback.text(input, cursor, generation, pending()), None);
            assert_eq!(
                feedback.text("zz", 2, 3, pending()),
                None,
                "invalidation is permanent"
            );
        }
    }
    #[test]
    fn path_feedback_stale_generation_rescan_release_and_incomplete_publication_clear() {
        for invalid in [
            SystemCommandScanStatus {
                generation: 5,
                ..pending()
            },
            SystemCommandScanStatus {
                latest_scan: 10,
                pending: Some(10),
                ..pending()
            },
            SystemCommandScanStatus {
                pending: None,
                ..pending()
            },
            SystemCommandScanStatus {
                pending: None,
                published: 9,
                complete: false,
                ..pending()
            },
        ] {
            let mut feedback = feedback();
            assert_eq!(feedback.text("zz", 2, 3, invalid), None);
            assert_eq!(feedback.text("zz", 2, 3, pending()), None);
        }
    }
    #[test]
    fn path_feedback_cancel_and_nonpending_receipt_cannot_arm() {
        let mut feedback = feedback();
        feedback.clear();
        assert_eq!(feedback.text("zz", 2, 3, pending()), None);
        feedback.request(
            "zz",
            2,
            3,
            SystemCommandScanStatus {
                pending: None,
                ..pending()
            },
        );
        assert!(!feedback.is_active());
    }
}
