//! # T9 — Fair-share quota scheduler.
//!
//! Replaces the "first engine to succeed soft-stops the rest" model with a
//! work-conserving, rate-capped dispatcher:
//!
//! * **Fair share** — every working engine gets the same dispatch treatment:
//!   an equal per-engine **in-flight cap** (the rate cap) and round-robin
//!   ordering. No single engine may hog the workers.
//! * **Global target** — dispatch stops once `target` links have been created
//!   (the user's "N backlinks" request). Jobs bound to engines that are
//!   cooling down or that failed simply stay queued; healthy engines absorb
//!   the load, so the run still approaches the target instead of stalling.
//! * **429/403 backoff** — when an engine answers with a rate-limit /
//!   captcha / "too many requests" signal, it is put on an exponential
//!   cooldown and its in-flight slots are released so other engines take the
//!   work. A clean success resets the streak.
//!
//! The controller is a small, lock-protected state object shared (via
//! `Arc<Mutex<FairState>>`) between the pool leader (which calls
//! [`FairState::admit`] before dispatching each job) and every worker (which
//! calls [`FairState::complete`] / [`FairState::release`] when a job it was
//! handed finishes). It is pure bookkeeping — no I/O — so it unit-tests in
//! isolation.
//!
//! ## Fairness vs. completion
//!
//! A *strict* even split (`target / N` per engine) guarantees no engine
//! dominates but makes the total reachable depend on the weakest engine. To
//! satisfy *both* "use all engines" and "deliver the full target", we cap the
//! per-engine share with a bounded-skew `max_per_engine` (default unbounded)
//! and rely on the global target gate + equal in-flight cap for fairness.
//! Over-provisioning the job queue (see the API's `run_auto_pool`) supplies
//! enough head-room that losing one engine still reaches the target.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// T10c — an engine that produces this many consecutive non-success
/// outcomes (of *any* class) is blacklisted for the run: it is presumed
/// dead or permanently blocked, so `admit()` stops offering it work and
/// [`FairState::engines_exhausted`] can go true. This is what lets a
/// replenished run TERMINATE (every engine out) instead of re-queueing a
/// dead engine forever. A success resets the streak (the engine recovered).
pub const BLACKLIST_AFTER: u32 = 3;

/// Per-run scheduling policy for the fair-share controller.
#[derive(Debug, Clone, Copy)]
pub struct FairPolicy {
    /// Global link target. `0` = unbounded (dispatch the whole queue). This is
    /// the user's "request N backlinks" number.
    pub target: usize,
    /// Rate cap: maximum jobs per engine that may be in-flight (dispatched,
    /// not yet finished) at once. Bounds how hard one engine can hammer so it
    /// cannot out-run the others. `0` = no per-engine in-flight cap.
    pub max_inflight_per_engine: u32,
    /// Hard per-engine link cap for this run. `0` = unbounded (overflow is
    /// allowed — healthy engines absorb a failed engine's share). Set to a
    /// small multiple of `target / N` to bound how far one engine may skew.
    pub max_per_engine: usize,
    /// Backoff base applied on the first rate-limit hit, and the ceiling after
    /// repeated consecutive hits (`base * 2^streak`, clamped to `max_backoff`).
    pub base_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for FairPolicy {
    fn default() -> Self {
        Self {
            target: 0,
            max_inflight_per_engine: 2,
            max_per_engine: 0,
            base_backoff: Duration::from_secs(20),
            max_backoff: Duration::from_secs(300),
        }
    }
}

/// Classify a finished job so the controller knows which reaction to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeClass {
    /// The engine submitted at least one link (counted toward the target).
    Success,
    /// The engine got "hot" (429/406/403, captcha, "too many requests") —
    /// put it on cooldown and let other engines take the work.
    RateLimited,
    /// Any other failure (transport, parse, form error, timeout) — no
    /// engine-level penalty; transport problems are the proxy's fault, not the
    /// engine's rate.
    Other,
}

