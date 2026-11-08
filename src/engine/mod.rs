//! Safrimba settlement engine v5 ("clockwork").
//!
//! Functional core / imperative shell:
//!   * [`Model::load`] reads the circle and its member ledgers into memory;
//!   * [`Model::sync`] applies every transition the clock makes due (automatic
//!     resume of a bounded pause, auto start, round settlements, final
//!     settlement) — pure in-memory computation that records *effects*
//!     (transfers, history records, events);
//!   * [`Model::persist`] writes the result (execute path). Queries run the same
//!     `sync` and simply drop the effects, so every read is correct for the block
//!     time even when nobody has sent a transaction.
//!
//! Settlement never fails for "expected" situations (missing contributions,
//! empty locks, absent admins): shortfalls become arrears, the schedule always
//! moves, every coin has exactly one destination. A bank send that fails is
//! credited to the pull ledger (`reply`) instead of reverting the settlement.
//!
//! Fairness rules (PLAN_V5 §3): nobody gains from someone else's departure.
//! A member who leaves before being served in the cycle gets back what they
//! put into it (a *claim*, paid from the pooled carry or from what would have
//! been their turn); the rest of that orphaned turn compensates exactly the
//! members whose pots shrank because the leaver stopped paying. A member who
//! leaves after being served settles the rest of the cycle (voluntary exit) or
//! stays liable through their lock, the shortfall being recorded as arrears.

pub mod clock;

use std::collections::BTreeMap;

use cosmwasm_std::{
    Addr, BankMsg, Coin, Env, Event, Order, StdResult, Storage, SubMsg, Timestamp, Uint128,
};
use sha2::{Digest, Sha256};

use crate::error::ContractError;
use crate::state::{
    Circle, CircleStatus, DepositRecord, EventLog, MemberState, MemberStatus, PayoutOrderType,
    PayoutRecord, PenaltyRecord, PlatformConfig, RefundRecord, SendBatch, CIRCLES, DEPOSITS,
    EVENTS, EVENT_COUNTER, MEMBERS, PAYOUTS, PENALTIES, PLATFORM_CONFIG, REFUNDS, SEND_BATCH,
};
use clock::{kind_of, threshold_start, Clock, Kind};

/// Rounds settled at most per transaction (gas bound). The keeper re-ticks
/// immediately when more are due.
pub const MAX_ROUNDS_PER_TX: u32 = 24;
/// Rounds simulated at most by a query.
pub const MAX_ROUNDS_PER_QUERY: u32 = 2_000;
/// Longest accepted pause reason.
pub const MAX_PAUSE_REASON_LEN: usize = 140;

/// Recorded side effects of a model run.
#[derive(Default, Debug)]
pub struct Effects {
    pub sends: Vec<(Addr, Uint128)>,
    pub deposits: Vec<DepositRecord>,
    pub payouts: BTreeMap<(u32, Addr), PayoutRecord>,
    pub penalties: BTreeMap<(Addr, u32), PenaltyRecord>,
    pub refunds: BTreeMap<Addr, RefundRecord>,
    pub logs: Vec<(String, String)>,
    pub events: Vec<Event>,
    pub rounds_settled_now: u32,
    pub started_now: bool,
    pub closed_now: bool,
    pub resumed_now: bool,
}

impl Effects {
    fn send(&mut self, to: &Addr, amount: Uint128) {
        if amount.is_zero() {
            return;
        }
        if let Some(entry) = self.sends.iter_mut().find(|(a, _)| a == to) {
            entry.1 += amount;
        } else {
            self.sends.push((to.clone(), amount));
        }
    }

    fn log(&mut self, kind: &str, data: String) {
        self.logs.push((kind.to_string(), data));
    }
}

/// What a departure does, computed without side effects. The same function
/// drives the execution ([`Model::leave`]) and the quote shown in the live view,
/// so what a member is shown is exactly what happens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeparturePlan {
    /// The member already received their turn in the cycle they leave in.
    pub served: bool,
    /// Exit penalty (from the lock, to the common pool).
    pub penalty: Uint128,
    /// Paid rounds beyond the member's liability, refunded now.
    pub prepaid_rounds: u32,
    pub prepaid_refund: Uint128,
    /// (not served) Arrears extinguished against the claim.
    pub arrears_offset: Uint128,
    /// (not served) Claim acquired: contributions to the cycle, net of arrears.
    pub claim: Uint128,
    /// (not served) Part of the claim paid immediately from the pooled carry.
    pub claim_now: Uint128,
    /// (not served) Remaining arrears settled from the lock.
    pub arrears_from_lock: Uint128,
    /// (served, voluntary) Rounds settled up to the end of the cycle.
    pub owed_rounds: u32,
    /// (served, voluntary) `owed_rounds × contribution + arrears`.
    pub owed: Uint128,
    /// (served, voluntary) Part of `owed` taken from the lock.
    pub from_lock: Uint128,
    /// (served, voluntary) Funds the member must attach to `ExitCircle`.
    pub to_attach: Uint128,
    /// Lock returned immediately.
    pub lock_refund: Uint128,
    /// (served, ejected) The lock stays to cover the rest of the cycle.
    pub keeps_lock: bool,
}

pub struct Model {
    pub circle: Circle,
    /// Every address that ever joined (ordered by slot, then join order).
    pub members: Vec<MemberState>,
    dirty: Vec<bool>,
    pub removed: Vec<Addr>,
    pub fx: Effects,
    pub platform: PlatformConfig,
    pub now: Timestamp,
    pub height: u64,
    pub contract: Addr,
}

impl Model {
    pub fn load(storage: &dyn Storage, env: &Env, circle_id: u64) -> Result<Model, ContractError> {
        let circle = CIRCLES
            .may_load(storage, circle_id)?
            .ok_or(ContractError::CircleNotFound { circle_id })?;
        let platform = PLATFORM_CONFIG.load(storage)?;
        let mut members: Vec<MemberState> = MEMBERS
            .prefix(circle_id)
            .range(storage, None, None, Order::Ascending)
            .map(|r| r.map(|(_, m)| m))
            .collect::<StdResult<_>>()?;
        // Deterministic order: slot (after start), then join time, then address.
        members.sort_by(|a, b| {
            a.slot
                .cmp(&b.slot)
                .then(a.joined_at.cmp(&b.joined_at))
                .then(a.address.cmp(&b.address))
        });
        let n = members.len();
        Ok(Model {
            circle,
            members,
            dirty: vec![false; n],
            removed: vec![],
            fx: Effects::default(),
            platform,
            now: env.block.time,
            height: env.block.height,
            contract: env.contract.address.clone(),
        })
    }

    // ------------------------------------------------------------------
    // Accessors
    // ------------------------------------------------------------------

    pub fn clock(&self) -> Option<Clock> {
        Clock::of(&self.circle)
    }

    pub fn now_s(&self) -> u64 {
        self.now.seconds()
    }

    pub fn index_of(&self, addr: &Addr) -> Option<usize> {
        self.members.iter().position(|m| m.address == addr)
    }

    pub fn member(&self, addr: &Addr) -> Option<&MemberState> {
        self.members.iter().find(|m| m.address == addr)
    }

    pub fn m(&mut self, i: usize) -> &mut MemberState {
        self.dirty[i] = true;
        &mut self.members[i]
    }

    pub fn active_count(&self) -> u32 {
        self.members.iter().filter(|m| m.is_active()).count() as u32
    }

    pub fn member_at_slot(&self, slot: u32) -> Option<usize> {
        self.members.iter().position(|m| m.slot == slot)
    }

    pub fn ts(&self) -> u32 {
        threshold_start(&self.circle.distribution_threshold, self.circle.slots())
    }

