//! The cold-start instrumentation: the launch path's four marks — the
//! store open, the first page query, the fuzzy index build, and the
//! first frame — and the report `--mark-startup` prints.
//!
//! The gates are the roadmap's: the first frame under two seconds, the
//! index under half a second, the first page query under fifty
//! milliseconds. The instrumented run prints every mark with its gate,
//! so a landing that touches the startup path can see whether it
//! stayed inside.
//!
//! Items: `Startup` — the collector, shared between the launch and
//! the pane; `StartupMarks` — the four marks; `FIRST_FRAME_GATE`,
//! `INDEX_GATE`, `PAGE_GATE` — the gates; `report` — the printed
//! lines.
//! Depends on: std::time (the clock), std::sync (the shared marks).
//! Called by: crate::ui (the launch path and the pane's first
//! render).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The gate the first frame must stay inside.
pub(crate) const FIRST_FRAME_GATE: Duration = Duration::from_millis(2000);

/// The gate the fuzzy index build must stay inside.
pub(crate) const INDEX_GATE: Duration = Duration::from_millis(500);

/// The gate the first page query must stay inside.
pub(crate) const PAGE_GATE: Duration = Duration::from_millis(50);

/// The marks one launch collected: each is the time since the launch
/// started.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StartupMarks {
    pub store: Option<Duration>,
    pub page: Option<Duration>,
    pub index: Option<Duration>,
    pub frame: Option<Duration>,
}

/// The collector: the marks, the launch's own clock, and the latch
/// that keeps the report to one printing.
#[derive(Debug)]
pub(crate) struct Startup {
    marks: Mutex<StartupMarks>,
    origin: Instant,
    printed: AtomicBool,
}

impl Startup {
    /// A collector starting now — the launch's clock.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            marks: Mutex::new(StartupMarks::default()),
            origin: Instant::now(),
            printed: AtomicBool::new(false),
        })
    }

    /// Records one mark measured from the launch's start to `at` — the
    /// store open and the first frame.
    pub(crate) fn mark_since_start(&self, slot: Slot, at: Instant) {
        let elapsed = at.saturating_duration_since(self.origin);
        self.mark_duration(slot, elapsed);
    }

    /// Records one mark measured on its own — the first page query's
    /// duration and the fuzzy index build's.
    pub(crate) fn mark_duration(&self, slot: Slot, elapsed: Duration) {
        let mut marks = self.marks.lock().unwrap_or_else(|error| error.into_inner());
        match slot {
            Slot::Store => marks.store = Some(elapsed),
            Slot::Page => marks.page = Some(elapsed),
            Slot::Index => marks.index = Some(elapsed),
            Slot::Frame => marks.frame = Some(elapsed),
        }
    }

    /// The marks as they stand.
    pub(crate) fn marks(&self) -> StartupMarks {
        *self.marks.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Prints the report — once. The lines carry each mark and its
    /// gate, and say whether the gate held.
    pub(crate) fn report(&self) {
        if self.printed.swap(true, Ordering::Relaxed) {
            return;
        }
        let marks = self.marks();
        println!("startup: store open {}", line(marks.store, None));
        println!(
            "startup: first page query {}",
            line(marks.page, Some(PAGE_GATE))
        );
        println!(
            "startup: fuzzy index {}",
            line(marks.index, Some(INDEX_GATE))
        );
        println!(
            "startup: first frame {}",
            line(marks.frame, Some(FIRST_FRAME_GATE))
        );
    }
}

/// Which mark is being recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Store,
    Page,
    Index,
    Frame,
}

/// One mark's line: the time, and whether it stayed inside its gate.
fn line(mark: Option<Duration>, gate: Option<Duration>) -> String {
    let Some(mark) = mark else {
        return "not reached".to_string();
    };
    let millis = mark.as_secs_f64() * 1000.0;
    match gate {
        None => format!("{millis:.0} ms"),
        Some(gate) => format!(
            "{millis:.0} ms (gate {} ms — {})",
            gate.as_millis(),
            if mark <= gate { "inside" } else { "OVER" },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_is_the_time_since_the_launch_started() {
        let startup = Startup::new();
        startup.mark_since_start(Slot::Frame, Instant::now());
        let marks = startup.marks();
        let frame = marks.frame.expect("the frame was marked");
        assert!(frame < Duration::from_secs(1), "the frame was quick");
    }

    #[test]
    fn a_measured_duration_is_recorded_as_it_is() {
        let startup = Startup::new();
        startup.mark_duration(Slot::Page, Duration::from_millis(12));
        assert_eq!(
            startup.marks().page,
            Some(Duration::from_millis(12)),
            "the page query's own duration"
        );
    }

    #[test]
    fn the_lines_say_whether_the_gate_held() {
        assert_eq!(line(None, Some(PAGE_GATE)), "not reached");
        assert!(
            line(Some(Duration::from_millis(10)), Some(PAGE_GATE)).contains("inside"),
            "a quick mark is inside"
        );
        assert!(
            line(Some(Duration::from_millis(80)), Some(PAGE_GATE)).contains("OVER"),
            "a slow mark is over"
        );
    }
}