#[derive(Debug, Clone, Default)]
struct EngineFair {
    /// Jobs dispatched for this engine that have not yet completed.
    inflight: u32,
    /// Links created by this engine this run.
    links: usize,
    /// Consecutive rate-limit hits (drives the exponential backoff). Reset to
    /// zero on a success.
    rate_streak: u32,
    /// Earliest instant this engine may be dispatched again (if set and in the
    /// future, the engine is cooling).
    cool_until: Option<Instant>,
    // ── diagnostics ──
    /// Total rate-limit hits (for the per-engine rollup).
    rate_hits: u32,
    /// Total admits granted (for the per-engine rollup).
    admitted: u32,
    /// T10c — consecutive non-success outcomes (any class). `Success`
    /// resets it to 0; `>= BLACKLIST_AFTER` flips `disabled`.
    fail_streak: u32,
    /// T10c — blacklisted for this run: `admit()` refuses it.
    disabled: bool,
}

/// One engine's share of a fair share (the even split the policy would give it).
#[inline]
pub fn fair_quota(target: usize, engines: usize) -> usize {
    if engines == 0 {
        0
    } else {
        target.div_ceil(engines)
    }
}

/// Exponential backoff: `base * 2^(streak-1)`, clamped to `max`. `streak >= 1`.
/// A pure free function (no receiver) so it can be called inside
/// [`FairState::complete`] while the `engines` map entry is mutably held —
/// no `self` borrow crosses the `engines` borrow.
pub fn backoff_delay(base: Duration, max: Duration, streak: u32) -> Duration {
    let mut d = base;
    for _ in 0..streak.saturating_sub(1) {
        d = d.saturating_mul(2);
        if d >= max {
            return max;
        }
    }
    d.min(max)
}

/// Shared state for one pool run. Guarded by a `std::sync::Mutex` and held only
/// for brief bookkeeping, so leader + all workers can touch it across tokio
/// tasks.
#[derive(Debug)]
pub struct FairState {
    policy: FairPolicy,
    engines: HashMap<Uuid, EngineFair>,
    total_links: usize,
    // diagnostics — why admits were rejected
    rejected_target: u32,
    rejected_cool: u32,
    rejected_inflight: u32,
    rejected_quota: u32,
    // T10 — operator cancel: once set, the leader stops admitting new jobs at
    // the top of its loop (in-flight/buffered work still drains). Separate from
    // `target_met()` so "stopped by hand" stays distinguishable in the summary.
    force_stop: bool,
    /// T10c — admits rejected because the engine was blacklisted.
    rejected_disabled: u32,
    /// T10c — why the leader stopped (set by the pool at the stop moment and
    /// surfaced in the run summary, so "why did it stop?" is always answered
    /// in plain English: cancelled / target met / engines exhausted / …).
    stop_reason: Option<String>,
}

impl FairState {
    /// Build a controller for a known set of engines (each pre-registered with
    /// zeroed share). Unknown engine ids encountered later are created lazily.
    pub fn new(policy: FairPolicy, engines: impl IntoIterator<Item = Uuid>) -> Self {
        let engines = engines
            .into_iter()
            .map(|id| (id, EngineFair::default()))
            .collect();
        Self {
            policy,
            engines,
            total_links: 0,
            rejected_target: 0,
            rejected_cool: 0,
            rejected_inflight: 0,
            rejected_quota: 0,
            force_stop: false,
            rejected_disabled: 0,
            stop_reason: None,
        }
    }

    /// **Leader call.** May we dispatch a job for `engine` right now?
    ///
    /// Returns `true` and reserves an in-flight slot when yes (the worker MUST
    /// later call [`FairState::complete`] or [`FairState::release`] to give it
    /// back). Returns `false` when the engine is cooling, over its in-flight or
    /// per-run link cap, or the global target is already met.
    pub fn admit(&mut self, engine: Uuid, now: Instant) -> bool {
        if self.policy.target > 0 && self.total_links >= self.policy.target {
            self.rejected_target += 1;
            return false;
        }
        // T10c — a blacklisted engine (3 consecutive non-successes) is not
        // offered any more work this run: re-dispatching a dead engine forever
        // is exactly the loop that a replenished queue would otherwise create.
        if self.engines.get(&engine).map(|e| e.disabled).unwrap_or(false) {
            self.rejected_disabled += 1;
            return false;
        }
        let e = self.engines.entry(engine).or_default();
        if self.policy.max_per_engine > 0 && e.links >= self.policy.max_per_engine {
            self.rejected_quota += 1;
            return false;
        }
        if let Some(until) = e.cool_until {
            if now < until {
                self.rejected_cool += 1;
                return false;
            }
        }
        if self.policy.max_inflight_per_engine > 0 && e.inflight >= self.policy.max_inflight_per_engine
        {
            self.rejected_inflight += 1;
            return false;
        }
        e.inflight += 1;
        e.admitted += 1;
        true
    }