    pub fn push_member(&mut self, m: MemberState) {
        self.members.push(m);
        self.dirty.push(true);
    }

    pub fn remove_member(&mut self, i: usize) {
        let m = self.members.remove(i);
        self.dirty.remove(i);
        self.removed.push(m.address);
    }

    fn is_active_addr(&self, a: &Addr) -> bool {
        self.member(a).map(|m| m.is_active()).unwrap_or(false)
    }

    /// Addresses holding admin powers right now (PLAN_V5 3.4): the creator and
    /// the co-admin while they are active members; if neither is, the active
    /// member with the lowest slot. No admin power is needed for the schedule.
    pub fn admins(&self) -> Vec<Addr> {
        let mut out: Vec<Addr> = vec![];
        let creator = &self.circle.creator_address;
        if self.is_active_addr(creator) {
            out.push(creator.clone());
        }
        if let Some(co) = &self.circle.co_admin_address {
            if co != creator && self.is_active_addr(co) {
                out.push(co.clone());
            }
        }
        if out.is_empty() {
            if let Some(m) = self
                .members
                .iter()
                .filter(|m| m.is_active())
                .min_by(|a, b| {
                    a.slot
                        .cmp(&b.slot)
                        .then(a.joined_at.cmp(&b.joined_at))
                        .then(a.address.cmp(&b.address))
                })
            {
                out.push(m.address.clone());
            }
        }
        out
    }

    /// Primary admin (receives rounding remainders).
    pub fn admin(&self) -> Option<Addr> {
        self.admins().into_iter().next()
    }

    pub fn is_admin(&self, a: &Addr) -> bool {
        self.admins().iter().any(|x| x == a)
    }

    fn late_fee(&self) -> Uint128 {
        self.circle
            .contribution_amount
            .multiply_ratio(self.circle.late_fee_percent, 10_000u64)
    }

    fn event(&mut self, ty: &str, attrs: Vec<(&str, String)>) {
        let mut ev = Event::new(ty).add_attribute("circle_id", self.circle.circle_id.to_string());
        for (k, v) in attrs {
            ev = ev.add_attribute(k, v);
        }
        self.fx.events.push(ev);
    }

    fn set_status(&mut self, to: CircleStatus) {
        self.set_status_reason(to, "");
    }

    fn set_status_reason(&mut self, to: CircleStatus, reason: &str) {
        if self.circle.circle_status != to {
            let from = format!("{:?}", self.circle.circle_status);
            self.circle.circle_status = to.clone();
            let mut attrs = vec![("from", from), ("to", format!("{:?}", to))];
            if !reason.is_empty() {
                attrs.push(("reason", reason.to_string()));
            }
            self.event("safrimba_status", attrs);
        }
    }

    /// Rounds of the focus cycle that are already settled (0 at a cycle boundary).
    pub fn closed_in_cycle(&self, clock: &Clock) -> u32 {
        let settled = self.circle.rounds_settled;
        let cycle = clock.cycle_of(settled + 1);
        settled.saturating_sub((cycle - 1) * clock.slots)
    }

    /// Cycle of the focus (oldest unsettled) round; 0 before start.
    pub fn current_cycle(&self) -> u32 {
        match self.clock() {
            Some(c) => c
                .cycle_of((self.circle.rounds_settled + 1).min(c.total_rounds.max(1)))
                .min(self.circle.total_cycles.max(1)),
            None => 0,
        }
    }

    /// `EndCycle` ends the circle immediately only before the first
    /// distribution round of the current cycle (nobody served in it yet).
    pub fn end_now_allowed(&self) -> bool {
        if !matches!(
            self.circle.circle_status,
            CircleStatus::Running | CircleStatus::Paused
        ) {
            return false;
        }
        match self.clock() {
            Some(clock) => self.closed_in_cycle(&clock) < self.ts(),
            None => false,
        }
    }

    /// A voluntary exit in strict mode is only possible right after a cycle ended.
    #[allow(clippy::manual_is_multiple_of)] // keep `%`: older wasm toolchains
    pub fn at_cycle_boundary(&self) -> bool {
        let settled = self.circle.rounds_settled;
        match self.clock() {
            Some(c) => settled >= c.slots && settled % c.slots == 0,
            None => false,
        }
    }

    // ------------------------------------------------------------------
    // Clock-driven transitions
    // ------------------------------------------------------------------

    /// Apply every transition that is due at `self.now`. Returns rounds settled.
    pub fn sync(&mut self, max_rounds: u32) -> Result<u32, ContractError> {
        self.maybe_auto_resume();
        self.maybe_auto_start_by_date()?;
        let mut settled = 0u32;
        if matches!(
            self.circle.circle_status,
            CircleStatus::Running | CircleStatus::Paused
        ) {
            if let Some(clock) = self.clock() {
                while self.circle.rounds_settled < clock.total_rounds && settled < max_rounds {
                    let k = self.circle.rounds_settled + 1;
                    if !clock.is_settleable(k, self.now_s()) {
                        break;
                    }
                    self.settle_round(k, &clock)?;
                    settled += 1;
                    if self.circle.is_terminal() {
                        break;
                    }
                    if let Some(c) = self.circle.end_after_cycle {
                        if k >= c.saturating_mul(clock.slots) {
                            self.close(CircleStatus::Completed, "admin_end", None)?;
                            break;
                        }
                    }
                }
                if !self.circle.is_terminal() && self.circle.rounds_settled >= clock.total_rounds {
                    self.close(CircleStatus::Completed, "schedule_complete", None)?;
                }
            }
        }
        self.fx.rounds_settled_now += settled;
        self.refresh_legacy_fields();
        Ok(settled)
    }

    /// A bounded pause ends by itself (PLAN_V5 3.9).
    fn maybe_auto_resume(&mut self) {
        if self.circle.circle_status != CircleStatus::Paused {
            return;
        }
        let Some(clock) = self.clock() else {
            return;
        };
        let Some(until) = clock.paused_until() else {
            return;
        };
        if self.now_s() < until {
            return;
        }
        self.end_pause(clock.pause_limit(), "auto_resume");
    }

    /// Start a pause now (caller checked admin rights, status and budget).
    pub fn begin_pause(&mut self, reason: String, emergency: bool) {
        self.circle.paused_at = Some(self.now);
        self.circle.pause_reason = Some(reason.clone());
        self.circle.emergency_stop_triggered = emergency;
        let until = self
            .clock()
            .and_then(|c| c.paused_until())
            .unwrap_or_default();
        self.set_status_reason(CircleStatus::Paused, &reason);
        self.fx.log(
            if emergency { "emergency_stop" } else { "circle_paused" },
            format!("Circle paused until {} ({})", until, reason),
        );
    }

    /// End the current pause, counting `paused_secs` paused seconds.
    pub fn end_pause(&mut self, paused_secs: u64, reason: &str) {
        self.circle.paused_secs_total = self.circle.paused_secs_total.saturating_add(paused_secs);
        self.circle.paused_at = None;
        self.circle.pause_reason = None;
        self.circle.emergency_stop_triggered = false;
        self.set_status_reason(CircleStatus::Running, reason);
        self.fx.resumed_now = true;
        self.fx.log(
            "circle_unpaused",
            format!("Circle resumed ({}) after {}s", reason, paused_secs),
        );
    }

    /// Rounds still due after the last sync (non-zero only when a sync hit its bound).
    pub fn rounds_behind(&self) -> u32 {
        if !matches!(
            self.circle.circle_status,
            CircleStatus::Running | CircleStatus::Paused
        ) {
            return 0;
        }
        self.clock()
            .map(|c| c.rounds_due(self.circle.rounds_settled, self.now_s()))
            .unwrap_or(0)
    }

