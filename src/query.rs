//! Queries (engine v5). Every circle query loads the model and applies the
//! clock in memory first ("virtual sync"), so responses describe the circle at
//! the query's block time — the UI never shows a stale round, even if nobody
//! has sent a transaction since the deadline.

use std::collections::BTreeMap;

use cosmwasm_std::{Addr, Deps, Env, Order, StdError, StdResult, Timestamp, Uint128};

use crate::engine::clock::{kind_of, max_pause_secs, payout_round_in_cycle, Clock, Kind};
use crate::engine::{Model, MAX_ROUNDS_PER_QUERY, MAX_ROUNDS_PER_TX};
use crate::msg::{
    AccumulatedLateFeesResponse, ArchivedDateResponse, BalanceResponse, BlockedMembersResponse,
    CalendarRound, CircleResponse, CircleStatsResponse, CircleViewResponse, CirclesResponse,
    CycleResponse, DepositRequirementResponse, DepositsResponse, DistributionCalendarResponse,
    EndCyclePreviewResponse, EventsResponse, ExitQuote, LockReserveResponse, LockReservesResponse,
    MemberDue, MemberLockedAmountResponse, MemberPseudonymResponse, MemberStatsResponse,
    MemberView, MembersResponse, PayoutsResponse, PenaltiesResponse, PendingPayoutResponse,
    PrivateMembersResponse, RefundsResponse, RoundKind, RoundPhase, RoundView, StatusResponse,
    WalletReputationResponse,
};
use crate::state::{
    CircleStatus, DepositRecord, MemberState, PayoutRecord, CIRCLES, DEPOSITS, EVENTS,
    EVENT_COUNTER, MEMBER_PSEUDONYMS, MEMBER_RATINGS, PAYOUTS, PENALTIES, PENDING_PAYOUTS,
    PRIVATE_MEMBER_LIST, REFUNDS, WALLET_RATING_COUNT, WALLET_RATING_INDEX,
};

fn std_err(e: impl ToString) -> StdError {
    StdError::generic_err(e.to_string())
}

/// Load + virtual sync.
pub fn live(deps: Deps, env: &Env, circle_id: u64) -> StdResult<Model> {
    let mut model = Model::load(deps.storage, env, circle_id).map_err(std_err)?;
    model.sync(MAX_ROUNDS_PER_QUERY).map_err(std_err)?;
    Ok(model)
}

fn ts(secs: u64) -> Timestamp {
    Timestamp::from_seconds(secs)
}

// ---------------------------------------------------------------------------
// Live view
// ---------------------------------------------------------------------------

fn kind_label(kind: Kind) -> RoundKind {
    match kind {
        Kind::Accumulate => RoundKind::Accumulate,
        Kind::Turn(_) => RoundKind::Turn,
        Kind::Group(_) => RoundKind::Group,
    }
}

fn planned_recipients(model: &Model, clock: &Clock, k: u32, live_only: bool) -> (Kind, Vec<Addr>) {
    let r = clock.round_in_cycle(k);
    let cycle = clock.cycle_of(k);
    let kind = kind_of(r, clock.slots, model.ts());
    let eligible = |m: &MemberState| !live_only || (m.is_active() && m.last_received_cycle < cycle);
    let list = match kind {
        Kind::Accumulate => vec![],
        Kind::Turn(idx) => model
            .members
            .iter()
            .filter(|m| m.slot == idx && eligible(m))
            .map(|m| m.address.clone())
            .collect(),
        Kind::Group(from) => model
            .members
            .iter()
            .filter(|m| m.slot != u32::MAX && m.slot >= from && eligible(m))
            .map(|m| m.address.clone())
            .collect(),
    };
    (kind, list)
}

