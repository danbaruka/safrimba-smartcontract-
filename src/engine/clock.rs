//! Pure time model. Every deadline of a running circle is a function of
//! `(anchor, round duration, grace, slots, pauses)` — nothing is "advanced" by
//! a transaction. Effective time stops while the circle is paused, so every
//! later deadline shifts by the paused duration.
//!
//! Pauses are bounded (PLAN_V5 3.9): one pause lasts at most
//! `max_pause = min(round, 7 days)` and the circle resumes by itself at
//! `paused_at + pause_limit()`; all pauses together last at most
//! `2 × max_pause`. No admin action is needed to get the money moving again.

use crate::state::{Circle, DistributionThreshold};

/// Upper bound of a single pause, whatever the round duration.
pub const MAX_PAUSE_CAP_SECS: u64 = 7 * 86_400;
/// Number of maximal pauses a circle may spend in total.
pub const PAUSE_BUDGET_FACTOR: u64 = 2;

/// Longest single pause for a round duration: `min(round, 7 days)`.
pub fn max_pause_secs(round_secs: u64) -> u64 {
    round_secs.clamp(1, MAX_PAUSE_CAP_SECS)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    /// Effective-time instant at which round 1 opens.
    pub start: u64,
    pub round_secs: u64,
    pub grace_secs: u64,
    pub slots: u32,
    pub total_rounds: u32,
    /// Seconds paused by completed pauses.
    pub paused_total: u64,
    /// Wall instant the current pause began.
    pub paused_at: Option<u64>,
    /// Longest single pause.
    pub max_pause: u64,
}

impl Clock {
    /// `None` until the circle has started.
    pub fn of(c: &Circle) -> Option<Clock> {
        let start = c.first_cycle_date?.seconds();
        let slots = c.members_at_start?;
        if slots == 0 {
            return None;
        }
        let round_secs = c.cycle_duration_secs().max(1);
        Some(Clock {
            start,
            round_secs,
            grace_secs: c.grace_period_secs(),
            slots,
            total_rounds: slots.saturating_mul(c.total_cycles),
            paused_total: c.paused_secs_total,
            paused_at: c.paused_at.map(|t| t.seconds()),
            max_pause: max_pause_secs(round_secs),
        })
    }

    /// Total pause budget of the circle.
    pub fn pause_budget(&self) -> u64 {
        self.max_pause.saturating_mul(PAUSE_BUDGET_FACTOR)
    }

    /// Pause budget left before the current pause (if any) started.
    pub fn pause_budget_left(&self) -> u64 {
        self.pause_budget().saturating_sub(self.paused_total)
    }

    /// Longest the current (or next) pause may last: per-pause cap and remaining budget.
    pub fn pause_limit(&self) -> u64 {
        self.max_pause.min(self.pause_budget_left())
    }

    /// Wall instant at which the current pause ends by itself.
    pub fn paused_until(&self) -> Option<u64> {
        self.paused_at
            .map(|p| p.saturating_add(self.pause_limit()))
    }

    /// Paused seconds counted at wall time `now` (the running pause is capped).
    pub fn paused_secs_at(&self, now: u64) -> u64 {
        let ongoing = match self.paused_at {
            Some(p) if now > p => (now - p).min(self.pause_limit()),
            _ => 0,
        };
        self.paused_total.saturating_add(ongoing)
    }

    /// Effective time: wall time minus every paused second (frozen while
    /// paused, running again once the pause limit is reached).
    pub fn eff(&self, now: u64) -> u64 {
        now.saturating_sub(self.paused_secs_at(now))
    }

    /// Wall-clock instant of an effective instant. While paused, instants after
    /// the pause began are placed after the automatic resume: the latest they
    /// can happen (an earlier admin resume brings them forward).
    pub fn to_wall(&self, t_eff: u64, _now: u64) -> u64 {
        let base = t_eff.saturating_add(self.paused_total);
        match self.paused_at {
            Some(p) if base > p => base.saturating_add(self.pause_limit()),
            _ => base,
        }
    }