    /// Wall time at which the next automatic transition becomes due. While
    /// paused this already accounts for the automatic resume.
    pub fn next_action_at(&self) -> Option<Timestamp> {
        match self.circle.circle_status {
            CircleStatus::Draft | CircleStatus::Open | CircleStatus::Full => {
                if self.circle.auto_start_type.as_deref() == Some("by_date") {
                    self.circle.auto_start_date
                } else {
                    None
                }
            }
            CircleStatus::Running | CircleStatus::Paused => {
                let clock = self.clock()?;
                let k = self.circle.rounds_settled + 1;
                if k > clock.total_rounds {
                    return Some(self.now);
                }
                Some(Timestamp::from_seconds(
                    clock.to_wall(clock.settles(k), self.now_s()),
                ))
            }
            _ => None,
        }
    }

    fn maybe_auto_start_by_date(&mut self) -> Result<(), ContractError> {
        if !matches!(
            self.circle.circle_status,
            CircleStatus::Draft | CircleStatus::Open | CircleStatus::Full
        ) {
            return Ok(());
        }
        if self.circle.auto_start_type.as_deref() != Some("by_date") {
            return Ok(());
        }
        let Some(at) = self.circle.auto_start_date else {
            return Ok(());
        };
        if self.now < at {
            return Ok(());
        }
        let n = self.active_count();
        if n >= self.min_to_start() {
            // Anchor at the advertised date: the UI counted down to it and any
            // member could pay from that instant (every deposit syncs first).
            self.start(at, "auto_start_date")?;
        } else if self.circle.auto_refund_if_min_not_met {
            self.cancel_before_start("min_members_not_met_at_start_date")?;
        }
        Ok(())
    }

    pub fn min_to_start(&self) -> u32 {
        self.circle.min_members_required.max(2)
    }

    /// Admission path shared by Join / AcceptInvite / AddPrivateMember.
    pub fn after_admission(&mut self) -> Result<(), ContractError> {
        let n = self.members.len() as u32;
        if n >= self.circle.max_members {
            self.set_status(CircleStatus::Full);
            let by_members = self.circle.auto_start_when_full
                && matches!(self.circle.auto_start_type.as_deref(), None | Some("by_members"));
            if by_members {
                self.start(self.now, "full")?;
                return Ok(());
            }
        } else if self.circle.circle_status == CircleStatus::Draft {
            self.set_status(CircleStatus::Open);
        }
        // A by-date circle whose date passed while short of members starts as
        // soon as the minimum is reached.
        if self.circle.auto_start_type.as_deref() == Some("by_date")
            && self.circle.auto_start_date.map(|d| self.now >= d).unwrap_or(false)
            && n >= self.min_to_start()
            && !self.circle.is_started()
        {
            self.start(self.now, "auto_start_date_late")?;
        }
        Ok(())
    }

    /// Start the schedule anchored at `anchor`.
    pub fn start(&mut self, anchor: Timestamp, reason: &str) -> Result<(), ContractError> {
        let n = self.members.len() as u32;
        if n < self.min_to_start() {
            return Err(ContractError::MinMembersNotMet {
                required: self.min_to_start(),
                current: n,
            });
        }
        let order = self.build_payout_order();
        for (slot, addr) in order.iter().enumerate() {
            let i = self.index_of(addr).expect("member in order");
            let m = self.m(i);
            m.slot = slot as u32;
            m.paid_through = 0;
        }
        self.members.sort_by_key(|m| m.slot);
        self.dirty = vec![true; self.members.len()];

        let c = &mut self.circle;
        c.members_at_start = Some(n);
        c.start_date = Some(anchor);
        c.first_cycle_date = Some(anchor);
        c.rounds_settled = 0;
        c.paused_secs_total = 0;
        c.paused_at = None;
        c.pause_reason = None;
        c.end_after_cycle = None;
        c.carry_pot = Uint128::zero();
        c.comp_pool = Uint128::zero();
        c.payout_order_list = Some(order.clone());
        c.payout_amount = c.contribution_amount * Uint128::from(n);
        let total_rounds = n * c.total_cycles;
        c.max_missed_payments_allowed =
            compute_max_missed(c.exit_penalty_percent, c.late_fee_percent, total_rounds);
        self.set_status(CircleStatus::Running);
        self.fx.started_now = true;

        let start_s = anchor.seconds();
        self.fx.log(
            "circle_started",
            format!(
                "Circle started ({}) at {} with {} members, {} rounds of {}s",
                reason,
                start_s,
                n,
                total_rounds,
                self.circle.cycle_duration_secs()
            ),
        );
        self.event(
            "safrimba_started",
            vec![
                ("anchor", start_s.to_string()),
                ("reason", reason.to_string()),
                ("slots", n.to_string()),
                ("total_rounds", total_rounds.to_string()),
                (
                    "order",
                    order.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(","),
                ),
            ],
        );
        Ok(())
    }

    fn build_payout_order(&self) -> Vec<Addr> {
        let joined: Vec<Addr> = {
            let mut v: Vec<&MemberState> = self.members.iter().collect();
            v.sort_by(|a, b| a.joined_at.cmp(&b.joined_at).then(a.address.cmp(&b.address)));
            v.into_iter().map(|m| m.address.clone()).collect()
        };
        match self.circle.payout_order_type {
            PayoutOrderType::PredefinedOrder => {
                let mut order: Vec<Addr> = vec![];
                if let Some(list) = &self.circle.payout_order_list {
                    for a in list {
                        if joined.contains(a) && !order.contains(a) {
                            order.push(a.clone());
                        }
                    }
                }
                for a in &joined {
                    if !order.contains(a) {
                        order.push(a.clone());
                    }
                }
                order
            }
            PayoutOrderType::RandomOrder => {
                let mut order = joined;
                let mut seed = Sha256::new();
                seed.update(self.contract.as_bytes());
                seed.update(self.height.to_be_bytes());
                seed.update(self.now.nanos().to_be_bytes());
                for a in &order {
                    seed.update(a.as_bytes());
                }
                let mut digest = seed.finalize().to_vec();
                let mut cursor = 0usize;
                // Fisher–Yates driven by a SHA-256 stream.
                for i in (1..order.len()).rev() {
                    if cursor + 8 > digest.len() {
                        digest = Sha256::digest(&digest).to_vec();
                        cursor = 0;
                    }
                    let mut b = [0u8; 8];
                    b.copy_from_slice(&digest[cursor..cursor + 8]);
                    cursor += 8;
                    let j = (u64::from_be_bytes(b) % (i as u64 + 1)) as usize;
                    order.swap(i, j);
                }
                order
            }
        }
    }