fn round_view(model: &Model, clock: &Clock, k: u32) -> RoundView {
    let now_s = model.now_s();
    let eff = clock.eff(now_s);
    let settled = model.circle.rounds_settled;
    let phase = if k <= settled {
        RoundPhase::Settled
    } else if eff < clock.opens(k) {
        RoundPhase::Upcoming
    } else if eff < clock.due(k) {
        RoundPhase::Open
    } else if eff < clock.settles(k) {
        RoundPhase::Grace
    } else {
        RoundPhase::Settling
    };
    let (kind, recipients) = planned_recipients(model, clock, k, k > settled);
    let contribution = model.circle.contribution_amount;
    let cycle = clock.cycle_of(k);
    let paid: Vec<Addr> = model
        .members
        .iter()
        .filter(|m| m.is_active() && k > settled && m.paid_through >= k)
        .map(|m| m.address.clone())
        .collect();
    let payers = model
        .members
        .iter()
        .filter(|m| k > settled && m.slot != u32::MAX && m.paid_through >= k)
        .count() as u128;
    // Members expected to fund this round: actives, plus members who left after
    // being served in this cycle and whose lock still covers them.
    let liable = model
        .members
        .iter()
        .filter(|m| {
            let left_cycle = clock.cycle_of(m.left_round.max(1));
            m.slot != u32::MAX
                && (m.is_active()
                    || (left_cycle == cycle
                        && m.last_received_cycle >= left_cycle
                        && !m.lock.is_zero()))
        })
        .count() as u128;
    let mut expected_pot = contribution * Uint128::from(liable);
    if matches!(kind, Kind::Group(_)) {
        expected_pot += model.circle.carry_pot;
    }
    RoundView {
        round: k,
        cycle,
        round_in_cycle: clock.round_in_cycle(k),
        kind: kind_label(kind),
        phase,
        opens_at: ts(clock.to_wall(clock.opens(k), now_s)),
        due_at: ts(clock.to_wall(clock.due(k), now_s)),
        settles_at: ts(clock.to_wall(clock.settles(k), now_s)),
        recipients,
        paid,
        expected_count: model.active_count(),
        collected: contribution * Uint128::from(payers),
        expected_pot,
    }
}

fn member_view(
    model: &Model,
    clock: Option<&Clock>,
    m: &MemberState,
    pending: Uint128,
) -> MemberView {
    let running = matches!(
        model.circle.circle_status,
        CircleStatus::Running | CircleStatus::Paused
    );
    let payout_round = clock.and_then(|c| {
        if !m.is_active() || m.slot == u32::MAX || model.circle.is_terminal() {
            return None;
        }
        let focus = (model.circle.rounds_settled + 1).min(c.total_rounds);
        let cycle = c.cycle_of(focus.max(1));
        let r = payout_round_in_cycle(m.slot, c.slots, model.ts());
        Some((cycle - 1) * c.slots + r)
    });
    let cycle = model.current_cycle();
    MemberView {
        address: m.address.clone(),
        slot: if m.slot == u32::MAX { None } else { Some(m.slot) },
        status: m.status,
        left_round: if m.left_round == 0 { None } else { Some(m.left_round) },
        lock: m.lock,
        lock_original: m.lock_original,
        arrears: m.arrears,
        paid_through: m.paid_through,
        missed_total: m.missed_total,
        strikes: m.strikes,
        contributed: m.contributed,
        received: m.received,
        refunded: m.refunded,
        fees_paid: m.fees_paid,
        last_received_cycle: m.last_received_cycle,
        payout_round_this_cycle: payout_round,
        claim: m.claim,
        late_count: m.late_count,
        served_this_cycle: cycle > 0 && m.last_received_cycle >= cycle,
        pending_payout: pending,
        can_be_ejected: running
            && m.is_active()
            && model.circle.manual_eject_enabled()
            && (m.strikes > 0 || !m.arrears.is_zero()),
        exit_quote: None,
    }
}

