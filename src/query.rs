use cosmwasm_std::{Addr, Deps, Env, Order, StdResult, Uint128, Timestamp};
use cw_storage_plus::Bound;

use crate::msg::{
    AccumulatedLateFeesResponse, BalanceResponse, CircleResponse, CirclesResponse, CycleResponse,
    DepositRequirementResponse, DepositsResponse, EventsResponse, MemberStatsResponse, MembersResponse,
    PayoutsResponse, PenaltiesResponse, PendingPayoutResponse, RefundsResponse,
    StatusResponse, CircleStatsResponse, MemberLockedAmountResponse,
    BlockedMembersResponse, MemberPseudonymResponse, PrivateMembersResponse,
    DistributionCalendarResponse, ArchivedDateResponse, CalendarRound,
};
use crate::state::{
    Circle, CircleStatus, CIRCLES, DEPOSITS, EVENTS, EVENT_COUNTER, PAYOUTS,
    PENALTIES, REFUNDS, MEMBER_LOCKED_AMOUNTS, MEMBER_ACCUMULATED_LATE_FEES,
    MEMBER_LAST_DEPOSITED_CYCLE, MEMBER_MISSED_PAYMENTS, BLOCKED_MEMBERS, MEMBER_PSEUDONYMS,
    PRIVATE_MEMBER_LIST, PENDING_PAYOUTS, DistributionThreshold,
};

pub fn query_circle(deps: Deps, _env: Env, circle_id: u64) -> StdResult<CircleResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    Ok(CircleResponse { circle })
}

pub fn query_circles(
    deps: Deps,
    _env: Env,
    start_after: Option<u64>,
    limit: Option<u32>,
    status: Option<CircleStatus>,
    creator: Option<Addr>,
) -> StdResult<CirclesResponse> {
    let limit = limit.unwrap_or(30).min(100) as usize;
    let start = start_after.map(Bound::exclusive);

    let circles: StdResult<Vec<Circle>> = CIRCLES
        .range(deps.storage, start, None, Order::Ascending)
        .take(limit)
        .map(|item| {
            let (_, circle) = item?;
            Ok(circle)
        })
        .collect();

    let mut circles = circles?;

    // Filter by status if provided
    if let Some(ref filter_status) = status {
        circles.retain(|c| &c.circle_status == filter_status);
    }

    // Filter by creator if provided
    if let Some(ref filter_creator) = creator {
        circles.retain(|c| &c.creator_address == filter_creator);
    }

    Ok(CirclesResponse { circles })
}

pub fn query_circle_members(deps: Deps, _env: Env, circle_id: u64) -> StdResult<MembersResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    Ok(MembersResponse {
        members: circle.members_list,
        pending_members: circle.pending_members,
    })
}

pub fn query_circle_status(deps: Deps, _env: Env, circle_id: u64) -> StdResult<StatusResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    Ok(StatusResponse {
        status: circle.circle_status,
    })
}

pub fn query_current_cycle(deps: Deps, _env: Env, circle_id: u64) -> StdResult<CycleResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    Ok(CycleResponse {
        current_cycle: circle.current_cycle_index,
        total_cycles: circle.total_cycles,
        next_payout_date: circle.next_payout_date,
        members_paid: circle.members_paid_this_cycle,
        members_late: circle.members_late_this_cycle,
    })
}

pub fn query_cycle_deposits(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    cycle: u32,
) -> StdResult<DepositsResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    let mut deposits = vec![];

    for member in &circle.members_list {
        if let Ok(Some(deposit)) =
            DEPOSITS.may_load(deps.storage, (circle_id, member.clone(), cycle))
        {
            deposits.push(deposit);
        }
    }

    Ok(DepositsResponse { deposits })
}

pub fn query_member_deposits(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<DepositsResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    let mut deposits = vec![];

    for cycle in 1..=circle.total_cycles {
        if let Ok(Some(deposit)) =
            DEPOSITS.may_load(deps.storage, (circle_id, member.clone(), cycle))
        {
            deposits.push(deposit);
        }
    }

    Ok(DepositsResponse { deposits })
}