    /// Settle round `k`: collect (real, lock-covered, arrears), charge late fees,
    /// apply ejection rules, distribute per the unified rule (orphaned shares go
    /// to the leaver's claim, then compensate), close the cycle's departures.
    fn settle_round(&mut self, k: u32, clock: &Clock) -> Result<(), ContractError> {
        let contribution = self.circle.contribution_amount;
        let late_fee = self.late_fee();
        let cycle = clock.cycle_of(k);
        let r = clock.round_in_cycle(k);
        let auto_eject = self.circle.auto_eject_enabled();
        let max_strikes = self.circle.max_missed_payments_allowed.max(1);
        let ts_now = self.now;

        let mut pot = Uint128::zero();
        let mut missed: Vec<String> = vec![];
        let mut to_eject: Vec<usize> = vec![];

        for i in 0..self.members.len() {
            if self.members[i].slot == u32::MAX {
                continue;
            }
            if self.members[i].paid_through >= k {
                // Paid for real (possibly before leaving).
                pot += contribution;
                continue;
            }
            let status = self.members[i].status;
            let left_cycle = clock.cycle_of(self.members[i].left_round.max(1));
            let served_when_left = self.members[i].last_received_cycle >= left_cycle;
            let m = self.m(i);
            m.paid_through = k;
            match status {
                MemberStatus::Active => {
                    m.missed_total += 1;
                    m.strikes += 1;
                    let could_cover = m.lock >= contribution;
                    let cover = m.lock.min(contribution);
                    m.lock -= cover;
                    m.covered += cover;
                    let short = contribution - cover;
                    m.arrears += short;
                    m.touch_cycle(cycle);
                    m.cy_short += short;
                    let fee = m.lock.min(late_fee);
                    m.lock -= fee;
                    m.fees_paid += fee;
                    let addr = m.address.clone();
                    let strikes = m.strikes;
                    pot += cover;
                    self.circle.total_arrears += short;
                    self.circle.common_pool += fee;
                    if !cover.is_zero() {
                        self.fx.deposits.push(DepositRecord {
                            member: addr.clone(),
                            cycle: k,
                            amount: cover,
                            timestamp: ts_now,
                            on_time: false,
                        });
                    }
                    if !fee.is_zero() {
                        self.add_penalty(&addr, k, fee, "late_fee");
                    }
                    missed.push(addr.to_string());
                    if auto_eject && (!could_cover || strikes >= max_strikes) {
                        to_eject.push(i);
                    }
                }
                MemberStatus::Ejected | MemberStatus::Exited => {
                    // Served before leaving: liable until the end of that cycle.
                    // The lock covers what it can; the rest is a recorded debt.
                    if left_cycle == cycle && served_when_left {
                        let cover = m.lock.min(contribution);
                        m.lock -= cover;
                        m.covered += cover;
                        let short = contribution - cover;
                        m.arrears += short;
                        m.touch_cycle(cycle);
                        m.cy_short += short;
                        let addr = m.address.clone();
                        pot += cover;
                        self.circle.total_arrears += short;
                        if !cover.is_zero() {
                            self.fx.deposits.push(DepositRecord {
                                member: addr,
                                cycle: k,
                                amount: cover,
                                timestamp: ts_now,
                                on_time: false,
                            });
                        }
                    }
                }
            }
        }

        for i in to_eject {
            self.leave(i, k, k, "auto", MemberStatus::Ejected, "engine", false);
        }

        // ---- distribution -------------------------------------------------
        let slots = clock.slots;
        let ts = self.ts();
        let kind = kind_of(r, slots, ts);
        let mut recipients: Vec<String> = vec![];
        let mut compensated: Vec<String> = vec![];
        match kind {
            Kind::Accumulate => {
                self.circle.carry_pot += pot;
            }
            Kind::Turn(idx) => match self.member_at_slot(idx) {
                Some(j) if self.eligible(j, cycle) => {
                    recipients.push(self.members[j].address.to_string());
                    self.pay_member(j, pot, k, Some(cycle), "turn");
                }
                Some(j) => {
                    self.orphan_share(j, pot, k);
                }
                None => {
                    self.circle.common_pool += pot;
                }
            },
            Kind::Group(from) => {
                let total = self.circle.carry_pot + pot;
                self.circle.carry_pot = Uint128::zero();
                let holders: Vec<usize> = (0..self.members.len())
                    .filter(|&j| {
                        let s = self.members[j].slot;
                        s != u32::MAX && s >= from
                    })
                    .collect();
                // One share per group slot, whoever holds it.
                let n_slots = slots.saturating_sub(from).max(1) as u128;
                let share = total.multiply_ratio(1u128, n_slots);
                let missing = n_slots.saturating_sub(holders.len() as u128);
                self.circle.common_pool +=
                    total - share * Uint128::from(n_slots) + share * Uint128::from(missing);
                for j in holders {
                    if self.eligible(j, cycle) {
                        recipients.push(self.members[j].address.to_string());
                        self.pay_member(j, share, k, Some(cycle), "group");
                    } else {
                        self.orphan_share(j, share, k);
                    }
                }
            }
        }

        // ---- end of cycle: regularise, then close the departures -----------
        if r == slots {
            compensated = self.settle_cycle(k, clock);
            for i in 0..self.members.len() {
                let m = &self.members[i];
                if m.is_active() || m.left_round == 0 || clock.cycle_of(m.left_round) != cycle {
                    continue;
                }
                let addr = m.address.clone();
                let lock = m.lock;
                let claim = m.claim;
                if !lock.is_zero() {
                    let mm = self.m(i);
                    mm.lock = Uint128::zero();
                    mm.refunded += lock;
                    self.refund(&addr, lock, "lock_released_after_leaving");
                }
                if !claim.is_zero() {
                    self.forfeit_claim(i, k);
                }
            }
        }

        self.circle.rounds_settled = k;
        let kind_s = match kind {
            Kind::Accumulate => "accumulate",
            Kind::Turn(_) => "turn",
            Kind::Group(_) => "group",
        };
        self.fx.log(
            "round_settled",
            format!(
                "Round {} (cycle {}, {}) settled: pot {} {}, missed [{}], recipients [{}], compensated [{}]",
                k,
                cycle,
                kind_s,
                pot,
                self.circle.denomination,
                missed.join(","),
                recipients.join(","),
                compensated.join(",")
            ),
        );
        self.event(
            "safrimba_round",
            vec![
                ("round", k.to_string()),
                ("cycle", cycle.to_string()),
                ("kind", kind_s.to_string()),
                ("pot", pot.to_string()),
                ("missed", missed.join(",")),
                ("recipients", recipients.join(",")),
                ("compensated", compensated.join(",")),
            ],
        );

        if self.active_count() < 2 && !self.circle.is_terminal() {
            self.close(CircleStatus::Ended, "not_enough_active_members", None)?;
        }
        Ok(())
    }

    fn eligible(&self, j: usize, cycle: u32) -> bool {
        let m = &self.members[j];
        m.is_active() && m.last_received_cycle < cycle
    }

    /// The share of a slot whose holder left (or cannot receive): first what
    /// the circle owes the leaver (their claim); the rest goes to the cycle's
    /// compensation reserve, settled when the cycle ends.
    fn orphan_share(&mut self, j: usize, share: Uint128, k: u32) {
        let mut rest = share;
        let claim = self.members[j].claim;
        if !claim.is_zero() && !rest.is_zero() {
            let paid = claim.min(rest);
            self.pay_claim(j, paid, k, "turn");
            rest -= paid;
        }
        self.circle.comp_pool += rest;
    }

