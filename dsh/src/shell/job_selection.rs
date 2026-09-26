//! Active-job current/previous selection authority.
//!
//! The active job table order is the single authority for `%`/`%+`/`%%`
//! (current), `%-` (previous), and the `jobs` `+`/`-` markers. Resolvers,
//! renderers, and completion all derive from [`ActiveJobSelection`] so they
//! cannot drift apart.
//!
//! REPL completed-job notice batch markers are presentation history and must
//! not be used as the authority for active-table current/previous selection.

/// Current/previous selection over the active job table, by table index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveJobSelection {
    current: Option<usize>,
    previous: Option<usize>,
}

impl ActiveJobSelection {
    /// Derive selection from the active table length.
    ///
    /// - 0 jobs: no current, no previous.
    /// - 1 job: current and previous both resolve to that job.
    /// - 2+ jobs: current is the last index, previous the second-last.
    pub(crate) fn for_len(len: usize) -> Self {
        match len {
            0 => Self {
                current: None,
                previous: None,
            },
            1 => Self {
                current: Some(0),
                previous: Some(0),
            },
            _ => Self {
                current: Some(len - 1),
                previous: Some(len - 2),
            },
        }
    }

    pub(crate) fn current(self) -> Option<usize> {
        self.current
    }

    pub(crate) fn previous(self) -> Option<usize> {
        self.previous
    }

    /// Marker role for a full-table index. Current wins when both coincide
    /// (single job displays `+`, never `-`).
    pub(crate) fn marker_for(self, index: usize) -> ActiveJobMarker {
        if self.current == Some(index) {
            ActiveJobMarker::Current
        } else if self.previous == Some(index) {
            ActiveJobMarker::Previous
        } else {
            ActiveJobMarker::None
        }
    }
}

/// Display role for one active-table entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveJobMarker {
    Current,
    Previous,
    None,
}

impl ActiveJobMarker {
    pub(crate) fn glyph(self) -> char {
        match self {
            Self::Current => '+',
            Self::Previous => '-',
            Self::None => ' ',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_job_selection_empty() {
        let selection = ActiveJobSelection::for_len(0);
        assert_eq!(selection.current(), None);
        assert_eq!(selection.previous(), None);
    }

    #[test]
    fn active_job_selection_single_job_is_current_and_previous() {
        let selection = ActiveJobSelection::for_len(1);
        assert_eq!(selection.current(), Some(0));
        assert_eq!(selection.previous(), Some(0));
        assert_eq!(selection.marker_for(0), ActiveJobMarker::Current);
    }

    #[test]
    fn active_job_selection_two_jobs() {
        let selection = ActiveJobSelection::for_len(2);
        assert_eq!(selection.current(), Some(1));
        assert_eq!(selection.previous(), Some(0));
        assert_eq!(selection.marker_for(0), ActiveJobMarker::Previous);
        assert_eq!(selection.marker_for(1), ActiveJobMarker::Current);
    }

    #[test]
    fn active_job_selection_three_jobs() {
        let selection = ActiveJobSelection::for_len(3);
        assert_eq!(selection.current(), Some(2));
        assert_eq!(selection.previous(), Some(1));
        assert_eq!(selection.marker_for(0), ActiveJobMarker::None);
        assert_eq!(selection.marker_for(1), ActiveJobMarker::Previous);
        assert_eq!(selection.marker_for(2), ActiveJobMarker::Current);
    }
}