pub fn query_payouts(deps: Deps, _env: Env, circle_id: u64) -> StdResult<PayoutsResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    let mut payouts = vec![];

    // `PAYOUTS` is keyed by `current_cycle_index` (the round counter), not by
    // calendar cycle. Iterating `1..=cycles_completed` therefore missed payouts
    // recorded outside that window (e.g. the per-round payouts that happen
    // when distribution_threshold = None / Total at the last round of a cycle).
    // Iterate by round instead so every PAYOUT row is returned.
    let last_round = circle.current_cycle_index;
    for round in 1..=last_round {
        for item in PAYOUTS
            .prefix((circle_id, round))
            .range(deps.storage, None, None, Order::Ascending)
        {
            let (_, payout) = item?;
            payouts.push(payout);
        }
    }

    Ok(PayoutsResponse { payouts })
}

pub fn query_payout_history(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    cycle: Option<u32>,
) -> StdResult<PayoutsResponse> {
    if let Some(cycle_num) = cycle {
        let payouts: StdResult<Vec<_>> = PAYOUTS
            .prefix((circle_id, cycle_num))
            .range(deps.storage, None, None, Order::Ascending)
            .map(|item| item.map(|(_, p)| p))
            .collect();
        Ok(PayoutsResponse {
            payouts: payouts?,
        })
    } else {
        query_payouts(deps, _env, circle_id)
    }
}

pub fn query_circle_balance(deps: Deps, env: Env, circle_id: u64) -> StdResult<BalanceResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    
    // Get actual balance from contract
    let balance = deps
        .querier
        .query_balance(env.contract.address, &circle.denomination)?;
    
    Ok(BalanceResponse {
        balance: balance.amount,
    })
}

pub fn query_member_balance(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<BalanceResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    let mut total_contributed = Uint128::zero();
    let mut total_received = Uint128::zero();

    // Calculate total contributed
    for cycle in 1..=circle.total_cycles {
        if let Ok(Some(deposit)) =
            DEPOSITS.may_load(deps.storage, (circle_id, member.clone(), cycle))
        {
            total_contributed = total_contributed
                .checked_add(deposit.amount)
                .unwrap_or(total_contributed);
        }
    }

    // Calculate total received from payouts
    for cycle in 1..=circle.cycles_completed {
        for item in PAYOUTS
            .prefix((circle_id, cycle))
            .range(deps.storage, None, None, Order::Ascending)
        {
            let (_, payout) = item?;
            if payout.recipient == member {
                total_received = total_received
                    .checked_add(payout.amount)
                    .unwrap_or(total_received);
            }
        }
    }

    // Balance = contributed - received
    let balance = total_contributed
        .checked_sub(total_received)
        .unwrap_or(Uint128::zero());

    Ok(BalanceResponse { balance })
}

pub fn query_penalties(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Option<Addr>,
) -> StdResult<PenaltiesResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    let mut penalties = vec![];

    let members_to_check = if let Some(ref mem) = member {
        vec![mem.clone()]
    } else {
        circle.members_list
    };

    for member_addr in members_to_check {
        for cycle in 1..=circle.total_cycles {
            if let Ok(Some(penalty)) =
                PENALTIES.may_load(deps.storage, (circle_id, member_addr.clone(), cycle))
            {
                penalties.push(penalty);
            }
        }
    }

    Ok(PenaltiesResponse { penalties })
}

