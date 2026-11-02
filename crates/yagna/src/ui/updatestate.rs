//! The update's status: what the sweep is doing, and what it did.
//!
//! One state machine answers every question the shell asks about the
//! update: is a sweep on, what has it done so far, when did it start,
//! and what did it end with. The worker owns the thread; this owns the
//! state, and the shell paints the state rather than the thread.
//!
//! The states are the ones a reader can act on: idle before anything
//! happens, running while the sweep works, stopping while a stop
//! request waits for the page boundary, and three terminal states —
//! done with the summary, stopped with its own line, error with the
//! problem. A finished run keeps its answer until the next one
//! supersedes it — a sweep's result is evidence, not a flash.
//!
//! Items: `UpdateStatus` — the states; `UpdateStatus::running`/
//! `stopping`/`done`/`stopped`/`error` — the transitions; `elapsed` —
//! how long the run has been on; `stoppable` — whether the stop
//! control shows.
//! Depends on: std (the clock).
//! Called by: crate::ui (the update row, the stop control, the menu
//! action) and crate::updater (the worker's transitions).
//! Calls: nothing.

use std::time::{Duration, Instant};

/// What the update is doing, or what it did.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) enum UpdateStatus {
    /// No sweep has run in this session.
    #[default]
    Idle,
    /// A sweep is on: the last progress line it reported, and when it
    /// started.
    Running {
        /// The last progress line the worker reported, when it reported
        /// one.
        progress: Option<String>,
        /// When the sweep started.
        started_at: Instant,
    },
    /// The reader asked the running sweep to stop: the request waits
    /// for the page boundary, and the card says so while it does.
    Stopping {
        /// The last progress line the worker reported.
        progress: Option<String>,
        /// When the sweep started.
        started_at: Instant,
    },
    /// A sweep finished: its summary line, and when it finished.
    Done {
        /// The summary line the worker ended with.
        summary: String,
        /// When the sweep finished.
        finished_at: Instant,
    },
    /// The reader stopped a sweep at a page boundary: its summary line
    /// ("stopped after …"), and when it stopped. A terminal state of
    /// its own — never "cancelled", never left spinning.
    Stopped {
        /// The summary line the worker ended with.
        summary: String,
        /// When the sweep stopped.
        finished_at: Instant,
    },
    /// A sweep failed: the problem, and when it failed.
    Error {
        /// The problem that stopped the sweep.
        message: String,
        /// When the sweep failed.
        finished_at: Instant,
    },
}

impl UpdateStatus {
    /// The state a sweep starts in.
    pub(crate) fn running(started_at: Instant) -> Self {
        UpdateStatus::Running {
            progress: None,
            started_at,
        }
    }

    /// The state a finished sweep leaves in.
    pub(crate) fn done(summary: String, finished_at: Instant) -> Self {
        UpdateStatus::Done {
            summary,
            finished_at,
        }
    }

    /// The state a stopped sweep leaves in.
    pub(crate) fn stopped(summary: String, finished_at: Instant) -> Self {
        UpdateStatus::Stopped {
            summary,
            finished_at,
        }
    }

    /// The state a failed sweep leaves in.
    pub(crate) fn error(message: String, finished_at: Instant) -> Self {
        UpdateStatus::Error {
            message,
            finished_at,
        }
    }

    /// Whether a sweep is on right now — running, or waiting for the
    /// page boundary a stop asked for. A second click is answered with
    /// the age of the run either way.
    pub(crate) fn is_running(&self) -> bool {
        matches!(
            self,
            UpdateStatus::Running { .. } | UpdateStatus::Stopping { .. }
        )
    }

    /// Whether the stop control has anything to stop.
    pub(crate) fn stoppable(&self) -> bool {
        self.is_running()
    }

    /// Whether the sweep is waiting for the page boundary a stop
    /// asked for — the "stopping…" half of the stop control's life.
    pub(crate) fn is_stopping(&self) -> bool {
        matches!(self, UpdateStatus::Stopping { .. })
    }

    /// The progress line the running sweep last reported.
    pub(crate) fn progress(&self) -> Option<&str> {
        match self {
            UpdateStatus::Running { progress, .. } | UpdateStatus::Stopping { progress, .. } => {
                progress.as_deref()
            }
            _ => None,
        }
    }

    /// How long the run has been on, or how long ago it ended: the
    /// moment the run started or ended, measured against now.
    pub(crate) fn elapsed(&self, now: Instant) -> Option<Duration> {
        match self {
            UpdateStatus::Running { started_at, .. }
            | UpdateStatus::Stopping { started_at, .. } => {
                Some(now.saturating_duration_since(*started_at))
            }
            UpdateStatus::Done { finished_at, .. }
            | UpdateStatus::Stopped { finished_at, .. }
            | UpdateStatus::Error { finished_at, .. } => {
                Some(now.saturating_duration_since(*finished_at))
            }
            UpdateStatus::Idle => None,
        }
    }

    /// The line the status row shows: what the sweep is doing, or what
    /// it ended with. An idle update says nothing.
    pub(crate) fn line(&self, now: Instant) -> Option<String> {
        match self {
            UpdateStatus::Idle => None,
            UpdateStatus::Running { progress, .. } => {
                let elapsed = format_elapsed(self.elapsed(now).unwrap_or_default());
                match progress {
                    Some(progress) => Some(format!("{progress} · {elapsed}")),
                    None => Some(format!("updating repositories… · {elapsed}")),
                }
            }
            UpdateStatus::Stopping { progress, .. } => {
                let elapsed = format_elapsed(self.elapsed(now).unwrap_or_default());
                match progress {
                    Some(progress) => Some(format!("stopping… {progress} · {elapsed}")),
                    None => Some(format!("stopping… · {elapsed}")),
                }
            }
            UpdateStatus::Done { summary, .. } => Some(summary.clone()),
            UpdateStatus::Stopped { summary, .. } => Some(summary.clone()),
            UpdateStatus::Error { message, .. } => Some(message.clone()),
        }
    }

