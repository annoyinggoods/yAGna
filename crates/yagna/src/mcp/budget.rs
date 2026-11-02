//! The spend policy and the session's spend ledger: what the agent may
//! spend, counted, confirmed at the threshold, refused at the ceilings.
//!
//! The policy file (`<data>/agent.toml`) carries the three ceilings;
//! an absent file is the default policy and silence, a broken one is
//! the default policy plus one loud line — never a silent overspend.
//! The ledger counts every live call the session makes; the call that
//! would take the session past `confirm_above` is a host question
//! first (answered yes or no), and any call whose window ceiling is
//! full is refused with a message naming the ceiling and the file that
//! raises it.
//!
//! Items: `SpendPolicy` — the three ceilings; `SpendKind` — the two
//! counted call kinds; `Admission` — what a call got; `Budget` — the
//! session's ledger.
//! Core functions: `load_policy` — the policy file on the settings'
//! contract; `Budget::admit` — charge or ask or refuse.
//! Depends on: chrono (the windows), toml (the policy file).
//! Called by: crate::mcp::server (every live tool call).

use std::collections::VecDeque;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};

/// The policy's three ceilings, as the file names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpendPolicy {
    /// The core API calls one session may make per hour.
    pub core_per_hour: u64,
    /// The search API calls one session may make per minute.
    pub search_per_minute: u64,
    /// The session's live-call count past which the host is asked.
    pub confirm_above: u64,
}

impl Default for SpendPolicy {
    fn default() -> Self {
        Self {
            core_per_hour: 500,
            search_per_minute: 10,
            confirm_above: 20,
        }
    }
}

/// The kinds of live call the ledger counts: the core API's budget and
/// the search API's own, separate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendKind {
    Core,
    Search,
}

/// What happened to one live call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// The call may proceed and is charged.
    Admitted,
    /// The session is about to cross `confirm_above`: ask the host.
    Confirm { message: String },
    /// The call is refused — a ceiling is full, or the host said no.
    Refused { message: String },
}

/// The session's spend ledger: the counted calls and the threshold's
/// state.
#[derive(Debug)]
pub struct Budget {
    policy: SpendPolicy,
    /// The file that raises the ceilings — every refusal names it.
    policy_path: std::path::PathBuf,
    /// The counted calls inside the windows, oldest first.
    calls: VecDeque<DateTime<Utc>>,
    kinds: VecDeque<SpendKind>,
    /// Every call the session made, windows aside.
    session_total: u64,
    /// What the host said about spending past the threshold.
    crossing: Crossing,
}

/// The threshold's host-side state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crossing {
    Unasked,
    Approved,
    Declined,
}

/// The policy file's parse shape: only the `[spend]` table matters, and
/// every absent key falls back to the default ceiling.
#[derive(Debug, Default, serde::Deserialize)]
struct PolicyFile {
    spend: Option<SpendTable>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct SpendTable {
    core_per_hour: Option<u64>,
    search_per_minute: Option<u64>,
    confirm_above: Option<u64>,
}

/// The policy file, on the settings' contract: an absent file is the
/// default policy and silence, a broken one is the default policy plus
/// one loud line naming the file.
pub fn load_policy(path: &Path) -> (SpendPolicy, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (SpendPolicy::default(), None);
    };
    match toml::from_str::<PolicyFile>(&text) {
        Ok(file) => (policy_from(file), None),
        Err(error) => (
            SpendPolicy::default(),
            Some(format!(
                "agent policy file {}: {error} — running on the default policy",
                path.display()
            )),
        ),
    }
}

fn policy_from(file: PolicyFile) -> SpendPolicy {
    let defaults = SpendPolicy::default();
    let spend = file.spend.unwrap_or_default();
    SpendPolicy {
        core_per_hour: spend.core_per_hour.unwrap_or(defaults.core_per_hour),
        search_per_minute: spend
            .search_per_minute
            .unwrap_or(defaults.search_per_minute),
        confirm_above: spend.confirm_above.unwrap_or(defaults.confirm_above),
    }
}

impl Budget {
    /// A ledger for one session against one policy and the file that
    /// raises it.
    pub fn new(policy: SpendPolicy, policy_path: std::path::PathBuf) -> Self {
        Self {
            policy,
            policy_path,
            calls: VecDeque::new(),
            kinds: VecDeque::new(),
            session_total: 0,
            crossing: Crossing::Unasked,
        }
    }