/// What `ExitCircle` would do for member `i` (same code path as the execution).
fn exit_quote(model: &Model, i: usize, stored_behind: u32) -> ExitQuote {
    let m = &model.members[i];
    let blocked = |reason: &str| ExitQuote {
        allowed: false,
        blocked_reason: Some(reason.to_string()),
        served_this_cycle: false,
        penalty: Uint128::zero(),
        owed: Uint128::zero(),
        from_lock: Uint128::zero(),
        to_attach: Uint128::zero(),
        lock_refund: Uint128::zero(),
        prepaid_refund: Uint128::zero(),
        claim: Uint128::zero(),
        claim_paid_now: Uint128::zero(),
    };
    if !m.is_active() {
        return blocked("not_active");
    }
    match model.circle.circle_status {
        CircleStatus::Draft | CircleStatus::Open | CircleStatus::Full => {
            if m.address == model.circle.creator_address {
                return blocked("creator_before_start");
            }
            ExitQuote {
                allowed: true,
                blocked_reason: None,
                lock_refund: m.lock,
                ..blocked("")
            }
        }
        CircleStatus::Running | CircleStatus::Paused => {
            let settled = model.circle.rounds_settled;
            let plan = model.departure_plan(i, settled + 1, settled, true);
            let reason = if stored_behind > MAX_ROUNDS_PER_TX {
                Some("catching_up".to_string())
            } else if model.circle.strict_mode && !model.at_cycle_boundary() {
                Some("strict_mode".to_string())
            } else {
                None
            };
            ExitQuote {
                allowed: reason.is_none(),
                blocked_reason: reason,
                served_this_cycle: plan.served,
                penalty: plan.penalty,
                owed: plan.owed,
                from_lock: plan.from_lock,
                to_attach: plan.to_attach,
                lock_refund: plan.lock_refund,
                prepaid_refund: plan.prepaid_refund,
                claim: plan.claim,
                claim_paid_now: plan.claim_now,
            }
        }
        _ => blocked("status"),
    }
}

fn member_due(
    model: &Model,
    clock: Option<&Clock>,
    m: &MemberState,
    stored_behind: u32,
) -> Option<MemberDue> {
    if !m.is_active() {
        return None;
    }
    let contribution = model.circle.contribution_amount;
    let max = contribution + m.arrears + m.refill_needed();
    let strikes_left = model
        .circle
        .max_missed_payments_allowed
        .saturating_sub(m.strikes);
    let running = matches!(
        model.circle.circle_status,
        CircleStatus::Running | CircleStatus::Paused
    );
    let (next_round, can_pay, opens_at, due_at) = match clock {
        Some(c) if running => {
            let target = m.paid_through + 1;
            if target > c.total_rounds {
                (None, false, None, None)
            } else {
                let now_s = model.now_s();
                (
                    Some(target),
                    // A deposit catches up at most MAX_ROUNDS_PER_TX rounds first.
                    c.is_open(target, now_s)
                        && model.rounds_behind() == 0
                        && stored_behind <= MAX_ROUNDS_PER_TX,
                    Some(ts(c.to_wall(c.opens(target), now_s))),
                    Some(ts(c.to_wall(c.due(target), now_s))),
                )
            }
        }
        _ => (None, false, None, None),
    };
    Some(MemberDue {
        next_round,
        can_pay_now: can_pay,
        opens_at,
        due_at,
        min_payment: contribution,
        max_payment: max,
        arrears: m.arrears,
        lock_refill: m.refill_needed(),
        strikes_left,
    })
}