    /// **Worker call.** A job this worker was handed for `engine` finished with
    /// `class`. Releases the in-flight slot and records the outcome (success
    /// counts toward the target; rate-limit schedules a cooldown).
    pub fn complete(&mut self, engine: Uuid, class: OutcomeClass, now: Instant) {
        // Capture the (Copy) policy fields up front so computing the backoff
        // below never borrows `self` while the `engines` entry is mutably held.
        let (base, max) = (self.policy.base_backoff, self.policy.max_backoff);
        let e = self.engines.entry(engine).or_default();
        e.inflight = e.inflight.saturating_sub(1);
        match class {
            OutcomeClass::Success => {
                e.links += 1;
                self.total_links += 1;
                e.rate_streak = 0; // recovered — reset the backoff streak
                e.fail_streak = 0; // healthy again — clear the blacklist streak
            }
            OutcomeClass::RateLimited => {
                e.rate_streak = e.rate_streak.saturating_add(1);
                e.rate_hits += 1;
                let backoff = backoff_delay(base, max, e.rate_streak);
                e.cool_until = Some(now + backoff);
                // T10c — also counts the blacklist streak: an engine
                // 429-walled three times in a row is not coming back this run.
                e.fail_streak = e.fail_streak.saturating_add(1);
                if e.fail_streak >= BLACKLIST_AFTER {
                    e.disabled = true;
                }
            }
            OutcomeClass::Other => {
                // No rate penalty — a clean non-rate error is not evidence the
                // engine is rate-limiting, so the backoff state stays as-is.
                // But it still counts the blacklist streak (T10c): three
                // straight non-successes of any kind means the engine is dead
                // for this run, and replenishment must not keep offering it.
                e.fail_streak = e.fail_streak.saturating_add(1);
                if e.fail_streak >= BLACKLIST_AFTER {
                    e.disabled = true;
                }
            }
        }
    }

    /// **Worker call.** Give back an in-flight slot without recording an
    /// outcome — used on claim-race / channel-close paths where no engine result
    /// is known (e.g. another worker took the job, or the worker task ended).
    pub fn release(&mut self, engine: Uuid) {
        if let Some(e) = self.engines.get_mut(&engine) {
            e.inflight = e.inflight.saturating_sub(1);
        }
    }

    /// Leader call — is the global link target already met (or unbounded)?
    /// Used to stop dispatching in fair mode: once true, the remaining queued
    /// jobs stay `pending` (resumable) and no new work is admitted.
    pub fn target_met(&self) -> bool {
        self.policy.target == 0 || self.total_links >= self.policy.target
    }

    /// T10 — operator cancel. `set_force_stop(true)` asks the leader to stop
    /// admitting new jobs; `force_stop()` reports whether a cancel was requested.
    pub fn set_force_stop(&mut self, v: bool) {
        self.force_stop = v;
    }
    pub fn force_stop(&self) -> bool {
        self.force_stop
    }
    /// Leader stop condition: cancel requested OR the fair target is met.
    /// Either one ends the current dispatch pass (in-flight work still drains).
    pub fn should_stop(&self) -> bool {
        self.force_stop || self.target_met()
    }