    pub fn opens(&self, k: u32) -> u64 {
        self.start
            .saturating_add((k.max(1) as u64 - 1).saturating_mul(self.round_secs))
    }

    pub fn due(&self, k: u32) -> u64 {
        self.start
            .saturating_add((k as u64).saturating_mul(self.round_secs))
    }

    pub fn settles(&self, k: u32) -> u64 {
        self.due(k).saturating_add(self.grace_secs)
    }

    pub fn cycle_of(&self, k: u32) -> u32 {
        (k.max(1) - 1) / self.slots + 1
    }

    pub fn round_in_cycle(&self, k: u32) -> u32 {
        (k.max(1) - 1) % self.slots + 1
    }

    pub fn is_open(&self, k: u32, now: u64) -> bool {
        self.opens(k) <= self.eff(now)
    }

    pub fn is_settleable(&self, k: u32, now: u64) -> bool {
        self.settles(k) <= self.eff(now)
    }

    /// Highest round that has opened (0 before the anchor), capped at the schedule.
    pub fn latest_open_round(&self, now: u64) -> u32 {
        let e = self.eff(now);
        if e < self.start {
            return 0;
        }
        let k = (e - self.start) / self.round_secs + 1;
        (k.min(self.total_rounds as u64)) as u32
    }

    /// Rounds whose settlement time has passed but that are not settled yet.
    pub fn rounds_due(&self, rounds_settled: u32, now: u64) -> u32 {
        let e = self.eff(now);
        if e < self.settles(1) {
            return 0;
        }
        // settles(k) <= e  <=>  k <= (e - start - grace) / round
        let k_max = (e - self.start - self.grace_secs) / self.round_secs;
        let k_max = (k_max.min(self.total_rounds as u64)) as u32;
        k_max.saturating_sub(rounds_settled)
    }
}