pub fn query_circle_view(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Option<Addr>,
) -> StdResult<CircleViewResponse> {
    let stored_settled = CIRCLES.load(deps.storage, circle_id)?.rounds_settled;
    let model = live(deps, &env, circle_id)?;
    let stored_behind = model.circle.rounds_settled.saturating_sub(stored_settled);
    let clock = model.clock();
    let c = &model.circle;
    let running = matches!(c.circle_status, CircleStatus::Running | CircleStatus::Paused);
    let pending: BTreeMap<Addr, Uint128> = PENDING_PAYOUTS
        .prefix(circle_id)
        .range(deps.storage, None, None, Order::Ascending)
        .collect::<StdResult<_>>()?;
    let pending_of = |a: &Addr| pending.get(a).copied().unwrap_or_default();
    let (round, next_round) = match (&clock, running) {
        (Some(clock), true) => {
            let focus = c.rounds_settled + 1;
            let r1 = (focus <= clock.total_rounds).then(|| round_view(&model, clock, focus));
            let r2 = (focus < clock.total_rounds).then(|| round_view(&model, clock, focus + 1));
            (r1, r2)
        }
        _ => (None, None),
    };
    let tick_due = model.fx.rounds_settled_now > 0 || model.fx.started_now || model.fx.closed_now;
    let members: Vec<MemberView> = model
        .members
        .iter()
        .map(|m| member_view(&model, clock.as_ref(), m, pending_of(&m.address)))
        .collect();
    let me_index = member.as_ref().and_then(|a| model.index_of(a));
    let me = me_index.map(|i| {
        let m = &model.members[i];
        let mut v = member_view(&model, clock.as_ref(), m, pending_of(&m.address));
        v.exit_quote = Some(exit_quote(&model, i, stored_behind));
        v
    });
    let my_due = me_index
        .and_then(|i| member_due(&model, clock.as_ref(), &model.members[i], stored_behind));
    let now_s = model.now_s();
    let paused_until = if c.circle_status == CircleStatus::Paused {
        clock.and_then(|k| k.paused_until()).map(ts)
    } else {
        None
    };
    let max_pause = clock
        .map(|k| k.max_pause)
        .unwrap_or_else(|| max_pause_secs(c.cycle_duration_secs()));
    let pause_left = clock
        .map(|k| k.pause_budget().saturating_sub(k.paused_secs_at(now_s)))
        .unwrap_or(max_pause * crate::engine::clock::PAUSE_BUDGET_FACTOR);
    Ok(CircleViewResponse {
        engine: crate::state::ENGINE_VERSION,
        circle_id,
        status: c.circle_status.clone(),
        now: model.now,
        paused: c.paused_at.is_some(),
        denomination: c.denomination.clone(),
        contribution: c.contribution_amount,
        slots: c.members_at_start.unwrap_or(0),
        total_rounds: clock.map(|k| k.total_rounds).unwrap_or(0),
        rounds_settled: c.rounds_settled,
        total_cycles: c.total_cycles,
        cycles_completed: c.cycles_completed,
        round_secs: c.cycle_duration_secs(),
        grace_secs: c.grace_period_secs(),
        threshold_start: model.ts(),
        max_strikes: c.max_missed_payments_allowed,
        ejection_policy: c.ejection_policy.clone(),
        start_at: c.first_cycle_date,
        end_at: c.end_date,
        auto_start_at: if c.is_started() {
            None
        } else {
            c.auto_start_date
                .filter(|_| c.auto_start_type.as_deref() == Some("by_date"))
        },
        round,
        next_round,
        next_action_at: model.next_action_at(),
        tick_due,
        rounds_behind: stored_behind,
        carry_pot: c.carry_pot,
        common_pool: c.common_pool,
        locks_total: c.total_amount_locked,
        total_distributed: c.total_distributed,
        total_arrears: c.total_arrears,
        active_members: model.active_count(),
        members,
        me,
        my_due,
        current_cycle: model.current_cycle(),
        paused_until,
        pause_reason: if c.circle_status == CircleStatus::Paused {
            c.pause_reason.clone()
        } else {
            None
        },
        max_pause_secs: max_pause,
        pause_secs_left: pause_left,
        ending_after_cycle: c.end_after_cycle,
        end_now_allowed: model.end_now_allowed(),
        admins: model.admins(),
        creator: c.creator_address.clone(),
        co_admin: c.co_admin_address.clone(),
        strict_mode: c.strict_mode,
        exit_penalty_bps: c.exit_penalty_percent,
        late_fee_bps: c.late_fee_percent,
        platform_fee_bps: c.platform_fee_percent,
        pending_payouts_total: c.total_pending_payouts,
        comp_pool: c.comp_pool,
        emergency_stop_enabled: c.emergency_stop_enabled,
    })
}

// ---------------------------------------------------------------------------
// v4-compatible queries
// ---------------------------------------------------------------------------

pub fn query_circle(deps: Deps, env: Env, circle_id: u64) -> StdResult<CircleResponse> {
    Ok(CircleResponse {
        circle: live(deps, &env, circle_id)?.circle,
    })
}