    /// End-of-cycle regularisation (zero-sum per cycle, PLAN_V5 3.3): the
    /// compensation reserve fills, for everyone who took part in the cycle, the
    /// gap between what they put into it and what they received from it, and
    /// pays what is still owed on the claims of members who left before their
    /// turn — pro-rata if the reserve is short (misses), surplus to the common
    /// pool. A departure therefore never enriches the others: each participant
    /// ends the cycle with what they contributed, however many members left and
    /// whatever the distribution mode.
    fn settle_cycle(&mut self, k: u32, clock: &Clock) -> Vec<String> {
        let pool = self.circle.comp_pool;
        self.circle.comp_pool = Uint128::zero();
        if pool.is_zero() {
            return vec![];
        }
        let cycle = clock.cycle_of(k);
        let full = self.circle.contribution_amount * Uint128::from(clock.slots as u128);
        // (member, amount owed, is_claim)
        let mut needs: Vec<(usize, Uint128, bool)> = vec![];
        for j in 0..self.members.len() {
            let m = &self.members[j];
            if m.slot == u32::MAX {
                continue;
            }
            let left_this_cycle =
                !m.is_active() && m.left_round > 0 && clock.cycle_of(m.left_round) == cycle;
            if m.is_active() || (left_this_cycle && m.last_received_cycle >= cycle) {
                let (short, received) = m.cycle_counters(cycle);
                let gap = full.saturating_sub(short).saturating_sub(received);
                if !gap.is_zero() {
                    needs.push((j, gap, false));
                }
            } else if left_this_cycle && !m.claim.is_zero() {
                needs.push((j, m.claim, true));
            }
        }
        let total: Uint128 = needs.iter().map(|(_, a, _)| *a).sum();
        if total.is_zero() {
            self.circle.common_pool += pool;
            return vec![];
        }
        let enough = pool >= total;
        let mut paid = Uint128::zero();
        let mut out = vec![];
        for (j, need, is_claim) in needs {
            let amount = if enough {
                need
            } else {
                need.multiply_ratio(pool, total)
            };
            if amount.is_zero() {
                continue;
            }
            paid += amount;
            out.push(self.members[j].address.to_string());
            if is_claim {
                self.pay_claim(j, amount, k, "cycle_end");
            } else {
                self.pay_member(j, amount, k, None, "compensation");
            }
        }
        self.circle.common_pool += pool - paid;
        out
    }

    /// Pay `gross` to member `j`: platform fee, then arrears, then (active
    /// members) lock refill; the rest is pushed. `served` marks the member's
    /// turn of a cycle (counted in the cycle's received total).
    fn pay_member(&mut self, j: usize, gross: Uint128, k: u32, served: Option<u32>, kind: &str) {
        if let Some(cycle) = served {
            let m = self.m(j);
            m.touch_cycle(cycle);
            m.cy_received += gross;
            m.last_received_cycle = cycle;
        }
        if gross.is_zero() {
            return;
        }
        let fee = gross.multiply_ratio(self.circle.platform_fee_percent, 10_000u64);
        if !fee.is_zero() {
            let platform = self.platform.platform_address.clone();
            self.fx.send(&platform, fee);
            self.circle.total_platform_fees_collected += fee;
        }
        let mut net = gross - fee;
        let m = self.m(j);
        let recovered = m.arrears.min(net);
        m.arrears -= recovered;
        net -= recovered;
        let refill = if m.is_active() {
            m.refill_needed().min(net)
        } else {
            Uint128::zero()
        };
        m.lock += refill;
        net -= refill;
        if m.arrears.is_zero() && m.lock >= m.lock_original {
            m.strikes = 0;
        }
        m.received += net;
        let addr = m.address.clone();
        self.circle.total_arrears = self.circle.total_arrears.saturating_sub(recovered);
        self.circle.common_pool += recovered;
        self.circle.total_distributed += gross;
        self.fx.send(&addr, net);
        self.add_payout(k, &addr, net);
        self.event(
            "safrimba_payout",
            vec![
                ("round", k.to_string()),
                ("recipient", addr.to_string()),
                ("gross", gross.to_string()),
                ("platform_fee", fee.to_string()),
                ("arrears_recovered", recovered.to_string()),
                ("lock_refilled", refill.to_string()),
                ("net", net.to_string()),
                ("kind", kind.to_string()),
            ],
        );
    }

    /// Pay part of a member's claim (a return of their own contributions: no fee).
    fn pay_claim(&mut self, j: usize, amount: Uint128, round: u32, source: &str) {
        if amount.is_zero() {
            return;
        }
        let m = self.m(j);
        let amount = amount.min(m.claim);
        m.claim -= amount;
        m.refunded += amount;
        let addr = m.address.clone();
        self.fx.send(&addr, amount);
        self.record_refund(&addr, amount, "claim");
        self.event(
            "safrimba_claim_paid",
            vec![
                ("member", addr.to_string()),
                ("amount", amount.to_string()),
                ("round", round.to_string()),
                ("source", source.to_string()),
            ],
        );
    }

    fn forfeit_claim(&mut self, j: usize, round: u32) {
        let m = self.m(j);
        let amount = m.claim;
        if amount.is_zero() {
            return;
        }
        m.claim = Uint128::zero();
        let addr = m.address.clone();
        self.fx.log(
            "claim_forfeited",
            format!("Unpaid claim of {} extinguished: {}", addr, amount),
        );
        self.event(
            "safrimba_claim_forfeited",
            vec![
                ("member", addr.to_string()),
                ("amount", amount.to_string()),
                ("round", round.to_string()),
            ],
        );
    }

    // ------------------------------------------------------------------
    // Departures
    // ------------------------------------------------------------------

    /// What member `i` leaving at `at_round` would do. `collected_through` is
    /// the last round already collected (= settled, or the round being settled
    /// when the engine ejects). `settle_up`: a voluntary exit after being
    /// served settles the rest of the cycle instead of staying liable.
    pub fn departure_plan(
        &self,
        i: usize,
        at_round: u32,
        collected_through: u32,
        settle_up: bool,
    ) -> DeparturePlan {
        let m = &self.members[i];
        let c = self.circle.contribution_amount;
        let mut p = DeparturePlan::default();
        let Some(clock) = self.clock() else {
            // Before start: the lock simply goes back.
            p.lock_refund = m.lock;
            return p;
        };
        let at = at_round.max(1);
        let cycle = clock.cycle_of(at);
        let cycle_first = (cycle - 1) * clock.slots;
        let cycle_end = cycle * clock.slots;
        p.served = m.last_received_cycle >= cycle;
        p.penalty = m
            .lock_original
            .multiply_ratio(self.circle.exit_penalty_percent, 10_000u64)
            .min(m.lock);
        let mut lock = m.lock - p.penalty;
        if !p.served {
            // (a) Not served: no liability left; what they put in comes back.
            p.prepaid_rounds = m.paid_through.saturating_sub(collected_through);
            p.prepaid_refund = c * Uint128::from(p.prepaid_rounds);
            let n = collected_through.saturating_sub(cycle_first);
            let gross = c * Uint128::from(n);
            p.arrears_offset = m.arrears.min(gross);
            p.claim = gross - p.arrears_offset;
            p.claim_now = p.claim.min(self.circle.carry_pot);
            let arrears_left = m.arrears - p.arrears_offset;
            p.arrears_from_lock = arrears_left.min(lock);
            lock -= p.arrears_from_lock;
            p.lock_refund = lock;
        } else {
            // (b) Served: liable until the end of the cycle.
            let liable_through = cycle_end.max(collected_through);
            p.prepaid_rounds = m.paid_through.saturating_sub(liable_through);
            p.prepaid_refund = c * Uint128::from(p.prepaid_rounds);
            if settle_up {
                p.owed_rounds = cycle_end.saturating_sub(m.paid_through);
                p.owed = c * Uint128::from(p.owed_rounds) + m.arrears;
                p.from_lock = lock.min(p.owed);
                lock -= p.from_lock;
                p.to_attach = p.owed - p.from_lock;
                p.lock_refund = lock;
            } else {
                p.keeps_lock = true;
            }
        }
        p
    }

