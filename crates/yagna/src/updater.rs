//! The app's update worker: UPDATE REPOSITORIES on its own thread, with
//! a channel back to the UI.
//!
//! One sweep at a time — a mutex-guarded flag answers a second start
//! with `false`, never a second thread. The thread opens its own store
//! connection (the UI's handle stays free), drives the same pipeline
//! the `sync` verb runs, and reports each progress line and the final
//! summary through the channel; the UI polls it and paints. A stop
//! request is an `AtomicBool` the pipeline polls at every page
//! boundary: the sweep ends at a boundary, never mid-page, and lands
//! in its own terminal state.
//!
//! The worker also keeps the run's status, so a second click can say
//! how long the run already on has been on — the answer the reader
//! gets instead of a second thread.
//!
//! Items: `UpdateMessage` — what crosses the channel; `UpdateWorker` —
//! the running flag, the status and the stop flag; `try_start` — the
//! one-way-in; `request_stop` — the stop control's half.
//! Depends on: crate::sync (the pipeline), crate::token, crate::ui
//! (the status machine), std (the thread, the channel, the flag).
//! Called by: crate::ui (the menu action, the launch hook, the stop
//! control).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::sync::{self, SweepEnd};
use crate::ui::updatestate::UpdateStatus;

/// What the worker says to the UI: a page's progress line, or the end
/// of the run — the summary line, or the problem.
#[derive(Debug, Clone)]
pub(crate) enum UpdateMessage {
    Progress(String),
    Finished(Result<String, String>),
}

/// The single-flight flag: one sweep at a time, whatever asked.
#[derive(Debug, Default)]
pub(crate) struct UpdateWorker {
    running: Arc<Mutex<bool>>,
    /// The run's status: what the shell paints.
    status: Arc<Mutex<UpdateStatus>>,
    /// The reader's stop request — polled at every page boundary, so a
    /// stop ends the sweep at a boundary, never mid-page.
    stop: Arc<AtomicBool>,
}