pub fn query_refunds(deps: Deps, _env: Env, circle_id: u64) -> StdResult<RefundsResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    let mut refunds = vec![];

    for member in &circle.members_list {
        if let Ok(Some(refund)) = REFUNDS.may_load(deps.storage, (circle_id, member.clone())) {
            refunds.push(refund);
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
    let limit = limit.unwrap_or(100).min(1000) as usize;
    let event_count = EVENT_COUNTER
        .may_load(deps.storage, circle_id)?
        .unwrap_or(0);

    let mut events = vec![];
    let start = if event_count > limit as u64 {
        event_count - limit as u64 + 1
    } else {
        1
    };

    for event_id in start..=event_count {
        if let Ok(Some(event)) = EVENTS.may_load(deps.storage, (circle_id, event_id)) {
            events.push(event);
        }
    }

    Ok(EventsResponse { events })
}

pub fn query_circle_stats(deps: Deps, _env: Env, circle_id: u64) -> StdResult<CircleStatsResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    
    let mut total_payouts = Uint128::zero();
    for cycle in 1..=circle.cycles_completed {
        for item in PAYOUTS
            .prefix((circle_id, cycle))
            .range(deps.storage, None, None, Order::Ascending)
        {
            let (_, payout) = item?;
            total_payouts = total_payouts
                .checked_add(payout.amount)
                .unwrap_or(total_payouts);
        }
    }

    Ok(CircleStatsResponse {
        circle_id,
        total_members: circle.members_list.len() as u32,
        total_cycles: circle.total_cycles,
        cycles_completed: circle.cycles_completed,
        total_amount_locked: circle.total_amount_locked,
        total_payouts,
        total_penalties: circle.total_penalties_collected,
        total_platform_fees: circle.total_platform_fees_collected,
        total_pending_payouts: circle.total_pending_payouts,
    })
}