    /// One live call against the ledger: refused at a full window
    /// ceiling or after a declined crossing, a host question at the
    /// threshold, and charged when it proceeds.
    pub fn admit(&mut self, kind: SpendKind, now: DateTime<Utc>) -> Admission {
        self.prune(now);
        if let Some(message) = self.ceiling_message(kind, now) {
            return Admission::Refused { message };
        }
        if self.crossing == Crossing::Declined && self.past_threshold() {
            return Admission::Refused {
                message: self.declined_message(),
            };
        }
        if self.crossing == Crossing::Unasked && self.past_threshold() {
            return Admission::Confirm {
                message: self.confirm_message(),
            };
        }
        self.charge(kind, now);
        Admission::Admitted
    }

    /// The host answered the threshold question with yes: the session
    /// may spend past `confirm_above`, bounded by the ceilings.
    pub fn approve_crossing(&mut self) {
        self.crossing = Crossing::Approved;
    }

    /// The host answered the threshold question with no: every call
    /// past the threshold is refused from here on.
    pub fn decline_crossing(&mut self) {
        self.crossing = Crossing::Declined;
    }

    /// Every live call the session has made, windows aside — the
    /// ledger's accounting, pinned by the tests.
    #[cfg(test)]
    pub fn session_total(&self) -> u64 {
        self.session_total
    }

    /// Whether this call would take the session past the threshold.
    fn past_threshold(&self) -> bool {
        self.session_total >= self.policy.confirm_above
    }

    fn charge(&mut self, kind: SpendKind, now: DateTime<Utc>) {
        self.calls.push_back(now);
        self.kinds.push_back(kind);
        self.session_total += 1;
    }

    /// Drops the calls older than the widest window — the hour the
    /// core budget runs on. Anything older cannot be inside a window.
    fn prune(&mut self, now: DateTime<Utc>) {
        let horizon = now - Duration::hours(1);
        while self.calls.front().is_some_and(|at| *at < horizon) {
            self.calls.pop_front();
            self.kinds.pop_front();
        }
    }

    /// The window ceiling this call would break, named with the file
    /// that raises it — or nothing when the window has room.
    fn ceiling_message(&self, kind: SpendKind, now: DateTime<Utc>) -> Option<String> {
        match kind {
            SpendKind::Core => self.window_message(
                self.core_in_window(now),
                self.policy.core_per_hour,
                "core_per_hour",
            ),
            SpendKind::Search => self.window_message(
                self.search_in_window(now),
                self.policy.search_per_minute,
                "search_per_minute",
            ),
        }
    }

    fn window_message(&self, used: u64, ceiling: u64, name: &str) -> Option<String> {
        if used < ceiling {
            return None;
        }
        Some(format!(
            "the session's {name} ceiling ({ceiling}) is reached — raise it in {} for the next session",
            self.policy_path.display()
        ))
    }

    fn core_in_window(&self, now: DateTime<Utc>) -> u64 {
        self.count_in_window(SpendKind::Core, now - Duration::hours(1))
    }

    fn search_in_window(&self, now: DateTime<Utc>) -> u64 {
        self.count_in_window(SpendKind::Search, now - Duration::minutes(1))
    }

    fn count_in_window(&self, kind: SpendKind, since: DateTime<Utc>) -> u64 {
        let mut count = 0;
        for (index, at) in self.calls.iter().enumerate() {
            if *at >= since && self.kinds[index] == kind {
                count += 1;
            }
        }
        count
    }

    /// The host question at the threshold: the count, the ceiling, and
    /// the file that raises it.
    fn confirm_message(&self) -> String {
        format!(
            "the session has spent {} live API calls — a host confirmation is needed to spend past confirm_above ({})",
            self.session_total, self.policy.confirm_above
        )
    }

    /// The refusal after a declined question — the ceiling and how to
    /// raise it.
    pub fn declined_message(&self) -> String {
        format!(
            "the host declined spending past confirm_above ({}) — raise it in {} for the next session",
            self.policy.confirm_above,
            self.policy_path.display()
        )
    }

    /// The refusal when the host cannot be asked at all — the ceiling
    /// and how to raise it, and never a silent yes.
    pub fn unaskable_message(&self) -> String {
        format!(
            "this client cannot be asked, and spending past confirm_above ({}) is refused — raise it in {} for the next session",
            self.policy.confirm_above,
            self.policy_path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: u64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_762_000_000 + second as i64, 0).expect("timestamp")
    }

    fn budget(core_per_hour: u64, search_per_minute: u64, confirm_above: u64) -> Budget {
        Budget::new(
            SpendPolicy {
                core_per_hour,
                search_per_minute,
                confirm_above,
            },
            std::path::PathBuf::from("agent.toml"),
        )
    }