    /// Member `i` leaves the active roster (exit, admin ejection, engine
    /// ejection). For a settled-up voluntary exit the caller has verified that
    /// exactly `plan.to_attach` was attached.
    #[allow(clippy::too_many_arguments)]
    pub fn leave(
        &mut self,
        i: usize,
        at_round: u32,
        collected_through: u32,
        reason: &str,
        status: MemberStatus,
        by: &str,
        settle_up: bool,
    ) -> DeparturePlan {
        if !self.members[i].is_active() {
            return DeparturePlan::default();
        }
        let plan = self.departure_plan(i, at_round, collected_through, settle_up);
        let contribution = self.circle.contribution_amount;
        let at = at_round.max(1);
        let addr = self.members[i].address.clone();

        // Exit penalty → common pool.
        {
            let m = self.m(i);
            m.status = status;
            m.left_round = at;
            m.lock -= plan.penalty;
            m.fees_paid += plan.penalty;
        }
        self.circle.common_pool += plan.penalty;
        if !plan.penalty.is_zero() {
            self.add_penalty(&addr, at, plan.penalty, &format!("exit_penalty:{}", reason));
        }

        // Paid rounds beyond the liability → back to the member.
        if plan.prepaid_rounds > 0 {
            let m = self.m(i);
            m.paid_through -= plan.prepaid_rounds;
            m.contributed = m.contributed.saturating_sub(plan.prepaid_refund);
            m.paid_count = m.paid_count.saturating_sub(plan.prepaid_rounds);
            m.refunded += plan.prepaid_refund;
            self.refund(&addr, plan.prepaid_refund, "prepaid_rounds");
        }

        if !plan.served {
            {
                let m = self.m(i);
                m.arrears -= plan.arrears_offset;
                m.claim += plan.claim;
                m.claim_acquired += plan.claim;
                m.arrears -= plan.arrears_from_lock;
                m.lock -= plan.arrears_from_lock;
            }
            self.circle.total_arrears = self
                .circle
                .total_arrears
                .saturating_sub(plan.arrears_offset + plan.arrears_from_lock);
            self.circle.common_pool += plan.arrears_from_lock;
            if !plan.claim_now.is_zero() {
                self.circle.carry_pot -= plan.claim_now;
                self.pay_claim(i, plan.claim_now, at, "carry");
            }
        } else if settle_up {
            let first_new = self.members[i].paid_through + 1;
            let recovered = self.members[i].arrears;
            {
                let m = self.m(i);
                m.lock -= plan.from_lock;
                m.arrears = Uint128::zero();
                m.paid_through += plan.owed_rounds;
                m.contributed += contribution * Uint128::from(plan.owed_rounds);
                m.paid_count += plan.owed_rounds;
            }
            self.circle.total_arrears = self.circle.total_arrears.saturating_sub(recovered);
            self.circle.common_pool += recovered;
            for round in first_new..first_new + plan.owed_rounds {
                self.fx.deposits.push(DepositRecord {
                    member: addr.clone(),
                    cycle: round,
                    amount: contribution,
                    timestamp: self.now,
                    on_time: true,
                });
            }
        }
        if !plan.lock_refund.is_zero() {
            let m = self.m(i);
            m.lock -= plan.lock_refund;
            m.refunded += plan.lock_refund;
            self.refund(&addr, plan.lock_refund, "lock_returned_on_leave");
        }

        self.circle.members_list.retain(|a| a != addr);
        if self.circle.co_admin_address.as_ref() == Some(&addr) {
            self.circle.co_admin_address = None;
        }
        let verb = if status == MemberStatus::Exited { "member_exited" } else { "member_ejected" };
        self.fx.log(
            verb,
            format!(
                "Member {} left at round {} (reason: {}, by: {}, served: {}, penalty: {}, claim: {}, lock back: {}, settled: {})",
                addr, at, reason, by, plan.served, plan.penalty, plan.claim, plan.lock_refund, plan.owed
            ),
        );
        self.event(
            "safrimba_member_left",
            vec![
                ("member", addr.to_string()),
                ("round", at.to_string()),
                ("reason", reason.to_string()),
                ("by", by.to_string()),
                ("served", plan.served.to_string()),
                ("penalty", plan.penalty.to_string()),
                ("claim", plan.claim.to_string()),
                ("lock_refund", plan.lock_refund.to_string()),
                ("prepaid_refund", plan.prepaid_refund.to_string()),
                ("settled", plan.owed.to_string()),
            ],
        );
        plan
    }

    // ------------------------------------------------------------------
    // Ending
    // ------------------------------------------------------------------

    /// Terminal settlement (PLAN_V5 C10/C11): refund unsettled payments; an
    /// unfinished cycle is regularised (members not served get back what they
    /// put in — claims —, served participants get their gap filled), funded by
    /// the carry pot, the compensation reserve and the common pool, pro-rata if
    /// short; what is left is split equally among active members, arrears set
    /// off; every lock is returned. `exclude` is left out of the final split
    /// (the creator cancelling).
    pub fn close(
        &mut self,
        status: CircleStatus,
        reason: &str,
        exclude: Option<Addr>,
    ) -> Result<(), ContractError> {
        let contribution = self.circle.contribution_amount;
        let settled = self.circle.rounds_settled;
        let round = settled.max(1);

        // 1) Paid-but-unsettled rounds are refunded to their payers.
        for i in 0..self.members.len() {
            let ahead = self.members[i].paid_through.saturating_sub(settled);
            if ahead > 0 && self.members[i].slot != u32::MAX {
                let amount = contribution * Uint128::from(ahead);
                let m = self.m(i);
                m.paid_through = settled;
                m.contributed = m.contributed.saturating_sub(amount);
                m.paid_count = m.paid_count.saturating_sub(ahead);
                m.refunded += amount;
                let addr = m.address.clone();
                self.refund(&addr, amount, "unsettled_contribution");
            }
        }

        // 2) An unfinished cycle is regularised like a finished one: members not
        //    served in it claim what they put in; served participants are owed
        //    the gap between what they put in and what they received.
        let mut gaps: Vec<(usize, Uint128)> = vec![];
        if let (Some(clock), true) = (self.clock(), settled > 0) {
            let cycle = clock.cycle_of(settled);
            let n_in = settled - (cycle - 1) * clock.slots;
            let gross = contribution * Uint128::from(n_in);
            let unfinished = n_in < clock.slots;
            for i in 0..self.members.len() {
                let m = &self.members[i];
                if m.slot == u32::MAX || gross.is_zero() {
                    continue;
                }
                let served = m.last_received_cycle >= cycle;
                let left_this_cycle =
                    !m.is_active() && m.left_round > 0 && clock.cycle_of(m.left_round) == cycle;
                if m.is_active() && !served {
                    let offset = m.arrears.min(gross);
                    let mm = self.m(i);
                    mm.arrears -= offset;
                    mm.claim += gross - offset;
                    mm.claim_acquired += gross - offset;
                    self.circle.total_arrears = self.circle.total_arrears.saturating_sub(offset);
                } else if unfinished && served && (m.is_active() || left_this_cycle) {
                    let (short, received) = m.cycle_counters(cycle);
                    let gap = gross.saturating_sub(short).saturating_sub(received);
                    if !gap.is_zero() {
                        gaps.push((i, gap));
                    }
                }
            }
        }

        // 3) Claims and gaps first: carry pot, compensation reserve, common pool.
        let mut available =
            self.circle.carry_pot + self.circle.comp_pool + self.circle.common_pool;
        self.circle.carry_pot = Uint128::zero();
        self.circle.comp_pool = Uint128::zero();
        self.circle.common_pool = Uint128::zero();
        let mut needs: Vec<(usize, Uint128, bool)> = (0..self.members.len())
            .filter(|&j| !self.members[j].claim.is_zero())
            .map(|j| (j, self.members[j].claim, true))
            .collect();
        needs.extend(gaps.into_iter().map(|(j, g)| (j, g, false)));
        let total_needs: Uint128 = needs.iter().map(|(_, a, _)| *a).sum();
        if !total_needs.is_zero() && !available.is_zero() {
            let full = available >= total_needs;
            let mut paid = Uint128::zero();
            for (j, need, is_claim) in needs {
                let amount = if full {
                    need
                } else {
                    need.multiply_ratio(available, total_needs)
                };
                paid += amount;
                if is_claim {
                    self.pay_claim(j, amount, round, "close");
                } else {
                    self.pay_member(j, amount, round, None, "compensation");
                }
            }
            available -= paid;
            // Arrears those payments recovered landed in the common pool: still to share.
            available += self.circle.common_pool;
            self.circle.common_pool = Uint128::zero();
        }

        // 4) What is left → equal split among active members, arrears set off.
        if !available.is_zero() {
            let actives: Vec<usize> = (0..self.members.len())
                .filter(|&j| self.members[j].is_active())
                .collect();
            let mut sharers: Vec<usize> = actives
                .iter()
                .copied()
                .filter(|&j| exclude.as_ref() != Some(&self.members[j].address))
                .collect();
            if sharers.is_empty() {
                sharers = actives;
            }
            if sharers.is_empty() {
                sharers = (0..self.members.len()).collect();
            }
            self.split_with_set_off(&sharers, available, round);
        }

        // 5) Every remaining lock goes back to its owner.
        for i in 0..self.members.len() {
            let amount = self.members[i].lock;
            if amount.is_zero() {
                continue;
            }
            let m = self.m(i);
            m.lock = Uint128::zero();
            m.refunded += amount;
            let addr = m.address.clone();
            self.refund(&addr, amount, "lock_returned");
        }

        // 6) Claims the circle could not fund are extinguished.
        for i in 0..self.members.len() {
            if !self.members[i].claim.is_zero() {
                self.forfeit_claim(i, round);
            }
        }

        let final_status = if status == CircleStatus::Completed
            && !self.circle.total_pending_payouts.is_zero()
        {
            CircleStatus::Finalizing
        } else {
            status
        };
        self.set_status_reason(final_status, reason);
        self.circle.closed_at = Some(self.now);
        self.circle.paused_at = None;
        self.circle.pause_reason = None;
        self.circle.end_after_cycle = None;
        self.fx.closed_now = true;
        self.fx.log("circle_completed", format!("Circle closed ({})", reason));
        self.event("safrimba_closed", vec![("reason", reason.to_string())]);
        Ok(())
    }