pub fn query_member_stats(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<MemberStatsResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    
    let mut total_contributed = Uint128::zero();
    let mut total_received = Uint128::zero();
    let mut total_penalties = Uint128::zero();
    let mut missed_payments = 0u32;

    // Calculate stats
    for cycle in 1..=circle.total_cycles {
        // Check deposits
        if let Ok(Some(deposit)) =
            DEPOSITS.may_load(deps.storage, (circle_id, member.clone(), cycle))
        {
            total_contributed = total_contributed
                .checked_add(deposit.amount)
                .unwrap_or(total_contributed);
        } else {
            missed_payments += 1;
        }

        // Check penalties
        if let Ok(Some(penalty)) =
            PENALTIES.may_load(deps.storage, (circle_id, member.clone(), cycle))
        {
            total_penalties = total_penalties
                .checked_add(penalty.amount)
                .unwrap_or(total_penalties);
        }
    }

    // Calculate received
    for cycle in 1..=circle.cycles_completed {
        for item in PAYOUTS
            .prefix((circle_id, cycle))
            .range(deps.storage, None, None, Order::Ascending)
        {
            let (_, payout) = item?;
            if payout.recipient == member {
                total_received = total_received
                    .checked_add(payout.amount)
                    .unwrap_or(total_received);
            }
        }
    }

    let pending_payout = PENDING_PAYOUTS
        .may_load(deps.storage, (circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());
    let accumulated_late_fees = MEMBER_ACCUMULATED_LATE_FEES
        .may_load(deps.storage, (circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());

    Ok(MemberStatsResponse {
        member,
        circles_joined: 1,
        total_contributed,
        total_received,
        total_penalties,
        missed_payments,
        pending_payout,
        accumulated_late_fees,
    })
}

pub fn query_pending_payout(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<PendingPayoutResponse> {
    let amount = PENDING_PAYOUTS
        .may_load(deps.storage, (circle_id, member))?
        .unwrap_or(Uint128::zero());
    Ok(PendingPayoutResponse { amount })
}

pub fn query_member_accumulated_late_fees(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<AccumulatedLateFeesResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    let amount = MEMBER_ACCUMULATED_LATE_FEES
        .may_load(deps.storage, (circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());
    let locked_amount = MEMBER_LOCKED_AMOUNTS
        .may_load(deps.storage, (circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());

    let late_fee_per_round = circle
        .contribution_amount
        .multiply_ratio(circle.late_fee_percent, 10000u64);
    let exit_penalty = locked_amount.multiply_ratio(circle.exit_penalty_percent, 10000u64);

    let missed_info = MEMBER_MISSED_PAYMENTS
        .may_load(deps.storage, (circle_id, member.clone()))?
        .unwrap_or(crate::state::MemberMissedPayments {
            member: member.clone(),
            missed_count: 0,
            last_missed_cycle: None,
            last_fee_round: None,
        });

    let total_deduction_so_far = amount + exit_penalty;
    let rounds_until_ejection = if total_deduction_so_far >= locked_amount || late_fee_per_round.is_zero() {
        0
    } else {
        let remaining = locked_amount - total_deduction_so_far;
        (remaining.u128() / late_fee_per_round.u128()) as u32
    };

    Ok(AccumulatedLateFeesResponse {
        amount,
        missed_rounds: missed_info.missed_count,
        late_fee_per_round,
        exit_penalty,
        locked_amount,
        rounds_until_ejection,
    })
}

pub fn query_deposit_requirement(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<DepositRequirementResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;

    let blocked = BLOCKED_MEMBERS
        .may_load(deps.storage, (circle_id, member.clone()))?
        .map(|bc| bc <= circle.current_cycle_index)
        .unwrap_or(false);

    let already_deposited = DEPOSITS
        .may_load(deps.storage, (circle_id, member.clone(), circle.current_cycle_index))?
        .is_some();

    let last_deposited_cycle = MEMBER_LAST_DEPOSITED_CYCLE
        .may_load(deps.storage, (circle_id, member.clone()))?
        .or_else(|| {
            DEPOSITS
                .prefix((circle_id, member.clone()))
                .range(deps.storage, None, None, Order::Descending)
                .next()
                .and_then(|r| r.ok())
                .map(|(c, _)| c)
        })
        .unwrap_or(0);

    let rounds_missed = circle
        .current_cycle_index
        .saturating_sub(last_deposited_cycle)
        .saturating_sub(1);

    let late_fee_per_round = circle
        .contribution_amount
        .multiply_ratio(circle.late_fee_percent, 10000u64);
    let late_fee_total = late_fee_per_round * Uint128::from(rounds_missed as u128);
    let required_amount = circle
        .contribution_amount
        .checked_add(late_fee_total)
        .unwrap_or(circle.contribution_amount);

    let can_deposit = !blocked
        && !already_deposited
        && rounds_missed < circle.max_missed_payments_allowed;

    Ok(DepositRequirementResponse {
        required_amount,
        missed_rounds: rounds_missed,
        can_deposit,
        contribution_amount: circle.contribution_amount,
        late_fee_total,
    })
}

pub fn query_member_locked_amount(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<MemberLockedAmountResponse> {
    let locked_amount = MEMBER_LOCKED_AMOUNTS
        .may_load(deps.storage, (circle_id, member))?
        .unwrap_or(Uint128::zero());
    
    Ok(MemberLockedAmountResponse {
        amount: locked_amount,
    })
}

pub fn query_blocked_members(
    deps: Deps,
    _env: Env,
    circle_id: u64,
) -> StdResult<BlockedMembersResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    let mut blocked_members = vec![];
    
    for member in &circle.members_list {
        if let Ok(Some(blocked_from_cycle)) = BLOCKED_MEMBERS.may_load(deps.storage, (circle_id, member.clone())) {
            blocked_members.push((member.clone(), blocked_from_cycle));
        }
    }
    
    Ok(BlockedMembersResponse {
        blocked_members,
    })
}

pub fn query_member_pseudonym(
    deps: Deps,
    _env: Env,
    circle_id: u64,
    member: Addr,
) -> StdResult<MemberPseudonymResponse> {
    let pseudonym = MEMBER_PSEUDONYMS
        .may_load(deps.storage, (circle_id, member))?
        .map(Some)
        .unwrap_or(None);
    
    Ok(MemberPseudonymResponse {
        pseudonym,
    })
}

pub fn query_private_members(
    deps: Deps,
    _env: Env,
    circle_id: u64,
) -> StdResult<PrivateMembersResponse> {
    let members = PRIVATE_MEMBER_LIST
        .may_load(deps.storage, circle_id)?
        .unwrap_or_default();
    
    Ok(PrivateMembersResponse {
        members,
    })
}

/// Returns the full calendar with `distribution_occurs` set per round:
/// None => every round; Total => only last round of each cycle (100% of all members); MinMembers(N) => from round N to end of cycle.
pub fn query_distribution_calendar(
    deps: Deps,
    _env: Env,
    circle_id: u64,
) -> StdResult<DistributionCalendarResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    
    let start_timestamp = circle.start_date.ok_or_else(|| {
        cosmwasm_std::StdError::generic_err("Circle has not started yet")
    })?;
    
    // Threshold semantics — kept in lockstep with
    // `distribution_min_round_for_active` in execute.rs. Diverging from execute
    // caused the cron to queue the wrong action and round to get stuck.
    //
    // - None     => same as Total: distribution at the last round of each cycle
    // - Total    => 100% of all members (distribution at end of each cycle only)
    // - MinMembers(N) => distribution from round N onward (within each cycle)
    //
    // For Total/None, the "last round" is determined by the PAYOUT ORDER size
    // (= members locked-in at start), NOT `max_members` — a 3-cap circle that
    // started with 2 has 2-round cycles. Using `max_members` here marked no
    // round as a distribution round (round_in_cycle maxes at 2, never reaches 3),
    // which removed every distribution from the calendar.
    let round_size = circle.payout_order_list
        .as_ref()
        .map(|l| l.len() as u32)
        .unwrap_or(circle.max_members)
        .max(1);
    let min_round_for_distribution: u32 = match circle.distribution_threshold {
        None | Some(DistributionThreshold::Total {}) => round_size,
        Some(DistributionThreshold::MinMembers { count }) => count,
    };
    
    let mut rounds = vec![];

    // Pre-load blocked members so we can mark ejected recipients in the
    // calendar. A member is blocked at cycle `bc`; we mark the calendar slot
    // recipient as None for rounds whose cycle_number > bc (i.e. the member
    // was already ejected before this slot was due) so the UI can render
    // "Ejected" instead of the original name and avoid implying a payout
    // that will never happen.
    let blocked: Vec<(Addr, u32)> = BLOCKED_MEMBERS
        .prefix(circle_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|r| r.ok())
        .collect();

    if let Some(payout_order) = &circle.payout_order_list {
        // Round size for the cycle modulo is the number of recipients in the
        // payout order — same value used by `execute` for round_in_cycle —
        // not max_members. With check_and_eject shrinking the list, this
        // keeps the calendar consistent with the contract's own bookkeeping.
        let round_size = payout_order.len().max(1) as u32;
        let mut round_number = 1u32;
        for cycle in 1..=circle.total_cycles {
            for recipient in payout_order.iter() {
                let round_in_cycle = ((round_number - 1) % round_size) + 1;
                let distribution_occurs = round_in_cycle >= min_round_for_distribution;

                let round_offset_seconds = (round_number - 1) as u64 * circle.cycle_duration_secs();
                let deposit_deadline = Timestamp::from_seconds(
                    start_timestamp.seconds() + round_offset_seconds
                );
                let distribution_date = Timestamp::from_seconds(
                    start_timestamp.seconds() + round_offset_seconds + circle.cycle_duration_secs()
                );

                // Was this recipient ejected before this cycle?
                let recipient_field = match blocked.iter().find(|(addr, _)| addr == recipient) {
                    Some((_, bc)) if cycle > *bc => None, // ejected before this slot
                    _ => Some(recipient.clone()),
                };

                rounds.push(CalendarRound {
                    round_number,
                    cycle_number: cycle,
                    deposit_deadline,
                    distribution_date,
                    distribution_occurs,
                    recipient: recipient_field,
                });

                round_number += 1;
            }
        }
    }

    Ok(DistributionCalendarResponse {
        rounds,
    })
}

pub fn query_archived_date(
    deps: Deps,
    _env: Env,
    circle_id: u64,
) -> StdResult<ArchivedDateResponse> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;
    
    let archived_date = if let Some(end_date) = circle.end_date {
        Some(Timestamp::from_seconds(
            end_date.seconds() + circle.grace_period_secs()
        ))
    } else {
        None
    };
    
    Ok(ArchivedDateResponse {
        archived_date,
    })
}