impl UpdateWorker {
    /// Whether a sweep is on.
    pub(crate) fn is_running(&self) -> bool {
        *self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The run's status: what the shell paints.
    pub(crate) fn status(&self) -> UpdateStatus {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Asks the running sweep to stop at the next page boundary. A
    /// sweep that is not on (or already stopping) is left alone — the
    /// request is a no-op, never a strike against the next run.
    pub(crate) fn request_stop(&self) {
        let mut slot = self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(&*slot, UpdateStatus::Running { .. }) {
            return;
        }
        let stopping = match slot.clone() {
            UpdateStatus::Running {
                progress,
                started_at,
            } => UpdateStatus::Stopping {
                progress,
                started_at,
            },
            other => other,
        };
        *slot = stopping;
        drop(slot);
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Starts the sweep on the worker thread, unless one is already on:
    /// `false` means the caller answers in the status line. The thread
    /// owns its store connection and reports through `sender`.
    pub(crate) fn try_start(&self, token: String, sender: Sender<UpdateMessage>) -> bool {
        let running = Arc::clone(&self.running);
        let mut flag = running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *flag {
            return false;
        }
        *flag = true;
        drop(flag);
        self.stop.store(false, Ordering::Relaxed);
        let started_at = Instant::now();
        self.set_status(UpdateStatus::running(started_at));
        let running = Arc::clone(&self.running);
        let slot = Arc::clone(&self.status);
        let stop = Arc::clone(&self.stop);
        std::thread::spawn(move || {
            let _guard = FlagGuard {
                running: Arc::clone(&running),
            };
            let progress_sender = sender.clone();
            let progress_slot = Arc::clone(&slot);
            let outcome = sync::perform_stoppable(
                &token,
                false,
                move |line| {
                    let _ = progress_sender.send(UpdateMessage::Progress(line.clone()));
                    keep_progress(
                        &progress_slot,
                        UpdateStatus::Running {
                            progress: Some(line),
                            started_at,
                        },
                    );
                },
                &stop,
                || Ok(()),
            );
            let finished = Instant::now();
            let outcome_status = match &outcome {
                Ok((summary, SweepEnd::Finished)) => UpdateStatus::done(summary.clone(), finished),
                Ok((summary, SweepEnd::Stopped)) => {
                    UpdateStatus::stopped(summary.clone(), finished)
                }
                Err(problem) => UpdateStatus::error(problem.clone(), finished),
            };
            set_status(&slot, outcome_status);
            let message = outcome.map(|(summary, _)| summary);
            let _ = sender.send(UpdateMessage::Finished(message));
        });
        true
    }

    /// Records the run's status.
    fn set_status(&self, status: UpdateStatus) {
        set_status(&self.status, status);
    }
}

/// Writes a running-shaped status into the shared slot only while the
/// run is still plainly running — a stop request that arrived between
/// two progress lines must not be overwritten by a stale "running"
/// line, and a finished run is never resurrected by a late progress.
fn keep_progress(slot: &Arc<Mutex<UpdateStatus>>, status: UpdateStatus) {
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !matches!(*guard, UpdateStatus::Running { .. }) {
        return;
    }
    *guard = status;
}

/// Writes a status into the shared slot, recovering from a poisoned
/// lock: a panic in the pipeline must not take the status with it.
fn set_status(slot: &Arc<Mutex<UpdateStatus>>, status: UpdateStatus) {
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
}

/// Clears the running flag when the worker thread ends — a panic in
/// the pipeline must not leave the flag held forever, refusing every
/// future update with "already running".
struct FlagGuard {
    running: Arc<Mutex<bool>>,
}

impl Drop for FlagGuard {
    fn drop(&mut self) {
        *self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::updatestate::UpdateStatus;
    use std::sync::mpsc;
    use std::time::Duration;

    /// The flag is honest: nothing is running, a test start is refused
    /// when the flag is held, and the flag clears.
    #[test]
    fn the_running_flag_guards_the_second_start() {
        let worker = UpdateWorker::default();
        assert!(!worker.is_running());
        // The flag held by hand: a start is refused, never queued.
        *worker
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        let (sender, receiver) = mpsc::channel();
        assert!(
            !worker.try_start(String::new(), sender),
            "a second sweep is answered with the status line, not a thread"
        );
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(10))
                .is_err()
        );
        *worker
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = false;
        assert!(!worker.is_running());
    }

    /// A second click mid-run yields one worker and one status line:
    /// the start is refused, and the refusal says how long the run
    /// already on has been on.
    #[test]
    fn a_second_click_mid_run_is_refused_with_one_status_line() {
        let worker = UpdateWorker::default();
        // A run that is on: the flag held and the status running.
        *worker
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        worker.set_status(UpdateStatus::running(
            Instant::now() - Duration::from_secs(42),
        ));

        let (sender, receiver) = mpsc::channel();
        assert!(!worker.try_start(String::new(), sender), "no second thread");
        assert!(
            receiver.recv_timeout(Duration::from_millis(10)).is_err(),
            "the refused start says nothing on the channel"
        );

        let line = worker
            .status()
            .already_running_line(Instant::now())
            .expect("the refusal names the running sweep");
        assert_eq!(
            line, "update already running — started 42s ago",
            "one status line, and it names the age"
        );
    }

    /// The worker completes: the status leaves the running state and
    /// keeps the summary.
    #[test]
    fn a_finished_run_leaves_the_running_state() {
        let worker = UpdateWorker::default();
        worker.set_status(UpdateStatus::running(Instant::now()));
        assert!(worker.status().is_running());

        let finished = Instant::now();
        worker.set_status(UpdateStatus::done(
            "2 637 repos, 41 enriched, 12s".to_string(),
            finished,
        ));
        let status = worker.status();
        assert!(!status.is_running(), "the run is over");
        assert_eq!(
            status.line(finished),
            Some("2 637 repos, 41 enriched, 12s".to_string()),
            "the summary stays as evidence"
        );
    }

    /// A stop request moves a running sweep to stopping and arms the
    /// flag the pipeline polls; a sweep that is not plainly running is
    /// left alone — stopping twice changes nothing, and an idle worker
    /// arms nothing.
    #[test]
    fn a_stop_request_arms_the_flag_only_for_a_running_sweep() {
        let worker = UpdateWorker::default();
        worker.request_stop();
        assert!(
            !worker.stop.load(Ordering::Relaxed),
            "an idle worker arms nothing"
        );

        worker.set_status(UpdateStatus::running(Instant::now()));
        worker.request_stop();
        assert!(worker.stop.load(Ordering::Relaxed), "the flag is armed");
        assert!(worker.status().is_stopping(), "the card says stopping…");
        assert!(worker.status().is_running(), "the sweep is still on");

        worker.request_stop();
        assert!(
            worker.status().is_stopping(),
            "a second request changes nothing"
        );
    }
}