    // ── T10c — exhaustion + stop reason ──────────────────────────────────
    /// T10c — every registered engine is blacklisted (3 consecutive
    /// non-successes each). With queue replenishment on, this is the run's
    /// termination condition: nothing left that can do work, so the leader
    /// stops instead of provisioning jobs for dead engines forever.
    pub fn engines_exhausted(&self) -> bool {
        !self.engines.is_empty() && self.engines.values().all(|e| e.disabled)
    }
    /// T10c — how many engines are blacklisted this run (summary).
    pub fn blacklisted_count(&self) -> u32 {
        self.engines.values().filter(|e| e.disabled).count() as u32
    }
    /// T10c — is `engine` blacklisted? (per-engine summary row)
    pub fn disabled_for(&self, engine: Uuid) -> bool {
        self.engines.get(&engine).map(|e| e.disabled).unwrap_or(false)
    }
    /// T10c — record why the leader stopped. The pool sets this at the exact
    /// stop moment; the API surfaces it in the run summary + live status.
    pub fn set_stop_reason(&mut self, reason: impl Into<String>) {
        self.stop_reason = Some(reason.into());
    }
    /// T10c — the recorded stop reason (once set, kept).
    pub fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }

    // ── introspection (summary + tests) ─────────────────────────────
    pub fn policy(&self) -> &FairPolicy {
        &self.policy
    }
    pub fn total_links(&self) -> usize {
        self.total_links
    }
    pub fn links_for(&self, id: Uuid) -> usize {
        self.engines.get(&id).map(|e| e.links).unwrap_or(0)
    }
    pub fn in_flight_for(&self, id: Uuid) -> usize {
        self.engines.get(&id).map(|e| e.inflight as usize).unwrap_or(0)
    }
    pub fn cool_remaining(&self, id: Uuid, now: Instant) -> Option<Duration> {
        self.engines
            .get(&id)
            .and_then(|e| e.cool_until)
            .and_then(|u| (now < u).then_some(u - now))
    }
    pub fn rate_hits_for(&self, id: Uuid) -> u32 {
        self.engines.get(&id).map(|e| e.rate_hits).unwrap_or(0)
    }
    pub fn admitted_for(&self, id: Uuid) -> u32 {
        self.engines.get(&id).map(|e| e.admitted).unwrap_or(0)
    }
    pub fn rejections(&self) -> (u32, u32, u32, u32, u32) {
        (
            self.rejected_target,
            self.rejected_cool,
            self.rejected_inflight,
            self.rejected_quota,
            self.rejected_disabled,
        )
    }

    /// Per-engine snapshot for the run summary (deterministic order).
    pub fn snapshot(&self, now: Instant) -> Vec<EngineSnapshot> {
        let mut out: Vec<EngineSnapshot> = self
            .engines
            .iter()
            .map(|(id, e)| EngineSnapshot {
                engine: *id,
                links: e.links,
                in_flight: e.inflight,
                rate_hits: e.rate_hits,
                admitted: e.admitted,
                cool_remaining: self.cool_remaining(*id, now),
                disabled: e.disabled,
            })
            .collect();
        out.sort_by(|a, b| a.engine.to_string().cmp(&b.engine.to_string()));
        out
    }
}

/// One engine's fair-share record at a point in time (for the summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSnapshot {
    pub engine: Uuid,
    pub links: usize,
    pub in_flight: u32,
    pub rate_hits: u32,
    pub admitted: u32,
    pub cool_remaining: Option<Duration>,
    /// T10c — blacklisted for this run (3 straight non-successes).
    pub disabled: bool,
}

/// Is this failure an engine "got hot" signal (rate-limit family) rather than a
/// transport/parse miss? Matches the HTTP 429/406/403 family, common "too many
/// requests" phrasings, and captcha walls.
pub fn is_rate_limited(errors: &[&str]) -> bool {
    const MARKERS: &[&str] = &[
        "429",
        "too many requests",
        "rate limit",
        "rate-limited",
        "ratelimit",
        "rate_limit",
        "406",
        "403",
        "unusual traffic",
        "captcha",
        "recaptcha",
        "too many",
        "limit exceeded",
        "temporarily blocked",
        "slow down",
        "back off",
        "backoff",
    ];
    errors.iter().any(|e| {
        let l = e.to_ascii_lowercase();
        MARKERS.iter().any(|m| l.contains(m))
    })
}