    /// Split `pool` among `sharers` with arrears set off. Recovered arrears
    /// belong to everyone, so every sharer is entitled to the same level
    /// `e = (pool + Σ debts) / n` and receives `e − debt`; a sharer whose debt
    /// exceeds the level receives nothing and their debt drops by `e`
    /// (water-filling). Rounding goes to the primary admin.
    fn split_with_set_off(&mut self, sharers: &[usize], pool: Uint128, round: u32) {
        if sharers.is_empty() || pool.is_zero() {
            return;
        }
        let mut set: Vec<usize> = sharers.to_vec();
        let level = loop {
            let debts: Uint128 = set.iter().map(|&j| self.members[j].arrears).sum();
            let e = (pool + debts).multiply_ratio(1u128, set.len() as u128);
            let before = set.len();
            set.retain(|&j| self.members[j].arrears < e);
            if set.len() == before || set.is_empty() {
                break e;
            }
        };
        let mut cash: Vec<(usize, Uint128)> = vec![];
        let mut paid = Uint128::zero();
        for &j in sharers {
            let debt = self.members[j].arrears;
            let (amount, cleared) = if debt < level {
                (level - debt, debt)
            } else {
                (Uint128::zero(), level)
            };
            if !cleared.is_zero() {
                self.m(j).arrears -= cleared;
                self.circle.total_arrears = self.circle.total_arrears.saturating_sub(cleared);
            }
            paid += amount;
            cash.push((j, amount));
        }
        let dust = pool.saturating_sub(paid);
        if !dust.is_zero() {
            let admin = self.admin();
            let target = cash
                .iter()
                .position(|(j, a)| Some(&self.members[*j].address) == admin.as_ref() && !a.is_zero())
                .or_else(|| cash.iter().position(|(_, a)| !a.is_zero()))
                .unwrap_or(0);
            cash[target].1 += dust;
        }
        for (j, amount) in cash {
            if amount.is_zero() {
                continue;
            }
            let m = self.m(j);
            m.received += amount;
            let addr = m.address.clone();
            self.fx.send(&addr, amount);
            self.add_payout(round, &addr, amount);
            self.event(
                "safrimba_payout",
                vec![
                    ("round", round.to_string()),
                    ("recipient", addr.to_string()),
                    ("gross", amount.to_string()),
                    ("platform_fee", "0".to_string()),
                    ("arrears_recovered", "0".to_string()),
                    ("lock_refilled", "0".to_string()),
                    ("net", amount.to_string()),
                    ("kind", "close_share".to_string()),
                ],
            );
        }
    }

    /// Cancel before start: every lock goes back.
    pub fn cancel_before_start(&mut self, reason: &str) -> Result<(), ContractError> {
        for i in 0..self.members.len() {
            let amount = self.members[i].lock;
            let m = self.m(i);
            m.lock = Uint128::zero();
            m.refunded += amount;
            let addr = m.address.clone();
            self.refund(&addr, amount, reason);
        }
        self.set_status_reason(CircleStatus::Cancelled, reason);
        self.circle.closed_at = Some(self.now);
        self.fx.closed_now = true;
        self.fx.log("circle_cancelled", format!("Circle cancelled ({})", reason));
        self.event("safrimba_closed", vec![("reason", reason.to_string())]);
        Ok(())
    }

    /// Cancel a started circle that has not distributed yet (PLAN_V5 3.6):
    /// everyone gets back what they put in; the creator pays the standard exit
    /// penalty (not the whole lock), shared with the pool among the others.
    pub fn cancel_running(&mut self) -> Result<(), ContractError> {
        if !self.circle.total_distributed.is_zero() {
            return Err(ContractError::CancelNotAllowedAfterDistribution {});
        }
        let creator = self.circle.creator_address.clone();
        if let Some(i) = self.index_of(&creator) {
            let bps = self.circle.exit_penalty_percent;
            let m = self.m(i);
            let penalty = m.lock_original.multiply_ratio(bps, 10_000u64).min(m.lock);
            m.lock -= penalty;
            m.fees_paid += penalty;
            self.circle.common_pool += penalty;
            if !penalty.is_zero() {
                let round = self.circle.rounds_settled.max(1);
                self.add_penalty(&creator, round, penalty, "cancel_penalty");
            }
        }
        self.fx
            .log("circle_cancelled", "Circle cancelled after start by the creator".to_string());
        self.close(CircleStatus::Cancelled, "cancelled_by_creator", Some(creator))
    }

    // ------------------------------------------------------------------
    // Records
    // ------------------------------------------------------------------

    fn refund(&mut self, to: &Addr, amount: Uint128, reason: &str) {
        if amount.is_zero() {
            return;
        }
        self.fx.send(to, amount);
        self.record_refund(to, amount, reason);
        self.event(
            "safrimba_refund",
            vec![
                ("member", to.to_string()),
                ("amount", amount.to_string()),
                ("reason", reason.to_string()),
            ],
        );
    }

    fn record_refund(&mut self, to: &Addr, amount: Uint128, reason: &str) {
        let now = self.now;
        let rec = self.fx.refunds.entry(to.clone()).or_insert(RefundRecord {
            member: to.clone(),
            amount: Uint128::zero(),
            reason: String::new(),
            timestamp: now,
        });
        rec.amount += amount;
        if !rec.reason.split('+').any(|r| r == reason) {
            if !rec.reason.is_empty() {
                rec.reason.push('+');
            }
            rec.reason.push_str(reason);
        }
    }