/// First round (within a cycle) where distribution happens.
pub fn threshold_start(threshold: &Option<DistributionThreshold>, slots: u32) -> u32 {
    let slots = slots.max(1);
    match threshold {
        Some(DistributionThreshold::MinMembers { count }) => (*count).clamp(1, slots),
        _ => slots,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Pot is pooled for the end of the cycle.
    Accumulate,
    /// Pot goes to the member at this payout-order index.
    Turn(u32),
    /// Last round: members from this payout-order index onward share pot + pool.
    Group(u32),
}

pub fn kind_of(round_in_cycle: u32, slots: u32, ts: u32) -> Kind {
    if round_in_cycle < ts {
        Kind::Accumulate
    } else if round_in_cycle < slots {
        Kind::Turn(round_in_cycle - ts)
    } else {
        Kind::Group(slots - ts)
    }
}

/// Payout round (within a cycle, 1-based) of the member at `slot`.
pub fn payout_round_in_cycle(slot: u32, slots: u32, ts: u32) -> u32 {
    let group_from = slots - ts;
    if slot < group_from {
        slot + ts
    } else {
        slots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(slots: u32) -> Clock {
        Clock {
            start: 1_000,
            round_secs: 100,
            grace_secs: 10,
            slots,
            total_rounds: slots * 2,
            paused_total: 0,
            paused_at: None,
            max_pause: max_pause_secs(100),
        }
    }

    #[test]
    fn deadlines_are_pure_functions_of_time() {
        let c = clock(3);
        assert_eq!(c.opens(1), 1_000);
        assert_eq!(c.due(1), 1_100);
        assert_eq!(c.settles(1), 1_110);
        assert_eq!(c.opens(4), 1_300);
        assert_eq!(c.cycle_of(4), 2);
        assert_eq!(c.round_in_cycle(4), 1);
        assert_eq!(c.latest_open_round(999), 0);
        assert_eq!(c.latest_open_round(1_000), 1);
        assert_eq!(c.latest_open_round(1_250), 3);
        assert_eq!(c.latest_open_round(99_999), 6);
        assert_eq!(c.rounds_due(0, 1_109), 0);
        assert_eq!(c.rounds_due(0, 1_110), 1);
        assert_eq!(c.rounds_due(0, 1_310), 3);
        assert_eq!(c.rounds_due(2, 1_310), 1);
        assert_eq!(c.rounds_due(0, 99_999), 6);
    }

    #[test]
    fn pause_freezes_effective_time_and_shifts_deadlines() {
        let mut c = clock(3);
        c.paused_at = Some(1_050);
        // Frozen during the pause (max_pause = one round = 100 s)...
        assert_eq!(c.eff(1_100), 1_050);
        assert_eq!(c.eff(1_150), 1_050);
        assert!(!c.is_settleable(1, 1_150));
        // ...then time runs again by itself: the pause counted exactly 100 s.
        assert_eq!(c.eff(1_160), 1_060);
        assert_eq!(c.eff(5_000), 4_900);
        assert_eq!(c.paused_until(), Some(1_150));
        // While paused, future deadlines are placed after the automatic resume.
        assert_eq!(c.to_wall(c.settles(1), 1_100), 1_110 + 100);
        // An instant before the pause keeps its wall time.
        assert_eq!(c.to_wall(c.opens(1), 1_100), 1_000);
        // Resumed by the admin after 30 s: every later deadline shifts by 30 s.
        c.paused_at = None;
        c.paused_total = 30;
        assert_eq!(c.eff(1_200), 1_170);
        assert!(!c.is_settleable(1, 1_139));
        assert!(c.is_settleable(1, 1_140));
        assert_eq!(c.to_wall(c.settles(1), 1_120), 1_140);
    }

    #[test]
    fn pause_budget_is_two_maximal_pauses() {
        let mut c = clock(3);
        assert_eq!(c.pause_budget(), 200);
        assert_eq!(c.pause_limit(), 100);
        c.paused_total = 150;
        // Only 50 s of budget left: the next pause ends after 50 s.
        assert_eq!(c.pause_limit(), 50);
        c.paused_at = Some(2_000);
        assert_eq!(c.paused_until(), Some(2_050));
        assert_eq!(c.eff(2_100), 2_100 - 200);
        c.paused_at = None;
        c.paused_total = 200;
        assert_eq!(c.pause_budget_left(), 0);
        assert_eq!(c.pause_limit(), 0);
        // Long rounds: a single pause is capped at 7 days.
        assert_eq!(max_pause_secs(30 * 86_400), MAX_PAUSE_CAP_SECS);
        assert_eq!(max_pause_secs(3_600), 3_600);
    }

    #[test]
    fn unified_distribution_rule() {
        // rotating: ts = 1
        assert_eq!(kind_of(1, 4, 1), Kind::Turn(0));
        assert_eq!(kind_of(3, 4, 1), Kind::Turn(2));
        assert_eq!(kind_of(4, 4, 1), Kind::Group(3));
        // total: ts = slots
        assert_eq!(kind_of(1, 4, 4), Kind::Accumulate);
        assert_eq!(kind_of(4, 4, 4), Kind::Group(0));
        // n-rounds: ts = 2
        assert_eq!(kind_of(1, 4, 2), Kind::Accumulate);
        assert_eq!(kind_of(2, 4, 2), Kind::Turn(0));
        assert_eq!(kind_of(3, 4, 2), Kind::Turn(1));
        assert_eq!(kind_of(4, 4, 2), Kind::Group(2));
        assert_eq!(payout_round_in_cycle(0, 4, 2), 2);
        assert_eq!(payout_round_in_cycle(1, 4, 2), 3);
        assert_eq!(payout_round_in_cycle(2, 4, 2), 4);
        assert_eq!(payout_round_in_cycle(3, 4, 2), 4);
    }
}