pub fn query_circles(
    deps: Deps,
    env: Env,
    start_after: Option<u64>,
    limit: Option<u32>,
    status: Option<CircleStatus>,
    creator: Option<Addr>,
) -> StdResult<CirclesResponse> {
    let limit = limit.unwrap_or(30).min(100) as usize;
    let ids: Vec<u64> = CIRCLES
        .keys(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .filter(|id| start_after.map(|s| *id > s).unwrap_or(true))
        .take(limit)
        .collect();
    let mut circles = vec![];
    for id in ids {
        let c = live(deps, &env, id)?.circle;
        if status.as_ref().map(|s| &c.circle_status == s).unwrap_or(true)
            && creator.as_ref().map(|a| c.creator_address == a).unwrap_or(true)
        {
            circles.push(c);
        }
    }
    Ok(CirclesResponse { circles })
}

pub fn query_circle_members(deps: Deps, env: Env, circle_id: u64) -> StdResult<MembersResponse> {
    let c = live(deps, &env, circle_id)?.circle;
    Ok(MembersResponse {
        members: c.members_list,
        pending_members: c.pending_members,
    })
}

pub fn query_circle_status(deps: Deps, env: Env, circle_id: u64) -> StdResult<StatusResponse> {
    Ok(StatusResponse {
        status: live(deps, &env, circle_id)?.circle.circle_status,
    })
}

pub fn query_current_cycle(deps: Deps, env: Env, circle_id: u64) -> StdResult<CycleResponse> {
    let c = live(deps, &env, circle_id)?.circle;
    Ok(CycleResponse {
        current_cycle: c.current_cycle_index,
        total_cycles: c.total_cycles,
        next_payout_date: c.next_payout_date,
        members_paid: c.members_paid_this_cycle,
        members_late: c.members_late_this_cycle,
    })
}

fn merged_deposits(deps: Deps, model: &Model, filter: impl Fn(&DepositRecord) -> bool) -> StdResult<Vec<DepositRecord>> {
    let id = model.circle.circle_id;
    let mut out: Vec<DepositRecord> = vec![];
    for m in &model.members {
        for item in DEPOSITS
            .prefix((id, m.address.clone()))
            .range(deps.storage, None, None, Order::Ascending)
        {
            let (_, d) = item?;
            if filter(&d) {
                out.push(d);
            }
        }
    }
    for d in &model.fx.deposits {
        if filter(d) {
            out.push(d.clone());
        }
    }
    out.sort_by(|a, b| a.cycle.cmp(&b.cycle).then(a.timestamp.cmp(&b.timestamp)));
    Ok(out)
}

pub fn query_cycle_deposits(
    deps: Deps,
    env: Env,
    circle_id: u64,
    cycle: u32,
) -> StdResult<DepositsResponse> {
    let model = live(deps, &env, circle_id)?;
    Ok(DepositsResponse {
        deposits: merged_deposits(deps, &model, |d| d.cycle == cycle)?,
    })
}

pub fn query_member_deposits(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<DepositsResponse> {
    let model = live(deps, &env, circle_id)?;
    Ok(DepositsResponse {
        deposits: merged_deposits(deps, &model, |d| d.member == member)?,
    })
}

fn merged_payouts(deps: Deps, model: &Model) -> StdResult<Vec<PayoutRecord>> {
    let id = model.circle.circle_id;
    let mut out: Vec<PayoutRecord> = PAYOUTS
        .sub_prefix(id)
        .range(deps.storage, None, None, Order::Ascending)
        .map(|r| r.map(|(_, p)| p))
        .collect::<StdResult<_>>()?;
    for ((round, addr), rec) in &model.fx.payouts {
        if let Some(existing) = out
            .iter_mut()
            .find(|p| p.cycle == *round && p.recipient == addr)
        {
            existing.amount += rec.amount;
        } else {
            out.push(rec.clone());
        }
    }
    out.sort_by(|a, b| a.cycle.cmp(&b.cycle).then(a.recipient.cmp(&b.recipient)));
    Ok(out)
}

pub fn query_payouts(deps: Deps, env: Env, circle_id: u64) -> StdResult<PayoutsResponse> {
    let model = live(deps, &env, circle_id)?;
    Ok(PayoutsResponse {
        payouts: merged_payouts(deps, &model)?,
    })
}

pub fn query_payout_history(
    deps: Deps,
    env: Env,
    circle_id: u64,
    cycle: Option<u32>,
) -> StdResult<PayoutsResponse> {
    let model = live(deps, &env, circle_id)?;
    let payouts = merged_payouts(deps, &model)?
        .into_iter()
        .filter(|p| cycle.map(|c| p.cycle == c).unwrap_or(true))
        .collect();
    Ok(PayoutsResponse { payouts })
}

pub fn query_circle_balance(deps: Deps, env: Env, circle_id: u64) -> StdResult<BalanceResponse> {
    let c = CIRCLES.load(deps.storage, circle_id)?;
    let balance = deps
        .querier
        .query_balance(&env.contract.address, &c.denomination)?
        .amount;
    Ok(BalanceResponse { balance })
}

pub fn query_member_balance(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<BalanceResponse> {
    let model = live(deps, &env, circle_id)?;
    let balance = model
        .member(&member)
        .map(|m| m.contributed.saturating_sub(m.received))
        .unwrap_or_default();
    Ok(BalanceResponse { balance })
}

pub fn query_penalties(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Option<Addr>,
) -> StdResult<PenaltiesResponse> {
    let model = live(deps, &env, circle_id)?;
    let mut penalties: Vec<_> = PENALTIES
        .sub_prefix(circle_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok().map(|(_, p)| p))
        .collect();
    penalties.extend(model.fx.penalties.values().cloned());
    let penalties = penalties
        .into_iter()
        .filter(|p| member.as_ref().map(|m| p.member == m).unwrap_or(true))
        .collect();
    Ok(PenaltiesResponse { penalties })
}

pub fn query_refunds(deps: Deps, env: Env, circle_id: u64) -> StdResult<RefundsResponse> {
    let model = live(deps, &env, circle_id)?;
    let mut refunds: Vec<_> = REFUNDS
        .prefix(circle_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok().map(|(_, p)| p))
        .collect();
    for rec in model.fx.refunds.values() {
        if let Some(existing) = refunds.iter_mut().find(|r| r.member == rec.member) {
            existing.amount += rec.amount;
        } else {
            refunds.push(rec.clone());
        }
    }
    Ok(RefundsResponse { refunds })
}

pub fn query_events(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    limit: Option<u32>,
) -> StdResult<EventsResponse> {
    let limit = limit.unwrap_or(100).min(1000) as u64;
    let count = EVENT_COUNTER.may_load(deps.storage, circle_id)?.unwrap_or(0);
    let start = if count > limit { count - limit + 1 } else { 1 };
    let mut events = vec![];
    for id in start..=count {
        if let Some(e) = EVENTS.may_load(deps.storage, (circle_id, id))? {
            events.push(e);
        }
    }
    Ok(EventsResponse { events })
}

pub fn query_circle_stats(deps: Deps, env: Env, circle_id: u64) -> StdResult<CircleStatsResponse> {
    let model = live(deps, &env, circle_id)?;
    let c = &model.circle;
    Ok(CircleStatsResponse {
        circle_id,
        total_members: model.active_count(),
        total_cycles: c.total_cycles,
        cycles_completed: c.cycles_completed,
        total_amount_locked: c.total_amount_locked,
        total_payouts: c.total_distributed,
        total_penalties: c.common_pool,
        total_platform_fees: c.total_platform_fees_collected,
        total_pending_payouts: c.total_pending_payouts,
    })
}

pub fn query_member_stats(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<MemberStatsResponse> {
    let model = live(deps, &env, circle_id)?;
    let pending = PENDING_PAYOUTS
        .may_load(deps.storage, (circle_id, member.clone()))?
        .unwrap_or_default();
    let m = model.member(&member);
    Ok(MemberStatsResponse {
        member: member.clone(),
        circles_joined: m.map(|_| 1).unwrap_or(0),
        total_contributed: m.map(|m| m.contributed).unwrap_or_default(),
        total_received: m.map(|m| m.received).unwrap_or_default(),
        total_penalties: m.map(|m| m.fees_paid).unwrap_or_default(),
        missed_payments: m.map(|m| m.missed_total).unwrap_or(0),
        pending_payout: pending,
        accumulated_late_fees: m.map(|m| m.arrears).unwrap_or_default(),
    })
}

pub fn query_pending_payout(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<PendingPayoutResponse> {
    Ok(PendingPayoutResponse {
        amount: PENDING_PAYOUTS
            .may_load(deps.storage, (circle_id, member))?
            .unwrap_or_default(),
    })
}

pub fn query_member_accumulated_late_fees(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<AccumulatedLateFeesResponse> {
    let model = live(deps, &env, circle_id)?;
    let c = &model.circle;
    let m = model
        .member(&member)
        .ok_or_else(|| StdError::not_found("member"))?;
    let late_fee = c.contribution_amount.multiply_ratio(c.late_fee_percent, 10_000u64);
    Ok(AccumulatedLateFeesResponse {
        amount: m.fees_paid,
        missed_rounds: m.strikes,
        late_fee_per_round: late_fee,
        exit_penalty: m.lock_original.multiply_ratio(c.exit_penalty_percent, 10_000u64),
        locked_amount: m.lock,
        rounds_until_ejection: c.max_missed_payments_allowed.saturating_sub(m.strikes),
    })
}

pub fn query_deposit_requirement(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<DepositRequirementResponse> {
    let model = live(deps, &env, circle_id)?;
    let clock = model.clock();
    let c = &model.circle;
    let Some(m) = model.member(&member) else {
        return Ok(DepositRequirementResponse {
            required_amount: c.contribution_amount,
            missed_rounds: 0,
            can_deposit: false,
            contribution_amount: c.contribution_amount,
            late_fee_total: Uint128::zero(),
            lock_refill_needed: Uint128::zero(),
            arrears: Uint128::zero(),
            min_amount: c.contribution_amount,
            round: None,
        });
    };
    let stored_settled = CIRCLES.load(deps.storage, circle_id)?.rounds_settled;
    let behind = model.circle.rounds_settled.saturating_sub(stored_settled);
    let due = member_due(&model, clock.as_ref(), m, behind);
    Ok(DepositRequirementResponse {
        required_amount: due
            .as_ref()
            .map(|d| d.max_payment)
            .unwrap_or(c.contribution_amount),
        missed_rounds: m.strikes,
        can_deposit: due.as_ref().map(|d| d.can_pay_now).unwrap_or(false),
        contribution_amount: c.contribution_amount,
        late_fee_total: Uint128::zero(),
        lock_refill_needed: m.refill_needed(),
        arrears: m.arrears,
        min_amount: c.contribution_amount,
        round: due.and_then(|d| d.next_round),
    })
}

pub fn query_member_locked_amount(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<MemberLockedAmountResponse> {
    let model = live(deps, &env, circle_id)?;
    Ok(MemberLockedAmountResponse {
        amount: model.member(&member).map(|m| m.lock).unwrap_or_default(),
    })
}

pub fn query_blocked_members(
    deps: Deps,
    env: Env,
    circle_id: u64,
) -> StdResult<BlockedMembersResponse> {
    let model = live(deps, &env, circle_id)?;
    Ok(BlockedMembersResponse {
        blocked_members: model
            .members
            .iter()
            .filter(|m| !m.is_active())
            .map(|m| (m.address.clone(), m.left_round))
            .collect(),
    })
}

pub fn query_member_pseudonym(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<MemberPseudonymResponse> {
    Ok(MemberPseudonymResponse {
        pseudonym: MEMBER_PSEUDONYMS.may_load(deps.storage, (circle_id, member))?,
    })
}

pub fn query_private_members(
    deps: Deps,
    _env: Env,
    circle_id: u64,
) -> StdResult<PrivateMembersResponse> {
    Ok(PrivateMembersResponse {
        members: PRIVATE_MEMBER_LIST
            .may_load(deps.storage, circle_id)?
            .unwrap_or_default(),
    })
}

/// Full schedule. Large schedules are windowed around the focus round.
pub fn query_distribution_calendar(
    deps: Deps,
    env: Env,
    circle_id: u64,
) -> StdResult<DistributionCalendarResponse> {
    let model = live(deps, &env, circle_id)?;
    let clock = model
        .clock()
        .ok_or_else(|| StdError::generic_err("Circle has not started yet"))?;
    let now_s = model.now_s();
    let settled = model.circle.rounds_settled;
    const WINDOW: u32 = 1_000;
    let (from, to) = if clock.total_rounds <= WINDOW {
        (1, clock.total_rounds)
    } else {
        let from = settled.saturating_sub(100).max(1);
        (from, (from + WINDOW - 1).min(clock.total_rounds))
    };
    let mut rounds = vec![];
    for k in from..=to {
        let (kind, recipients) = planned_recipients(&model, &clock, k, false);
        rounds.push(CalendarRound {
            round_number: k,
            cycle_number: clock.cycle_of(k),
            deposit_deadline: ts(clock.to_wall(clock.due(k), now_s)),
            distribution_date: ts(clock.to_wall(clock.settles(k), now_s)),
            distribution_occurs: !matches!(kind, Kind::Accumulate),
            recipient: match kind {
                Kind::Turn(_) => recipients.first().cloned(),
                _ => None,
            },
            opens_at: Some(ts(clock.to_wall(clock.opens(k), now_s))),
            kind: Some(kind_label(kind)),
            recipients,
            settled: k <= settled,
        });
    }
    Ok(DistributionCalendarResponse { rounds })
}

pub fn query_archived_date(deps: Deps, env: Env, circle_id: u64) -> StdResult<ArchivedDateResponse> {
    let model = live(deps, &env, circle_id)?;
    let archived = model
        .circle
        .closed_at
        .or_else(|| {
            model
                .circle
                .end_date
                .map(|e| e.plus_seconds(model.circle.grace_period_secs()))
        });
    Ok(ArchivedDateResponse {
        archived_date: archived,
    })
}

fn lock_reserve(m: &MemberState) -> LockReserveResponse {
    LockReserveResponse {
        member: m.address.clone(),
        original_lock: m.lock_original,
        remaining_lock: m.lock,
        used_for_coverage: m.covered,
        is_blocked: !m.is_active(),
        blocked_from_cycle: if m.left_round == 0 { None } else { Some(m.left_round) },
    }
}

pub fn query_lock_reserve(
    deps: Deps,
    env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<LockReserveResponse> {
    let model = live(deps, &env, circle_id)?;
    model
        .member(&member)
        .map(lock_reserve)
        .ok_or_else(|| StdError::not_found("member"))
}

pub fn query_lock_reserves(deps: Deps, env: Env, circle_id: u64) -> StdResult<LockReservesResponse> {
    let model = live(deps, &env, circle_id)?;
    Ok(LockReservesResponse {
        reserves: model.members.iter().map(lock_reserve).collect(),
    })
}

pub fn query_preview_end_cycle(
    deps: Deps,
    env: Env,
    circle_id: u64,
) -> StdResult<EndCyclePreviewResponse> {
    let model = live(deps, &env, circle_id)?;
    let eligible: Vec<Addr> = model
        .members
        .iter()
        .filter(|m| m.is_active())
        .map(|m| m.address.clone())
        .collect();
    let settleable = model.circle.carry_pot + model.circle.common_pool;
    let n = eligible.len() as u128;
    let (per, rem) = if n == 0 {
        (Uint128::zero(), settleable)
    } else {
        let per = settleable.multiply_ratio(1u128, n);
        (per, settleable - per * Uint128::from(n))
    };
    Ok(EndCyclePreviewResponse {
        settleable_amount: settleable,
        eligible_members: eligible,
        amount_per_member: per,
        remainder: rem,
    })
}

pub fn query_wallet_reputation(
    deps: Deps,
    _env: Env,
    address: Addr,
) -> StdResult<WalletReputationResponse> {
    let count = WALLET_RATING_COUNT
        .may_load(deps.storage, address.clone())?
        .unwrap_or(0);
    let mut ratings = Vec::new();
    let mut sum: u64 = 0;
    for i in 0..count {
        if let Some((circle_id, rater)) =
            WALLET_RATING_INDEX.may_load(deps.storage, (address.clone(), i))?
        {
            if let Some(r) =
                MEMBER_RATINGS.may_load(deps.storage, (circle_id, address.clone(), rater))?
            {
                sum += r.score as u64;
                ratings.push(r);
            }
        }
    }
    let average_score = if count == 0 {
        None
    } else {
        let scaled = sum * 100 / count;
        Some(format!("{}.{:02}", scaled / 100, scaled % 100))
    };
    Ok(WalletReputationResponse {
        address,
        rating_count: count as u32,
        average_score,
        ratings,
    })
}