    /// The line the "already running" answer shows: how long the run
    /// that is already on has been on.
    pub(crate) fn already_running_line(&self, now: Instant) -> Option<String> {
        if !self.is_running() {
            return None;
        }
        let elapsed = format_elapsed(self.elapsed(now).unwrap_or_default());
        Some(format!("update already running — started {elapsed} ago"))
    }
}

/// A duration as a short, readable span: seconds, then minutes and
/// seconds, then hours and minutes.
pub(crate) fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let minutes = seconds / 60;
    let hours = seconds / 3600;
    match (hours, minutes, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, rest) => format!("{minutes}m {rest}s"),
        (hours, _, _) => format!("{hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run that starts, reports and finishes keeps its answers.
    #[test]
    fn a_run_reports_progress_and_keeps_its_summary() {
        let start = Instant::now();
        let mut status = UpdateStatus::running(start);
        assert!(status.is_running());
        assert_eq!(status.progress(), None);
        assert_eq!(
            status.line(start),
            Some("updating repositories… · 0s".to_string())
        );

        status = UpdateStatus::Running {
            progress: Some("page 3: 120 repos".to_string()),
            started_at: start,
        };
        assert_eq!(status.progress(), Some("page 3: 120 repos"));
        assert_eq!(
            status.line(start),
            Some("page 3: 120 repos · 0s".to_string())
        );

        status = UpdateStatus::done("2 637 repos, 41 enriched, 12s".to_string(), start);
        assert!(!status.is_running());
        assert_eq!(
            status.line(start),
            Some("2 637 repos, 41 enriched, 12s".to_string())
        );
    }

    /// A finished run keeps its answer until the next one supersedes
    /// it: the summary is evidence, not a flash.
    #[test]
    fn a_finished_run_keeps_its_answer() {
        let start = Instant::now();
        let status = UpdateStatus::done("2 637 repos".to_string(), start);
        let later = start + Duration::from_secs(30);
        assert_eq!(status.line(later), Some("2 637 repos".to_string()));
        assert_eq!(status.elapsed(later), Some(Duration::from_secs(30)));
    }

    /// A failed run keeps the problem.
    #[test]
    fn a_failed_run_keeps_the_problem() {
        let start = Instant::now();
        let status = UpdateStatus::error("the token was refused".to_string(), start);
        assert!(!status.is_running());
        assert_eq!(
            status.line(start),
            Some("the token was refused".to_string())
        );
    }

    /// The "already running" answer names how long the run has been on.
    #[test]
    fn the_already_running_answer_names_the_age() {
        let start = Instant::now();
        let status = UpdateStatus::running(start);
        let later = start + Duration::from_secs(42);
        assert_eq!(
            status.already_running_line(later),
            Some("update already running — started 42s ago".to_string())
        );
    }

    /// An idle update has nothing to say — and nothing to refuse.
    #[test]
    fn an_idle_update_says_nothing() {
        let now = Instant::now();
        let status = UpdateStatus::Idle;
        assert_eq!(status.line(now), None);
        assert_eq!(status.already_running_line(now), None);
        assert_eq!(status.elapsed(now), None);
        assert!(!status.is_running());
        assert!(!status.stoppable());
    }

    /// A stop request moves the running state to stopping — the sweep
    /// is still on, the card says so — and the stopped state that
    /// follows is terminal, with its own summary and timestamp.
    #[test]
    fn a_stop_lands_as_its_own_terminal_state() {
        let start = Instant::now();
        let status = UpdateStatus::running(start);
        assert!(status.stoppable(), "the stop control shows while running");

        let stopping = UpdateStatus::Stopping {
            progress: Some("page 12: 500 repos".to_string()),
            started_at: start,
        };
        assert!(stopping.is_running(), "the sweep is on until it stops");
        assert!(stopping.is_stopping());
        assert_eq!(
            stopping.line(start),
            Some("stopping… page 12: 500 repos · 0s".to_string())
        );

        let stopped = UpdateStatus::stopped("stopped after 4m 2s".to_string(), start);
        assert!(!stopped.is_running(), "a stopped sweep is not on");
        assert!(!stopped.stoppable(), "the stop control is gone");
        assert_eq!(stopped.line(start), Some("stopped after 4m 2s".to_string()));
    }

    /// A second click while the sweep waits for its stop is answered
    /// with the age of the run, exactly like a running one.
    #[test]
    fn the_already_running_answer_covers_the_stopping_half_too() {
        let start = Instant::now();
        let stopping = UpdateStatus::Stopping {
            progress: None,
            started_at: start,
        };
        let later = start + Duration::from_secs(42);
        assert_eq!(
            stopping.already_running_line(later),
            Some("update already running — started 42s ago".to_string())
        );
    }

    /// The elapsed span reads in the largest unit that carries weight.
    #[test]
    fn the_elapsed_span_reads_in_the_largest_unit() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(42)), "42s");
        assert_eq!(format_elapsed(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "1h");
    }
}