/// Classify a finished [`crate::execute::JobOutcome`] for the controller.
pub fn classify(outcome: &crate::execute::JobOutcome) -> OutcomeClass {
    if outcome.successful() {
        return OutcomeClass::Success;
    }
    // A captcha wall means the engine is rate-limiting / bot-walling us.
    if outcome.report.captcha_failed > 0 {
        return OutcomeClass::RateLimited;
    }
    let mut errs: Vec<&str> = outcome.report.errors.iter().map(String::as_str).collect();
    if let Some(d) = &outcome.driver_error {
        errs.push(d.as_str());
    }
    if is_rate_limited(&errs) {
        return OutcomeClass::RateLimited;
    }
    OutcomeClass::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fair_quota_even_split() {
        assert_eq!(fair_quota(0, 0), 0);
        assert_eq!(fair_quota(10, 2), 5);
        assert_eq!(fair_quota(10, 3), 4); // 10/3 = 3.33 -> 4
        assert_eq!(fair_quota(1, 5), 1);
    }

    #[test]
    fn success_counts_toward_target_and_stops_admit() {
        let a = Uuid::new_v4();
        let mut s = FairState::new(
            FairPolicy {
                target: 1,
                ..Default::default()
            },
            [a],
        );
        let now = Instant::now();
        assert!(s.admit(a, now));
        s.complete(a, OutcomeClass::Success, now);
        assert_eq!(s.total_links(), 1);
        assert_eq!(s.links_for(a), 1);
        // Target met: nothing more is admitted.
        assert!(!s.admit(a, now));
        let (rt, _, _, _, _) = s.rejections();
        assert_eq!(rt, 1);
    }

    #[test]
    fn in_flight_cap_bounds_an_engine() {
        let a = Uuid::new_v4();
        let mut s = FairState::new(
            FairPolicy {
                max_inflight_per_engine: 1,
                ..Default::default()
            },
            [a],
        );
        let now = Instant::now();
        assert!(s.admit(a, now));
        // Second concurrent dispatch on the same engine is denied.
        assert!(!s.admit(a, now));
        // ...but another engine is fine.
        let b = Uuid::new_v4();
        assert!(s.admit(b, now));
        // Release one slot -> the first engine can go again.
        s.release(a);
        assert!(s.admit(a, now));
    }

    #[test]
    fn rate_limit_triggers_exponential_cooldown() {
        let a = Uuid::new_v4();
        let base = Duration::from_secs(20);
        let max = Duration::from_secs(120);
        let mut s = FairState::new(
            FairPolicy {
                base_backoff: base,
                max_backoff: max,
                ..Default::default()
            },
            [a],
        );
        let t0 = Instant::now();
        // 1st hit: cool for base.
        s.complete(a, OutcomeClass::RateLimited, t0);
        let r1 = s.cool_remaining(a, t0).unwrap();
        assert_eq!(r1, base);
        assert!(!s.admit(a, t0));

        // Let the cooldown pass, hit again -> double.
        let t1 = t0 + base;
        assert!(s.admit(a, t1));
        s.complete(a, OutcomeClass::RateLimited, t1);
        assert_eq!(s.cool_remaining(a, t1).unwrap(), base * 2);

        // Third consecutive hit -> x4, but clamped to max.
        let t2 = t1 + base * 2;
        assert!(s.admit(a, t2));
        s.complete(a, OutcomeClass::RateLimited, t2);
        assert_eq!(s.cool_remaining(a, t2).unwrap(), base * 4);
    }

    #[test]
    fn cooldown_clamps_to_max() {
        let a = Uuid::new_v4();
        let mut s = FairState::new(
            FairPolicy {
                base_backoff: Duration::from_secs(100),
                max_backoff: Duration::from_secs(100),
                ..Default::default()
            },
            [a],
        );
        let t0 = Instant::now();
        s.complete(a, OutcomeClass::RateLimited, t0);
        assert_eq!(s.cool_remaining(a, t0).unwrap(), Duration::from_secs(100));
    }

    #[test]
    fn success_resets_rate_streak() {
        let a = Uuid::new_v4();
        let base = Duration::from_secs(20);
        let mut s = FairState::new(FairPolicy { base_backoff: base, ..Default::default() }, [a]);
        let t0 = Instant::now();
        s.complete(a, OutcomeClass::RateLimited, t0); // streak 1 -> base
        let t1 = t0 + base;
        assert!(s.admit(a, t1));
        s.complete(a, OutcomeClass::Success, t1); // reset streak
        let t2 = t1 + Duration::from_secs(1);
        assert!(s.admit(a, t2));
        s.complete(a, OutcomeClass::RateLimited, t2); // fresh streak 1 -> base (not 2x)
        assert_eq!(s.cool_remaining(a, t2).unwrap(), base);
    }

    #[test]
    fn max_per_engine_caps_overflow() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut s = FairState::new(
            FairPolicy {
                max_per_engine: 2,
                target: 0,
                ..Default::default()
            },
            [a, b],
        );
        let now = Instant::now();
        // Two links go through under the cap...
        assert!(s.admit(a, now));
        s.complete(a, OutcomeClass::Success, now);
        assert!(s.admit(a, now));
        s.complete(a, OutcomeClass::Success, now);
        assert_eq!(s.links_for(a), 2);
        // ...the 3rd admit is rejected because `a` hit its per-run cap — so a
        // capped engine never exceeds its share and the load goes elsewhere.
        assert!(!s.admit(a, now));
        let (_, _, _, rq, _) = s.rejections();
        assert_eq!(rq, 1);
    }

    #[test]
    fn is_rate_limited_matches_markers() {
        assert!(is_rate_limited(&["HTTP 429 Too Many Requests"]));
        assert!(is_rate_limited(&["rate limited"]));
        assert!(is_rate_limited(&["Captcha challenge"]));
        assert!(is_rate_limited(&["403 Forbidden - unusual traffic"]));
        assert!(!is_rate_limited(&["connection refused"]));
        assert!(!is_rate_limited(&["form field missing"]));
        assert!(!is_rate_limited(&[]));
    }

    #[test]
    fn no_leak_on_release_path() {
        let a = Uuid::new_v4();
        let mut s = FairState::new(FairPolicy { max_inflight_per_engine: 2, ..Default::default() }, [a]);
        let now = Instant::now();
        assert!(s.admit(a, now));
        assert_eq!(s.in_flight_for(a), 1);
        s.release(a);
        assert_eq!(s.in_flight_for(a), 0);
        assert!(s.admit(a, now));
    }

    #[test]
    fn blacklist_after_three_failures_stops_admissions() {
        let a = Uuid::new_v4();
        let mut s = FairState::new(FairPolicy { target: 0, ..Default::default() }, [a]);
        // Distinct, advancing timestamps: a RateLimited outcome puts the
        // engine in cooldown, so re-admitting "at the same instant" would be
        // rejected on cooling (correct behavior) before ever testing the
        // blacklist path.
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(3600);
        let t2 = t1 + Duration::from_secs(3600);
        // Two consecutive non-successes: still offered work…
        assert!(s.admit(a, t0));
        s.complete(a, OutcomeClass::Other, t0);
        assert!(s.admit(a, t1));
        s.complete(a, OutcomeClass::RateLimited, t1);
        assert!(!s.disabled_for(a));
        // …the third consecutive one blacklists the engine for the run.
        assert!(s.admit(a, t2));
        s.complete(a, OutcomeClass::Other, t2);
        assert!(s.disabled_for(a));
        assert!(!s.admit(a, t2 + Duration::from_secs(3600)));
        let (_, _, _, _, rj_disabled) = s.rejections();
        assert_eq!(rj_disabled, 1);
        // Single engine out => the run can terminate instead of looping.
        assert!(s.engines_exhausted());
    }

    #[test]
    fn success_clears_blacklist_streak() {
        let a = Uuid::new_v4();
        let mut s = FairState::new(FairPolicy { target: 0, ..Default::default() }, [a]);
        let t0 = Instant::now();
        assert!(s.admit(a, t0));
        s.complete(a, OutcomeClass::Other, t0); // streak 1
        assert!(s.admit(a, t0));
        s.complete(a, OutcomeClass::Success, t0); // reset
        assert!(s.admit(a, t0));
        s.complete(a, OutcomeClass::Other, t0); // streak 1 again
        assert!(s.admit(a, t0));
        s.complete(a, OutcomeClass::Other, t0); // streak 2 — under the threshold
        assert!(!s.disabled_for(a));
    }

    #[test]
    fn engines_exhausted_only_when_all_blacklisted() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut s = FairState::new(FairPolicy::default(), [a, b]);
        let t0 = Instant::now();
        for _ in 0..BLACKLIST_AFTER {
            assert!(s.admit(a, t0));
            s.complete(a, OutcomeClass::Other, t0);
        }
        assert!(s.disabled_for(a));
        assert!(!s.engines_exhausted()); // b can still work
        for _ in 0..BLACKLIST_AFTER {
            assert!(s.admit(b, t0));
            s.complete(b, OutcomeClass::Other, t0);
        }
        assert!(s.engines_exhausted());
        assert_eq!(s.blacklisted_count(), 2);
    }

    #[test]
    fn stop_reason_records_and_survives() {
        let mut s = FairState::new(FairPolicy::default(), [Uuid::new_v4()]);
        assert_eq!(s.stop_reason(), None);
        s.set_stop_reason("all engines exhausted (0 links created)");
        assert_eq!(s.stop_reason(), Some("all engines exhausted (0 links created)"));
    }
}