    #[test]
    fn an_absent_policy_file_is_the_default_policy_and_silence() {
        let path = std::env::temp_dir().join("yagna-no-such-agent.toml");
        let (policy, line) = load_policy(&path);
        assert_eq!(policy, SpendPolicy::default());
        assert_eq!(line, None);
    }

    #[test]
    fn a_broken_policy_file_is_the_default_policy_and_a_loud_line() {
        let dir = std::env::temp_dir().join(format!("yagna-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("agent.toml");
        std::fs::write(&path, "[spend\ncore_per_hour = = 5").expect("write");
        let (policy, line) = load_policy(&path);
        assert_eq!(policy, SpendPolicy::default());
        let line = line.expect("loud");
        assert!(
            line.contains("agent.toml"),
            "the line names the file: {line}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spending_below_the_threshold_is_just_charged() {
        let mut ledger = budget(500, 10, 3);
        for second in 0..3 {
            assert_eq!(
                ledger.admit(SpendKind::Core, at(second)),
                Admission::Admitted
            );
        }
        assert_eq!(ledger.session_total(), 3);
    }

    #[test]
    fn the_call_that_would_cross_the_threshold_asks_first() {
        let mut ledger = budget(500, 10, 2);
        assert_eq!(ledger.admit(SpendKind::Core, at(0)), Admission::Admitted);
        assert_eq!(ledger.admit(SpendKind::Core, at(1)), Admission::Admitted);
        let question = ledger.admit(SpendKind::Core, at(2));
        let Admission::Confirm { message } = question else {
            panic!("the third call asks, got {question:?}");
        };
        assert!(
            message.contains("confirm_above"),
            "the message names the ceiling: {message}"
        );
        assert_eq!(ledger.session_total(), 2, "the asking call is not charged");
        ledger.approve_crossing();
        assert_eq!(ledger.admit(SpendKind::Core, at(3)), Admission::Admitted);
        assert_eq!(
            ledger.admit(SpendKind::Core, at(4)),
            Admission::Admitted,
            "one yes covers the session's tail"
        );
    }

    #[test]
    fn a_declined_crossing_refuses_the_call_and_names_the_file() {
        let mut ledger = budget(500, 10, 1);
        assert_eq!(ledger.admit(SpendKind::Core, at(0)), Admission::Admitted);
        let question = ledger.admit(SpendKind::Core, at(1));
        assert!(matches!(question, Admission::Confirm { .. }));
        ledger.decline_crossing();
        let refused = ledger.admit(SpendKind::Core, at(2));
        let Admission::Refused { message } = refused else {
            panic!("declined means refused, got {refused:?}");
        };
        assert!(
            message.contains("agent.toml"),
            "the message names the file: {message}"
        );
        assert!(
            message.contains("confirm_above"),
            "and the ceiling: {message}"
        );
    }

    #[test]
    fn the_core_hour_ceiling_refuses_and_names_the_ceiling() {
        let mut ledger = budget(2, 10, 100);
        assert_eq!(ledger.admit(SpendKind::Core, at(0)), Admission::Admitted);
        assert_eq!(ledger.admit(SpendKind::Core, at(1)), Admission::Admitted);
        let refused = ledger.admit(SpendKind::Core, at(2));
        let Admission::Refused { message } = refused else {
            panic!("the ceiling refuses, got {refused:?}");
        };
        assert!(
            message.contains("core_per_hour"),
            "the message names the ceiling: {message}"
        );
        assert!(message.contains("agent.toml"), "and the file: {message}");
        // The window slides: past the hour the calls are old news.
        assert_eq!(
            ledger.admit(SpendKind::Core, at(3700)),
            Admission::Admitted,
            "an hour later the window has room again"
        );
    }

    #[test]
    fn the_search_minute_ceiling_refuses_and_slides() {
        let mut ledger = budget(500, 1, 100);
        assert_eq!(ledger.admit(SpendKind::Search, at(0)), Admission::Admitted);
        let refused = ledger.admit(SpendKind::Search, at(1));
        let Admission::Refused { message } = refused else {
            panic!("the search ceiling refuses, got {refused:?}");
        };
        assert!(
            message.contains("search_per_minute"),
            "the message names it: {message}"
        );
        assert_eq!(
            ledger.admit(SpendKind::Core, at(1)),
            Admission::Admitted,
            "the core window is a separate budget"
        );
        assert_eq!(
            ledger.admit(SpendKind::Search, at(61)),
            Admission::Admitted,
            "a minute later the window has room again"
        );
    }
}