    fn add_payout(&mut self, round: u32, to: &Addr, amount: Uint128) {
        let now = self.now;
        let rec = self.fx.payouts.entry((round, to.clone())).or_insert(PayoutRecord {
            cycle: round,
            recipient: to.clone(),
            amount: Uint128::zero(),
            timestamp: now,
            transaction_hash: None,
        });
        rec.amount += amount;
    }

    fn add_penalty(&mut self, member: &Addr, round: u32, amount: Uint128, reason: &str) {
        let now = self.now;
        let rec = self
            .fx
            .penalties
            .entry((member.clone(), round))
            .or_insert(PenaltyRecord {
                member: member.clone(),
                cycle: round,
                amount: Uint128::zero(),
                reason: String::new(),
                timestamp: now,
            });
        rec.amount += amount;
        if !rec.reason.is_empty() {
            rec.reason.push('+');
        }
        rec.reason.push_str(reason);
    }

    pub fn log(&mut self, kind: &str, data: String) {
        self.fx.log(kind, data);
    }

    pub fn emit(&mut self, ty: &str, attrs: Vec<(&str, String)>) {
        self.event(ty, attrs);
    }

    pub fn push_send(&mut self, to: &Addr, amount: Uint128) {
        self.fx.send(to, amount);
    }

    // ------------------------------------------------------------------
    // Legacy display fields
    // ------------------------------------------------------------------

    pub fn refresh_legacy_fields(&mut self) {
        let locks: Uint128 = self.members.iter().map(|m| m.lock).sum();
        let creator_lock = self
            .member(&self.circle.creator_address)
            .map(|m| m.lock)
            .unwrap_or_default();
        let clock = self.clock();
        let now_s = self.now_s();
        let terminal = self.circle.is_terminal();
        let c = &mut self.circle;
        c.total_amount_locked = locks;
        c.creator_lock_amount = creator_lock;
        c.total_penalties_collected = c.common_pool;
        c.engine_version = crate::state::ENGINE_VERSION;
        c.auto_payout_enabled = true;
        c.manual_trigger_enabled = false;
        c.emergency_stop_triggered = c.emergency_stop_triggered && c.paused_at.is_some();
        if let Some(clock) = clock {
            let total = clock.total_rounds;
            let focus = (c.rounds_settled + 1).min(total.max(1));
            c.current_cycle_index = if terminal { c.rounds_settled.max(1) } else { focus };
            c.cycles_completed = c.rounds_settled / clock.slots;
            c.next_payout_date = if terminal {
                None
            } else {
                Some(Timestamp::from_seconds(clock.to_wall(clock.opens(focus), now_s)))
            };
            c.end_date = Some(Timestamp::from_seconds(clock.to_wall(clock.due(total), now_s)));
            let paid: Vec<Addr> = self
                .members
                .iter()
                .filter(|m| m.is_active() && m.paid_through >= focus && m.paid_through > c.rounds_settled)
                .map(|m| m.address.clone())
                .collect();
            c.members_paid_this_cycle = paid;
            c.members_late_this_cycle = vec![];
        }
        c.updated_at = self.now;
    }

    // ------------------------------------------------------------------
    // Persistence
    // ------------------------------------------------------------------

    /// Write the model and return the bank sends (as sub-messages that report
    /// failures to `reply`) and the events to attach.
    pub fn persist(
        mut self,
        storage: &mut dyn Storage,
    ) -> Result<(Vec<SubMsg>, Vec<Event>), ContractError> {
        self.refresh_legacy_fields();
        let id = self.circle.circle_id;
        CIRCLES.save(storage, id, &self.circle)?;
        for (i, m) in self.members.iter().enumerate() {
            if self.dirty[i] {
                MEMBERS.save(storage, (id, &m.address), m)?;
            }
        }
        for a in &self.removed {
            MEMBERS.remove(storage, (id, a));
        }
        for d in &self.fx.deposits {
            DEPOSITS.save(storage, (id, d.member.clone(), d.cycle), d)?;
        }
        for ((round, addr), rec) in &self.fx.payouts {
            let key = (id, *round, addr.clone());
            let merged = match PAYOUTS.may_load(storage, key.clone())? {
                Some(mut old) => {
                    old.amount += rec.amount;
                    old.timestamp = rec.timestamp;
                    old
                }
                None => rec.clone(),
            };
            PAYOUTS.save(storage, key, &merged)?;
        }
        for ((addr, round), rec) in &self.fx.penalties {
            let key = (id, addr.clone(), *round);
            let merged = match PENALTIES.may_load(storage, key.clone())? {
                Some(mut old) => {
                    old.amount += rec.amount;
                    old.reason = format!("{}+{}", old.reason, rec.reason);
                    old
                }
                None => rec.clone(),
            };
            PENALTIES.save(storage, key, &merged)?;
        }
        for (addr, rec) in &self.fx.refunds {
            let key = (id, addr.clone());
            let merged = match REFUNDS.may_load(storage, key.clone())? {
                Some(mut old) => {
                    old.amount += rec.amount;
                    old.reason = format!("{}+{}", old.reason, rec.reason);
                    old.timestamp = rec.timestamp;
                    old
                }
                None => rec.clone(),
            };
            REFUNDS.save(storage, key, &merged)?;
        }
        let mut counter = EVENT_COUNTER.may_load(storage, id)?.unwrap_or(0);
        for (kind, data) in &self.fx.logs {
            counter += 1;
            EVENTS.save(
                storage,
                (id, counter),
                &EventLog {
                    event_type: kind.clone(),
                    circle_id: id,
                    data: data.clone(),
                    timestamp: self.now,
                },
            )?;
        }
        EVENT_COUNTER.save(storage, id, &counter)?;

        let denom = self.circle.denomination.clone();
        let sends: Vec<(Addr, Uint128)> = self
            .fx
            .sends
            .iter()
            .filter(|(_, a)| !a.is_zero())
            .cloned()
            .collect();
        let mut msgs: Vec<SubMsg> = vec![];
        if !sends.is_empty() {
            SEND_BATCH.save(
                storage,
                &SendBatch {
                    circle_id: id,
                    denom: denom.clone(),
                    sends: sends.clone(),
                },
            )?;
            for (n, (to, amount)) in sends.iter().enumerate() {
                msgs.push(SubMsg::reply_on_error(
                    BankMsg::Send {
                        to_address: to.to_string(),
                        amount: vec![Coin {
                            denom: denom.clone(),
                            amount: *amount,
                        }],
                    },
                    n as u64,
                ));
            }
        }
        Ok((msgs, self.fx.events))
    }

    /// Money the contract must hold for this circle right now.
    pub fn obligations(&self) -> Uint128 {
        let settled = self.circle.rounds_settled;
        let contribution = self.circle.contribution_amount;
        let unsettled: Uint128 = self
            .members
            .iter()
            .filter(|m| m.slot != u32::MAX)
            .map(|m| contribution * Uint128::from(m.paid_through.saturating_sub(settled)))
            .sum();
        let locks: Uint128 = self.members.iter().map(|m| m.lock).sum();
        locks
            + self.circle.carry_pot
            + self.circle.common_pool
            + self.circle.comp_pool
            + unsettled
            + self.circle.total_pending_payouts
    }
}

/// Strikes allowed before auto-ejection: floor((10000 − exit) / late), at most rounds − 1.
pub fn compute_max_missed(exit_penalty_bps: u64, late_fee_bps: u64, total_rounds: u32) -> u32 {
    let base = if late_fee_bps == 0 {
        u32::MAX
    } else {
        (10_000u64.saturating_sub(exit_penalty_bps) / late_fee_bps).min(u32::MAX as u64) as u32
    };
    let cap = total_rounds.saturating_sub(1).max(1);
    base.min(cap).max(1)
}
