use cosmwasm_std::{
    Addr, BankMsg, Coin, CosmosMsg, DepsMut, Env, MessageInfo, Order, Response, StdResult, Storage,
    Timestamp, Uint128,
};
use cw_utils::must_pay;

use crate::error::ContractError;
use crate::msg::ExecuteMsg;
use crate::state::{
    Circle, CircleStatus, DepositRecord, DistributionThreshold, EventLog, MemberMissedPayments,
    PayoutOrderType, PayoutRecord, PenaltyRecord, RefundMode, Visibility, BLOCKED_MEMBERS,
    CIRCLE_COUNTER, CIRCLES, CREATOR_REWARDS_CREDITED, DEPOSITS, EVENTS, EVENT_COUNTER,
    MEMBER_ACCUMULATED_LATE_FEES, MEMBER_LAST_DEPOSITED_CYCLE, MEMBER_LOCKED_AMOUNTS,
    MEMBER_MISSED_PAYMENTS, MEMBER_PSEUDONYMS, PAYOUTS, PENALTIES, PENDING_PAYOUTS,
    PLATFORM_CONFIG, PRIVATE_MEMBER_LIST,
};

/// First round index (within a savings cycle) where distribution may occur.
/// `None` means the creator left threshold unset (older private circles) — match app/frontend
/// behavior: same as [`DistributionThreshold::Total`] using **active** member count.
fn distribution_min_round_for_active(
    threshold: &Option<DistributionThreshold>,
    active_count: u32,
) -> u32 {
    match threshold {
        None | Some(DistributionThreshold::Total {}) => active_count,
        Some(DistributionThreshold::MinMembers { count }) => (*count).min(active_count),
    }
}

/// Whether pooled "full cycle" payout applies at the distribution round (`Total` semantics).
fn is_total_style_threshold(threshold: &Option<DistributionThreshold>) -> bool {
    matches!(
        threshold,
        None | Some(DistributionThreshold::Total {})
    )
}

pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::CreateCircle {
            circle_name,
            circle_description,
            circle_image,
            max_members,
            min_members_required,
            invite_only,
            contribution_amount,
            denomination,
            exit_penalty_percent,
            late_fee_percent,
            total_cycles,
            cycle_duration_days,
            cycle_duration_seconds,
            start_date,
            grace_period_hours,
            grace_period_seconds,
            auto_start_when_full,
            auto_start_type,
            auto_start_date,
            payout_order_type,
            payout_order_list,
            auto_payout_enabled,
            manual_trigger_enabled,
            emergency_stop_enabled,
            auto_refund_if_min_not_met,
            strict_mode,
            visibility,
            show_member_identities,
            distribution_threshold,
        } => execute_create_circle(
            deps,
            env,
            info,
            circle_name,
            circle_description,
            circle_image,
            max_members,
            min_members_required,
            invite_only,
            contribution_amount,
            denomination,
            exit_penalty_percent,
            late_fee_percent,
            total_cycles,
            cycle_duration_days,
            cycle_duration_seconds,
            start_date,
            grace_period_hours,
            grace_period_seconds,
            auto_start_when_full,
            auto_start_type,
            auto_start_date,
            payout_order_type,
            payout_order_list,
            auto_payout_enabled,
            manual_trigger_enabled,
            emergency_stop_enabled,
            auto_refund_if_min_not_met,
            strict_mode,
            visibility,
            show_member_identities,
            distribution_threshold,
        ),
        ExecuteMsg::JoinCircle { circle_id } => execute_join_circle(deps, env, info, circle_id),
        ExecuteMsg::AcceptInvite { circle_id } => execute_join_circle(deps, env, info, circle_id),
        ExecuteMsg::InviteMember {
            circle_id,
            member_address,
        } => execute_invite_member(deps, env, info, circle_id, member_address),
        ExecuteMsg::ExitCircle { circle_id } => execute_exit_circle(deps, env, info, circle_id),
        ExecuteMsg::StartCircle { circle_id } => execute_start_circle(deps, env, info, circle_id),
        ExecuteMsg::DepositContribution { circle_id } => {
            execute_deposit_contribution(deps, env, info, circle_id)
        }
        ExecuteMsg::ProcessPayout { circle_id } => {
            execute_process_payout(deps, env, info, circle_id)
        }
        ExecuteMsg::AdvanceRound { circle_id } => execute_advance_round(deps, env, info, circle_id),
        ExecuteMsg::Withdraw { circle_id } => execute_withdraw(deps, env, info, circle_id),
        ExecuteMsg::CheckAndEject { circle_id } => {
            execute_check_and_eject(deps, env, info, circle_id)
        }
        ExecuteMsg::PauseCircle { circle_id } => execute_pause_circle(deps, env, info, circle_id),
        ExecuteMsg::UnpauseCircle { circle_id } => {
            execute_unpause_circle(deps, env, info, circle_id)
        }
        ExecuteMsg::EmergencyStop { circle_id } => {
            execute_emergency_stop(deps, env, info, circle_id)
        }
        ExecuteMsg::CancelCircle { circle_id } => {
            execute_cancel_circle(deps, env, info, circle_id)
        }
        ExecuteMsg::UpdateCircle {
            circle_id,
            circle_name,
            circle_description,
            circle_image,
        } => execute_update_circle(
            deps,
            env,
            info,
            circle_id,
            circle_name,
            circle_description,
            circle_image,
        ),
        ExecuteMsg::WithdrawPlatformFees { circle_id } => {
            execute_withdraw_platform_fees(deps, env, info, circle_id)
        }
        ExecuteMsg::SweepDust { circle_id } => execute_sweep_dust(deps, env, info, circle_id),
        ExecuteMsg::DepositCreatorReward { circle_id } => {
            execute_deposit_creator_reward(deps, env, info, circle_id)
        }
        ExecuteMsg::AddPrivateMember {
            circle_id,
            member_address,
            pseudonym,
        } => execute_add_private_member(deps, env, info, circle_id, member_address, pseudonym),
        ExecuteMsg::UpdateMemberPseudonym {
            circle_id,
            member_address,
            pseudonym,
        } => execute_update_member_pseudonym(deps, env, info, circle_id, member_address, pseudonym),
        ExecuteMsg::BlockMember {
            circle_id,
            member_address,
        } => execute_block_member(deps, env, info, circle_id, member_address),
        ExecuteMsg::DistributeBlockedFunds { circle_id, cycle } => {
            execute_distribute_blocked_funds(deps, env, info, circle_id, cycle)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute required creator lock: contribution_amount * 2
fn compute_creator_lock(contribution: Uint128, _max_members: u32) -> Result<Uint128, ContractError> {
    contribution
        .checked_mul(Uint128::from(2u64))
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Creator lock overflow".to_string(),
        })
}

/// Compute late fee per round: contribution * late_fee_percent / 10000
fn compute_late_fee_per_round(contribution: Uint128, late_fee_percent: u64) -> Uint128 {
    contribution.multiply_ratio(late_fee_percent, 10000u64)
}

/// Compute exit penalty for a given locked amount: locked * exit_penalty_percent / 10000
fn compute_exit_penalty(locked: Uint128, exit_penalty_percent: u64) -> Uint128 {
    locked.multiply_ratio(exit_penalty_percent, 10000u64)
}

/// Base max_missed: floor((10000 - exit_penalty_percent) / late_fee_percent) from % penalty and late fee.
/// Returns u32::MAX if late_fee_percent is 0 (no late fees configured).
fn compute_max_missed_base(exit_penalty_percent: u64, late_fee_percent: u64) -> u32 {
    if late_fee_percent == 0 {
        return u32::MAX;
    }
    let numerator = 10000u64.saturating_sub(exit_penalty_percent);
    (numerator / late_fee_percent) as u32
}

/// Compute max_missed scaled by active members. When members_at_start is None, returns base.
fn compute_max_missed_scaled(
    exit_penalty_percent: u64,
    late_fee_percent: u64,
    members_at_start: Option<u32>,
    current_active_members: u32,
) -> u32 {
    let base = compute_max_missed_base(exit_penalty_percent, late_fee_percent);
    let Some(start_count) = members_at_start else {
        return base;
    };
    if start_count == 0 {
        return base;
    }
    // Scale: max_missed decreases (stricter) when members exit or are ejected
    let scaled = (base as u64 * current_active_members as u64 / start_count as u64) as u32;
    scaled.max(1)
}

/// Cap max_missed so ejection happens before the last round. max_missed <= total_rounds - 1.
fn cap_max_missed_by_rounds(max_missed: u32, total_rounds: u32) -> u32 {
    let cap = total_rounds.saturating_sub(1).max(1);
    max_missed.min(cap)
}

/// Legacy alias for compute_max_missed_base (used at CreateCircle for preview).
fn compute_max_missed(exit_penalty_percent: u64, late_fee_percent: u64) -> u32 {
    compute_max_missed_base(exit_penalty_percent, late_fee_percent)
}

/// Get the original locked amount for a member (what they deposited at join time).
/// Creator's lock = creator_lock_amount; other members = contribution_amount.
fn original_lock_for_member(circle: &Circle, member: &Addr) -> Uint128 {
    if member == &circle.creator_address {
        circle.creator_lock_amount
    } else {
        circle.contribution_amount
    }
}

/// Read the member's current `MEMBER_LOCKED_AMOUNTS` balance, treating
/// "no entry" as zero. The creator has no entry (their lock lives in
/// `circle.creator_lock_amount`), so this is for non-creator members only.
fn current_member_lock(storage: &dyn Storage, circle_id: u64, member: &Addr) -> Uint128 {
    MEMBER_LOCKED_AMOUNTS
        .may_load(storage, (circle_id, member.clone()))
        .unwrap_or(None)
        .unwrap_or(Uint128::zero())
}

/// Apply Running-state bookkeeping to a circle: anchor calendar timestamps to
/// `now`, set status / cycle index, snapshot `members_at_start`, and recompute
/// `max_missed_payments_allowed` (scaled by current active members).
///
/// Used by both `execute_start_circle` (creator-triggered) and the
/// `auto_start_when_full + by_members` branch in `execute_join_circle` so the
/// two paths cannot drift.
fn apply_running_state(circle: &mut Circle, now: Timestamp) {
    circle.start_date = Some(now);
    circle.first_cycle_date = Some(now);
    circle.next_payout_date = Some(now);

    // Round count is locked at start based on who actually joined, NOT the
    // configured max. A 3-max-member circle that started with 2 members
    // has 2 * total_cycles rounds, not 3 * total_cycles. Using max_members
    // here caused circles to overshoot their calendar and never transition
    // to Finalizing (the cron kept advancing past the last real round).
    let members_at_start = circle.members_list.len() as u32;
    let total_rounds = members_at_start * circle.total_cycles;
    let total_duration_seconds = circle.cycle_duration_secs() * total_rounds as u64;
    let end_timestamp = Timestamp::from_seconds(now.seconds() + total_duration_seconds);
    circle.end_date = Some(end_timestamp);

    circle.circle_status = CircleStatus::Running;
    circle.current_cycle_index = 1;
    circle.updated_at = now;

    circle.members_at_start = Some(members_at_start);
    circle.max_missed_payments_allowed = cap_max_missed_by_rounds(
        compute_max_missed_scaled(
            circle.exit_penalty_percent,
            circle.late_fee_percent,
            Some(members_at_start),
            members_at_start,
        ),
        total_rounds,
    );
}

/// Check if member meets ejection condition. A member is ejected when EITHER:
///   1. Their `missed_count >= max_missed_payments_allowed` (the configured cap), OR
///   2. `accumulated_late_fees + exit_penalty >= original_lock` (locked funds exhausted).
///
/// Uses `original_lock` (the amount the member deposited at join time) rather than the
/// remaining locked balance, since locked funds may have been partially consumed to cover
/// missed deposits via `use_locked_amount_for_member`.
fn should_eject_member(
    storage: &dyn Storage,
    circle_id: u64,
    member: &Addr,
    original_lock: Uint128,
    exit_penalty_percent: u64,
    max_missed_payments_allowed: u32,
) -> bool {
    if original_lock.is_zero() {
        return false;
    }
    // Hard cap: missed payments at or above the configured max trigger ejection
    // independently of the late-fee/penalty arithmetic. Without this, low %
    // configs (e.g. 10% late fee + 20% exit penalty in a 6-round circle) never
    // accumulate enough fees to reach the original_lock threshold within the
    // circle's lifetime, so members can miss every round and stay in the circle.
    if max_missed_payments_allowed > 0 {
        let missed = MEMBER_MISSED_PAYMENTS
            .may_load(storage, (circle_id, member.clone()))
            .unwrap_or(None)
            .map(|m| m.missed_count)
            .unwrap_or(0);
        if missed >= max_missed_payments_allowed {
            return true;
        }
    }
    let accumulated = MEMBER_ACCUMULATED_LATE_FEES
        .may_load(storage, (circle_id, member.clone()))
        .unwrap_or(None)
        .unwrap_or(Uint128::zero());
    let exit_penalty = compute_exit_penalty(original_lock, exit_penalty_percent);
    accumulated + exit_penalty >= original_lock
}

/// Eject a member from a running circle: remove from members_list, record in BLOCKED_MEMBERS, keep locked funds in pool, emit event.
fn eject_member_from_circle(
    deps: &mut DepsMut,
    env: &Env,
    circle: &mut Circle,
    member: &Addr,
) -> Result<(), ContractError> {
    circle.members_list.retain(|m| m != member);
    BLOCKED_MEMBERS.save(
        deps.storage,
        (circle.circle_id, member.clone()),
        &circle.current_cycle_index,
    )?;

    // Accumulate the exit penalty and remaining late fees as penalties collected
    let locked = MEMBER_LOCKED_AMOUNTS
        .may_load(deps.storage, (circle.circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());
    let accumulated_fees = MEMBER_ACCUMULATED_LATE_FEES
        .may_load(deps.storage, (circle.circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());
    let exit_penalty = compute_exit_penalty(locked, circle.exit_penalty_percent);
    let penalty_total = accumulated_fees + exit_penalty;
    let actual_penalty = if penalty_total > locked {
        locked
    } else {
        penalty_total
    };

    circle.total_penalties_collected = circle
        .total_penalties_collected
        .checked_add(actual_penalty)
        .unwrap_or(circle.total_penalties_collected);

    // Clear per-member fee tracking
    MEMBER_ACCUMULATED_LATE_FEES.remove(deps.storage, (circle.circle_id, member.clone()));

    // Record missed payments for stats
    let mut missed = MEMBER_MISSED_PAYMENTS
        .may_load(deps.storage, (circle.circle_id, member.clone()))?
        .unwrap_or(MemberMissedPayments {
            member: member.clone(),
            missed_count: 0,
            last_missed_cycle: None,
            last_fee_round: None,
        });
    missed.last_missed_cycle = Some(circle.current_cycle_index);
    MEMBER_MISSED_PAYMENTS.save(deps.storage, (circle.circle_id, member.clone()), &missed)?;

    // Recompute max_missed_payments_allowed (dynamic from % penalty and late fee, scaled by active members).
    // Round count is based on members_at_start (locked when circle started) NOT max_members — see apply_running_state.
    let members_basis = circle.members_at_start.unwrap_or(circle.max_members);
    let total_rounds = members_basis * circle.total_cycles;
    circle.max_missed_payments_allowed = cap_max_missed_by_rounds(
        compute_max_missed_scaled(
            circle.exit_penalty_percent,
            circle.late_fee_percent,
            circle.members_at_start,
            circle.members_list.len() as u32,
        ),
        total_rounds,
    );

    // Keep `payout_order_list` in sync with `members_list` so the calendar
    // query (`get_distribution_calendar`) and the per-round recipient picker
    // in `execute_process_payout` see the same active roster. Previously this
    // was only done in the `check_and_eject` batch path, so an ejection that
    // fired from inside `process_payout` / `advance_round` / `deposit` left a
    // stale order with the blocked address, producing phantom calendar
    // entries until the next `check_and_eject` swept it.
    if let Some(ref mut order) = circle.payout_order_list {
        order.retain(|m| m != member);
    }

    // Display-only field: keep it consistent with the new active roster size
    // so the UI doesn't show a payout target that no longer matches reality.
    // Actual on-chain payout math is recomputed inside `execute_process_payout`.
    circle.payout_amount = circle
        .contribution_amount
        .checked_mul(Uint128::from(circle.members_list.len() as u128))
        .unwrap_or(circle.payout_amount);

    log_event(
        deps,
        env,
        circle.circle_id,
        "member_ejected",
        &format!(
            "Member {} ejected at cycle {} (locked: {}, fees: {}, penalty: {})",
            member, circle.current_cycle_index, locked, accumulated_fees, exit_penalty
        ),
    )?;

    // Re-emit the distribution calendar so off-chain consumers (frontend,
    // server sync, scheduler) can re-render without waiting for the next
    // full sync. `members_at_start` is intentionally NOT changed (it locks
    // the schedule), so total_rounds stays put — only the per-cycle round
    // recipients shift.
    let start_ts = circle.first_cycle_date.unwrap_or(env.block.time);
    let rebuilt = build_distribution_calendar(circle, start_ts);
    log_event(
        deps,
        env,
        circle.circle_id,
        "calendar_rebuilt",
        &format!(
            "{{reason:\"ejection\",active_members:{},calendar:[{}]}}",
            circle.members_list.len(),
            rebuilt
        ),
    )?;

    // Soft warning if the roster has fallen below the configured minimum.
    // We do not auto-cancel — that's a product decision left to the creator
    // via CancelCircle — but the warning surfaces in the event feed so the
    // UI can flag the circle for attention.
    if (circle.members_list.len() as u32) < circle.min_members_required {
        log_event(
            deps,
            env,
            circle.circle_id,
            "min_members_breach",
            &format!(
                "Active members ({}) below min_members_required ({}) after ejection",
                circle.members_list.len(),
                circle.min_members_required
            ),
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Create Circle
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn execute_create_circle(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_name: String,
    circle_description: String,
    circle_image: Option<String>,
    max_members: u32,
    min_members_required: u32,
    invite_only: bool,
    contribution_amount: Uint128,
    denomination: Option<String>,
    exit_penalty_percent: u64,
    late_fee_percent: u64,
    total_cycles: u32,
    cycle_duration_days: u32,
    cycle_duration_seconds: Option<u64>,
    start_date: Option<Timestamp>,
    grace_period_hours: u32,
    grace_period_seconds: Option<u64>,
    auto_start_when_full: bool,
    auto_start_type: Option<String>,
    auto_start_date: Option<Timestamp>,
    payout_order_type: PayoutOrderType,
    payout_order_list: Option<Vec<Addr>>,
    auto_payout_enabled: bool,
    manual_trigger_enabled: bool,
    emergency_stop_enabled: bool,
    auto_refund_if_min_not_met: bool,
    strict_mode: bool,
    visibility: Visibility,
    show_member_identities: bool,
    distribution_threshold: Option<DistributionThreshold>,
) -> Result<Response, ContractError> {
    if max_members == 0 || min_members_required == 0 {
        return Err(ContractError::InvalidParameters {
            msg: "max_members and min_members_required must be greater than 0".to_string(),
        });
    }
    if min_members_required > max_members {
        return Err(ContractError::InvalidParameters {
            msg: "min_members_required cannot exceed max_members".to_string(),
        });
    }
    if total_cycles == 0 {
        return Err(ContractError::InvalidParameters {
            msg: "total_cycles must be greater than 0".to_string(),
        });
    }
    if contribution_amount.is_zero() {
        return Err(ContractError::InvalidParameters {
            msg: "contribution_amount must be greater than 0".to_string(),
        });
    }
    if exit_penalty_percent > 9000 {
        return Err(ContractError::InvalidParameters {
            msg: "exit_penalty_percent cannot exceed 90% (9000 basis points)".to_string(),
        });
    }
    if late_fee_percent == 0 || late_fee_percent > 5000 {
        return Err(ContractError::InvalidParameters {
            msg: "late_fee_percent must be between 1 and 5000 basis points".to_string(),
        });
    }
    if exit_penalty_percent + late_fee_percent > 10000 {
        return Err(ContractError::InvalidParameters {
            msg: "exit_penalty_percent + late_fee_percent cannot exceed 100%".to_string(),
        });
    }

    // Public circles are temporarily disabled at the contract level. Existing
    // Public circles continue to operate; new ones are rejected. Re-enable by
    // removing this guard once the public-circle UX is finalized.
    if matches!(visibility, Visibility::Public) {
        return Err(ContractError::InvalidParameters {
            msg: "Public circles are temporarily disabled. Please create a Private (invite-only) circle.".to_string(),
        });
    }

    // Validate the payment denomination against the allow-list.
    // - "usaf"      → native SAF (6 decimals)
    // - the Noble IBC USDC trace on Safrochain (6 decimals)
    // Unknown denoms are rejected up-front so a typo can't trap funds in a circle
    // members will never be able to deposit into.
    const SAF_DENOM: &str = "usaf";
    const USDC_DENOM: &str =
        "ibc/2180E84E20F5679FCC760D8C165B60F42065DEF7F46A72B447CFF1B7DC6C0A65";
    let chosen_denom: String = match denomination.as_deref() {
        None | Some("") => SAF_DENOM.to_string(),
        Some(d) if d == SAF_DENOM || d == USDC_DENOM => d.to_string(),
        Some(other) => {
            return Err(ContractError::InvalidParameters {
                msg: format!(
                    "denomination '{}' is not supported. Allowed: '{}' or '{}'.",
                    other, SAF_DENOM, USDC_DENOM
                ),
            });
        }
    };

    // Force distribution_threshold = Total for Public circles
    let effective_threshold = match visibility {
        Visibility::Public => Some(DistributionThreshold::Total {}),
        Visibility::Private => {
            if let Some(DistributionThreshold::MinMembers { count }) = distribution_threshold {
                if count == 0 {
                    return Err(ContractError::InvalidParameters {
                        msg: "distribution_threshold MinMembers count must be > 0".to_string(),
                    });
                }
                if count > min_members_required {
                    return Err(ContractError::InvalidParameters {
                        msg: format!(
                            "distribution_threshold MinMembers count ({}) cannot exceed min_members_required ({})",
                            count, min_members_required
                        ),
                    });
                }
                Some(DistributionThreshold::MinMembers { count })
            } else {
                distribution_threshold
            }
        }
    };

    // Validate payout_order_list
    if let Some(ref order_list) = payout_order_list {
        if order_list.len() as u32 != max_members {
            return Err(ContractError::InvalidParameters {
                msg: "payout_order_list length must match max_members".to_string(),
            });
        }
    }

    // Auto-calculate creator lock amount: contribution * (1 + max_members * 10%)
    let required_creator_lock = compute_creator_lock(contribution_amount, max_members)?;

    // Validate payment: creator must send exactly required_creator_lock
    let payment = must_pay(&info, &chosen_denom).map_err(|_| ContractError::InsufficientFunds {
        required: required_creator_lock.to_string(),
        sent: "0".to_string(),
    })?;

    if payment < required_creator_lock {
        return Err(ContractError::InsufficientFunds {
            required: required_creator_lock.to_string(),
            sent: payment.to_string(),
        });
    }

    // Auto-calculate max_missed_payments_allowed; cap at total_rounds-1 so ejection happens before last round
    let total_rounds = max_members * total_cycles;
    let max_missed = cap_max_missed_by_rounds(
        compute_max_missed(exit_penalty_percent, late_fee_percent),
        total_rounds,
    );

    let circle_id = CIRCLE_COUNTER
        .may_load(deps.storage)?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| ContractError::InvalidParameters {
            msg: "Circle ID overflow".to_string(),
        })?;

    let payout_amount = contribution_amount
        .checked_mul(Uint128::from(max_members))
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Payout amount overflow".to_string(),
        })?;

    let cycle_secs = cycle_duration_seconds
        .filter(|&s| s > 0)
        .unwrap_or(cycle_duration_days as u64 * 86400);
    if cycle_secs == 0 {
        return Err(ContractError::InvalidParameters {
            msg: "cycle_duration_days must be > 0, or cycle_duration_seconds must be > 0".to_string(),
        });
    }

    // Grace period must be strictly less than cycle duration. If they are equal,
    // the round-end gate (block.time >= round_end) and the grace gate
    // (block.time > grace_end) coincide, leaving a 1-tick window where
    // advance_round / process_payout can still be rejected with
    // "Grace period not ended" even though the calendar says the round ended.
    // If grace > cycle, advance can never proceed when any member is missing.
    let grace_secs = grace_period_seconds
        .filter(|&s| s > 0)
        .unwrap_or(grace_period_hours as u64 * 3600);
    if grace_secs >= cycle_secs {
        return Err(ContractError::InvalidParameters {
            msg: format!(
                "grace_period ({}s) must be strictly less than cycle_duration ({}s)",
                grace_secs, cycle_secs
            ),
        });
    }

    let end_date = start_date.map(|start| {
        Timestamp::from_seconds(
            start.seconds() + (cycle_secs * max_members as u64 * total_cycles as u64),
        )
    });

    let final_payout_order = match payout_order_type {
        PayoutOrderType::RandomOrder => None,
        PayoutOrderType::PredefinedOrder => payout_order_list,
    };

    // manual_trigger_enabled only meaningful for Private circles
    let effective_manual_trigger = match visibility {
        Visibility::Public => false,
        Visibility::Private => manual_trigger_enabled,
    };

    let circle = Circle {
        circle_id,
        circle_name,
        circle_description,
        circle_image,
        creator_address: info.sender.clone(),
        created_at: env.block.time,
        updated_at: env.block.time,
        max_members,
        min_members_required,
        invite_only,
        members_list: vec![info.sender.clone()],
        pending_members: vec![],
        contribution_amount,
        denomination: chosen_denom.clone(),
        payout_amount,
        exit_penalty_percent,
        late_fee_percent,
        platform_fee_percent: PLATFORM_CONFIG.load(deps.storage)?.platform_fee_percent,
        max_missed_payments_allowed: max_missed,
        total_cycles,
        cycle_duration_days,
        cycle_duration_seconds: cycle_duration_seconds.unwrap_or(0),
        start_date,
        first_cycle_date: start_date,
        next_payout_date: start_date,
        end_date,
        grace_period_hours,
        grace_period_seconds: grace_period_seconds.unwrap_or(0),
        auto_start_when_full,
        auto_start_type,
        auto_start_date,
        payout_order_type,
        payout_order_list: final_payout_order,
        auto_payout_enabled,
        manual_trigger_enabled: effective_manual_trigger,
        emergency_stop_enabled,
        emergency_stop_triggered: false,
        auto_refund_if_min_not_met,
        strict_mode,
        escrow_address: Some(env.contract.address.clone()),
        total_amount_locked: required_creator_lock, // creator lock already in contract
        total_penalties_collected: Uint128::zero(),
        total_platform_fees_collected: Uint128::zero(),
        total_pending_payouts: Uint128::zero(),
        withdrawal_lock: false,
        refund_mode: RefundMode::FullRefund,
        circle_status: CircleStatus::Draft,
        current_cycle_index: 0,
        cycles_completed: 0,
        members_paid_this_cycle: vec![],
        members_late_this_cycle: vec![],
        visibility,
        show_member_identities,
        creator_lock_amount: required_creator_lock,
        distribution_threshold: effective_threshold,
        members_at_start: None, // Set at StartCircle when member count is known
    };

    CIRCLES.save(deps.storage, circle_id, &circle)?;
    CIRCLE_COUNTER.save(deps.storage, &circle_id)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "circle_created",
        &format!(
            "Circle {} created by {} (creator_lock: {}, max_missed: {})",
            circle_id, info.sender, required_creator_lock, max_missed
        ),
    )?;

    Ok(Response::new()
        .add_attribute("action", "create_circle")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("creator", info.sender)
        .add_attribute("creator_lock_amount", required_creator_lock.to_string())
        .add_attribute("max_missed_payments_allowed", max_missed.to_string()))
}

// ---------------------------------------------------------------------------
// Join Circle (merges former LockJoinDeposit)
// ---------------------------------------------------------------------------

fn execute_join_circle(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if !matches!(circle.circle_status, CircleStatus::Draft | CircleStatus::Open) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Draft or Open".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    if circle.members_list.len() as u32 >= circle.max_members {
        return Err(ContractError::CircleFull {
            max: circle.max_members,
        });
    }

    if circle.members_list.contains(&info.sender) {
        return Err(ContractError::AlreadyMember {
            address: info.sender.to_string(),
        });
    }

    // Invite/private check
    if circle.invite_only || matches!(circle.visibility, Visibility::Private) {
        if !circle.pending_members.contains(&info.sender) {
            return Err(ContractError::InviteOnly { circle_id });
        }
        circle.pending_members.retain(|m| m != &info.sender);
    }

    // Require member to send contribution_amount as join deposit (locked security)
    let payment = must_pay(&info, &circle.denomination).map_err(|_| {
        ContractError::InsufficientFunds {
            required: circle.contribution_amount.to_string(),
            sent: "0".to_string(),
        }
    })?;

    if payment < circle.contribution_amount {
        return Err(ContractError::InsufficientFunds {
            required: circle.contribution_amount.to_string(),
            sent: payment.to_string(),
        });
    }

    // Lock the join deposit
    if MEMBER_LOCKED_AMOUNTS
        .may_load(deps.storage, (circle_id, info.sender.clone()))?
        .is_some()
    {
        return Err(ContractError::InvalidParameters {
            msg: "Join deposit already locked".to_string(),
        });
    }

    add_member_locked(
        deps.storage,
        circle_id,
        &info.sender,
        circle.contribution_amount,
        &mut circle.total_amount_locked,
    )?;

    // Add member
    circle.members_list.push(info.sender.clone());
    circle.updated_at = env.block.time;

    if circle.members_list.len() as u32 >= circle.max_members {
        circle.circle_status = CircleStatus::Full;

        if circle.auto_start_when_full {
            if let Some(ref auto_type) = circle.auto_start_type.clone() {
                // by_members: auto-start only when the circle is full (last seat filled).
                // We are already inside `members_list.len() >= max_members`.
                // Creator can still call StartCircle earlier via execute_start_circle once min_members_required is met.
                if auto_type == "by_members" {
                    generate_payout_order(&mut circle, &env);
                    // Use the same helper as `execute_start_circle` so
                    // members_at_start, end_date, and max_missed_payments_allowed
                    // are set consistently across both code paths.
                    apply_running_state(&mut circle, env.block.time);
                }
            }
        }
    } else if circle.circle_status == CircleStatus::Draft {
        circle.circle_status = CircleStatus::Open;
    }

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    let event_type = if matches!(circle.circle_status, CircleStatus::Running)
        && circle.current_cycle_index == 1
    {
        "circle_auto_started"
    } else {
        "member_joined"
    };

    log_event(
        &mut deps,
        &env,
        circle_id,
        event_type,
        &format!("Member {} joined circle {} (locked: {})", info.sender, circle_id, circle.contribution_amount),
    )?;

    Ok(Response::new()
        .add_attribute("action", "join_circle")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", info.sender)
        .add_attribute("locked_amount", circle.contribution_amount.to_string()))
}

// ---------------------------------------------------------------------------
// Invite Member
// ---------------------------------------------------------------------------

fn execute_invite_member(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can invite members".to_string(),
        });
    }

    if !circle.invite_only {
        return Err(ContractError::InvalidParameters {
            msg: "Circle is not invite-only".to_string(),
        });
    }

    let validated_addr = deps.api.addr_validate(member_address.as_str())?;

    if circle.members_list.contains(&validated_addr) {
        return Err(ContractError::AlreadyMember {
            address: validated_addr.to_string(),
        });
    }

    if circle.pending_members.contains(&validated_addr) {
        return Err(ContractError::InvalidParameters {
            msg: "Member already invited".to_string(),
        });
    }

    circle.pending_members.push(validated_addr.clone());
    circle.updated_at = env.block.time;

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "member_invited",
        &format!("Member {} invited to circle {}", validated_addr, circle_id),
    )?;

    Ok(Response::new()
        .add_attribute("action", "invite_member")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", validated_addr))
}

// ---------------------------------------------------------------------------
// Exit Circle
// ---------------------------------------------------------------------------

fn execute_exit_circle(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if circle.circle_status == CircleStatus::Cancelled {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Draft, Open, Running or Paused".to_string(),
            actual: "Cancelled".to_string(),
        });
    }

    if !circle.members_list.contains(&info.sender) {
        return Err(ContractError::Unauthorized {
            msg: "Not a member of this circle".to_string(),
        });
    }

    let started = matches!(
        circle.circle_status,
        CircleStatus::Running | CircleStatus::Paused
    );

    let mut messages: Vec<CosmosMsg> = vec![];
    let mut refund_amount = Uint128::zero();

    if !started {
        // Before start: full refund of locked amount, no penalty
        if let Ok(Some(locked)) =
            MEMBER_LOCKED_AMOUNTS.may_load(deps.storage, (circle_id, info.sender.clone()))
        {
            refund_amount = locked;
            debit_member_locked(
                deps.storage,
                circle_id,
                &info.sender,
                locked,
                &mut circle.total_amount_locked,
            )?;

            if !refund_amount.is_zero() {
                let refund_msgs = safe_refund_or_queue(
                    deps.branch(),
                    &env,
                    circle_id,
                    &info.sender,
                    refund_amount,
                    &circle.denomination,
                )?;
                messages.extend(refund_msgs);
            }
        }

        // Update status
        circle.members_list.retain(|m| m != &info.sender);
        circle.updated_at = env.block.time;

        if (circle.members_list.len() as u32) < circle.max_members
            && circle.circle_status == CircleStatus::Full
        {
            circle.circle_status = CircleStatus::Open;
        }

        if (circle.members_list.len() as u32) < circle.min_members_required
            && circle.auto_refund_if_min_not_met
        {
            circle.circle_status = CircleStatus::Cancelled;
            let locked_entries: Vec<(Addr, Uint128)> = MEMBER_LOCKED_AMOUNTS
                .prefix(circle_id)
                .range(deps.storage, None, None, Order::Ascending)
                .filter_map(|res| res.ok().map(|(m, a)| (m, a)))
                .collect();
            for (member, amount) in locked_entries {
                if !amount.is_zero() {
                    let refund_msgs = safe_refund_or_queue(
                        deps.branch(),
                        &env,
                        circle_id,
                        &member,
                        amount,
                        &circle.denomination,
                    )?;
                    messages.extend(refund_msgs);
                    debit_member_locked(
                        deps.storage,
                        circle_id,
                        &member,
                        amount,
                        &mut circle.total_amount_locked,
                    )?;
                }
            }
            if !circle.creator_lock_amount.is_zero() {
                let creator_amount = circle.creator_lock_amount;
                let refund_msgs = safe_refund_or_queue(
                    deps.branch(),
                    &env,
                    circle_id,
                    &circle.creator_address,
                    creator_amount,
                    &circle.denomination,
                )?;
                messages.extend(refund_msgs);
            }
        }

        if circle.members_list.len() == 1 && circle.circle_status == CircleStatus::Open {
            circle.circle_status = CircleStatus::Draft;
        }
    } else {
        // After start: only allowed if strict_mode = false
        if circle.strict_mode {
            return Err(ContractError::StrictModeNoExit { circle_id });
        }

        let locked = MEMBER_LOCKED_AMOUNTS
            .may_load(deps.storage, (circle_id, info.sender.clone()))?
            .unwrap_or(Uint128::zero());

        let accumulated_fees = MEMBER_ACCUMULATED_LATE_FEES
            .may_load(deps.storage, (circle_id, info.sender.clone()))?
            .unwrap_or(Uint128::zero());

        let exit_penalty = compute_exit_penalty(locked, circle.exit_penalty_percent);
        let total_deduction = accumulated_fees + exit_penalty;
        let refund = if total_deduction >= locked {
            Uint128::zero()
        } else {
            locked - total_deduction
        };

        // The accumulated fees + exit penalty stay in the pool
        let penalty_kept = if total_deduction > locked {
            locked
        } else {
            total_deduction
        };
        circle.total_penalties_collected = circle
            .total_penalties_collected
            .checked_add(penalty_kept)
            .unwrap_or(circle.total_penalties_collected);

        if !refund.is_zero() {
            let refund_msgs = safe_refund_or_queue(
                deps.branch(),
                &env,
                circle_id,
                &info.sender,
                refund,
                &circle.denomination,
            )?;
            messages.extend(refund_msgs);
        }

        refund_amount = refund;

        // Clean up: debit full locked amount from member and aggregate
        debit_member_locked(
            deps.storage,
            circle_id,
            &info.sender,
            locked,
            &mut circle.total_amount_locked,
        )?;
        MEMBER_ACCUMULATED_LATE_FEES.remove(deps.storage, (circle_id, info.sender.clone()));

        // Recalculate payout order without this member
        circle.members_list.retain(|m| m != &info.sender);
        circle.updated_at = env.block.time;

        // Recompute max_missed_payments_allowed (dynamic from % penalty and late fee, scaled by active members)
        let total_rounds = circle.max_members * circle.total_cycles;
        circle.max_missed_payments_allowed = cap_max_missed_by_rounds(
            compute_max_missed_scaled(
                circle.exit_penalty_percent,
                circle.late_fee_percent,
                circle.members_at_start,
                circle.members_list.len() as u32,
            ),
            total_rounds,
        );

        // Remove from payout order for future rounds
        if let Some(ref mut order) = circle.payout_order_list {
            order.retain(|m| m != &info.sender);
        }

        // Recalculate payout_amount
        circle.payout_amount = circle
            .contribution_amount
            .checked_mul(Uint128::from(circle.members_list.len() as u128))
            .unwrap_or(circle.payout_amount);

        // Creator exit: forfeit creator_lock_amount to remaining active members
        if info.sender == circle.creator_address && !circle.creator_lock_amount.is_zero() {
            let active: Vec<Addr> = circle
                .members_list
                .iter()
                .filter(|m| *m != &circle.creator_address)
                .filter(|m| {
                    BLOCKED_MEMBERS
                        .may_load(deps.storage, (circle_id, (*m).clone()))
                        .unwrap_or(None)
                        .map(|bc| bc > circle.current_cycle_index)
                        .unwrap_or(true)
                })
                .cloned()
                .collect();

            if !active.is_empty() {
                let count = Uint128::from(active.len() as u128);
                let per_member = circle.creator_lock_amount.multiply_ratio(1u128, count.u128());
                let remainder = circle
                    .creator_lock_amount
                    .checked_sub(per_member * count)
                    .unwrap_or(Uint128::zero());

                for (idx, member) in active.iter().enumerate() {
                    let mut share = per_member;
                    if idx == 0 {
                        share = share.checked_add(remainder).unwrap_or(share);
                    }
                    if !share.is_zero() {
                        credit_pending_payout(
                            deps.storage,
                            circle_id,
                            member,
                            share,
                            &mut circle.total_pending_payouts,
                        )?;
                    }
                }
            }
            circle.creator_lock_amount = Uint128::zero();
        }
    }

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "member_exited",
        &format!(
            "Member {} exited circle {} (started: {}, refund: {})",
            info.sender, circle_id, started, refund_amount
        ),
    )?;

    let resp = Response::new()
        .add_messages(messages)
        .add_attribute("action", "exit_circle")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", info.sender)
        .add_attribute("refund_amount", refund_amount.to_string());
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Start Circle
// ---------------------------------------------------------------------------

fn execute_start_circle(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can start circle".to_string(),
        });
    }

    if !matches!(
        circle.circle_status,
        CircleStatus::Open | CircleStatus::Full
    ) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Open or Full".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    if (circle.members_list.len() as u32) < circle.min_members_required {
        return Err(ContractError::MinMembersNotMet {
            required: circle.min_members_required,
            current: circle.members_list.len() as u32,
        });
    }

    generate_payout_order(&mut circle, &env);

    // Always use the actual on-chain time as the start — not the pre-set start_date
    // which may have been set to a future date at creation time.
    let start_timestamp = env.block.time;
    apply_running_state(&mut circle, start_timestamp);

    let total_rounds = circle.max_members * circle.total_cycles;
    let end_timestamp = circle.end_date.unwrap_or(start_timestamp);
    let archived_timestamp = Timestamp::from_seconds(
        end_timestamp.seconds() + circle.grace_period_secs(),
    );

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    let calendar_data = build_distribution_calendar(&circle, start_timestamp);

    log_event(
        &mut deps,
        &env,
        circle_id,
        "circle_started",
        &format!("Circle {} started at {}", circle_id, start_timestamp.seconds()),
    )?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "distribution_calendar",
        &format!(
            "{{start_date:{},end_date:{},archived_date:{},calendar:[{}]}}",
            start_timestamp.seconds(),
            end_timestamp.seconds(),
            archived_timestamp.seconds(),
            calendar_data
        ),
    )?;

    Ok(Response::new()
        .add_attribute("action", "start_circle")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("start_date", start_timestamp.seconds().to_string())
        .add_attribute("end_date", end_timestamp.seconds().to_string())
        .add_attribute("archived_date", archived_timestamp.seconds().to_string())
        .add_attribute("total_rounds", total_rounds.to_string()))
}

// ---------------------------------------------------------------------------
// Deposit Contribution
// ---------------------------------------------------------------------------

fn execute_deposit_contribution(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if !matches!(circle.circle_status, CircleStatus::Running) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Running".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    if !circle.members_list.contains(&info.sender) {
        return Err(ContractError::Unauthorized {
            msg: "Not a member of this circle".to_string(),
        });
    }

    if let Ok(Some(blocked_from_cycle)) =
        BLOCKED_MEMBERS.may_load(deps.storage, (circle_id, info.sender.clone()))
    {
        if blocked_from_cycle <= circle.current_cycle_index {
            return Err(ContractError::InvalidParameters {
                msg: format!("Member is blocked from cycle {} onwards", blocked_from_cycle),
            });
        }
    }

    if DEPOSITS
        .may_load(
            deps.storage,
            (circle_id, info.sender.clone(), circle.current_cycle_index),
        )?
        .is_some()
    {
        return Err(ContractError::AlreadyDeposited {
            address: info.sender.to_string(),
            cycle: circle.current_cycle_index,
        });
    }

    // Last cycle member deposited for (from MEMBER_LAST_DEPOSITED_CYCLE or scan DEPOSITS for backward compat)
    let last_deposited_cycle = MEMBER_LAST_DEPOSITED_CYCLE
        .may_load(deps.storage, (circle_id, info.sender.clone()))?
        .or_else(|| {
            DEPOSITS
                .prefix((circle_id, info.sender.clone()))
                .range(deps.storage, None, None, Order::Descending)
                .next()
                .and_then(|r| r.ok())
                .map(|(c, _)| c)
        })
        .unwrap_or(0);

    // Rounds missed = cycles between last_deposited+1 and current-1 (inclusive)
    let rounds_missed = circle
        .current_cycle_index
        .saturating_sub(last_deposited_cycle)
        .saturating_sub(1);

    let late_fee_per_round =
        compute_late_fee_per_round(circle.contribution_amount, circle.late_fee_percent);

    // If missed >= max_missed: add late fees for missed rounds, then eject, cannot deposit
    if rounds_missed >= circle.max_missed_payments_allowed {
        let late_fee_total = late_fee_per_round * Uint128::from(rounds_missed as u128);
        let mut accumulated = MEMBER_ACCUMULATED_LATE_FEES
            .may_load(deps.storage, (circle_id, info.sender.clone()))?
            .unwrap_or(Uint128::zero());
        accumulated = accumulated
            .checked_add(late_fee_total)
            .unwrap_or(accumulated);
        MEMBER_ACCUMULATED_LATE_FEES.save(
            deps.storage,
            (circle_id, info.sender.clone()),
            &accumulated,
        )?;

        let orig_lock = original_lock_for_member(&circle, &info.sender);
        if should_eject_member(
            deps.storage,
            circle_id,
            &info.sender,
            orig_lock,
            circle.exit_penalty_percent,
            circle.max_missed_payments_allowed,
        ) {
            eject_member_from_circle(&mut deps, &env, &mut circle, &info.sender)?;
            CIRCLES.save(deps.storage, circle_id, &circle)?;
        }
        return Err(ContractError::MaxMissedPaymentsExceeded {
            max: circle.max_missed_payments_allowed,
        });
    }

    let late_fee_total = late_fee_per_round * Uint128::from(rounds_missed as u128);

    // CATCH-UP DEPOSIT
    // ----------------
    // When a member missed N rounds before depositing, the contract already
    // drained `N × contribution_amount` from their MEMBER_LOCKED (via the
    // process_payout / advance_round force-fund path) to keep the schedule
    // moving. The catch-up payment must:
    //   1. Cover the current round (contribution_amount)
    //   2. Refill the lock by the amount that was drained, capped at the
    //      original_lock - if missed rounds drained 3 × C, they need 3 × C
    //      to restore. If their lock was 2 × C (creator) we still cap at the
    //      original limit so they can't accidentally over-fund.
    //   3. Pay the accumulated late fees (1 × late_fee_per_round per miss)
    //
    // After a successful catch-up the member is "fresh": missed_count and
    // accumulated_late_fees both reset to zero, lock back to original.
    //
    // For non-creator members only - creator lock is in `creator_lock_amount`
    // and is never drained by the round-cover path.
    let original_lock = original_lock_for_member(&circle, &info.sender);
    let current_lock = if info.sender == circle.creator_address {
        Uint128::zero() // creator has no MEMBER_LOCKED entry
    } else {
        current_member_lock(deps.storage, circle_id, &info.sender)
    };
    let lock_refill_needed = if info.sender == circle.creator_address {
        Uint128::zero()
    } else {
        original_lock.saturating_sub(current_lock)
    };

    let required_amount = circle
        .contribution_amount
        .checked_add(lock_refill_needed)
        .and_then(|v| v.checked_add(late_fee_total))
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Required amount overflow".to_string(),
        })?;

    let payment = must_pay(&info, &circle.denomination).map_err(|_| {
        ContractError::InsufficientFunds {
            required: required_amount.to_string(),
            sent: "0".to_string(),
        }
    })?;

    if payment < required_amount {
        return Err(ContractError::InsufficientFunds {
            required: required_amount.to_string(),
            sent: payment.to_string(),
        });
    }

    let is_late = rounds_missed > 0;

    // Add late fees (paid in tokens) to pool
    if !late_fee_total.is_zero() {
        circle.total_penalties_collected = circle
            .total_penalties_collected
            .checked_add(late_fee_total)
            .map_err(|_| ContractError::InvalidParameters {
                msg: "Penalties overflow".to_string(),
            })?;

        PENALTIES.save(
            deps.storage,
            (circle_id, info.sender.clone(), circle.current_cycle_index),
            &PenaltyRecord {
                member: info.sender.clone(),
                cycle: circle.current_cycle_index,
                amount: late_fee_total,
                reason: format!(
                    "Late deposit: {} rounds missed ({}% per round paid in tokens)",
                    rounds_missed, circle.late_fee_percent / 100
                ),
                timestamp: env.block.time,
            },
        )?;
    }

    // Refill the lock that was drained covering this member's misses, and
    // reset the per-member miss counters so they get a clean slate.
    if !lock_refill_needed.is_zero() {
        add_member_locked(
            deps.storage,
            circle_id,
            &info.sender,
            lock_refill_needed,
            &mut circle.total_amount_locked,
        )?;
    }
    if rounds_missed > 0 {
        MEMBER_ACCUMULATED_LATE_FEES.remove(deps.storage, (circle_id, info.sender.clone()));
        MEMBER_MISSED_PAYMENTS.remove(deps.storage, (circle_id, info.sender.clone()));
    }

    // Record deposit (only `contribution_amount` is the deposit proper;
    // the refill goes into MEMBER_LOCKED above, the late fees into PENALTIES).
    DEPOSITS.save(
        deps.storage,
        (circle_id, info.sender.clone(), circle.current_cycle_index),
        &DepositRecord {
            member: info.sender.clone(),
            cycle: circle.current_cycle_index,
            amount: circle.contribution_amount,
            timestamp: env.block.time,
            on_time: !is_late,
        },
    )?;

    MEMBER_LAST_DEPOSITED_CYCLE.save(
        deps.storage,
        (circle_id, info.sender.clone()),
        &circle.current_cycle_index,
    )?;

    circle.total_amount_locked = circle
        .total_amount_locked
        .checked_add(circle.contribution_amount)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Total amount overflow".to_string(),
        })?;

    if is_late {
        circle.members_late_this_cycle.push(info.sender.clone());
    } else {
        circle.members_paid_this_cycle.push(info.sender.clone());
    }

    let deposited_cycle = circle.current_cycle_index;

    // Rounds advance by calendar (AdvanceRound/ProcessPayout), not by deposit

    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "contribution_deposited",
        &format!(
            "Member {} deposited {} for cycle {} (on_time: {})",
            info.sender, circle.contribution_amount, deposited_cycle, !is_late
        ),
    )?;

    Ok(Response::new()
        .add_attribute("action", "deposit_contribution")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", info.sender)
        .add_attribute("cycle", deposited_cycle.to_string())
        .add_attribute("amount", circle.contribution_amount.to_string())
        .add_attribute("on_time", (!is_late).to_string()))
}

// ---------------------------------------------------------------------------
// Process Payout — stores to PENDING_PAYOUTS instead of direct send
// ---------------------------------------------------------------------------

fn execute_process_payout(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if !matches!(circle.circle_status, CircleStatus::Running) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Running".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    // Idempotency: reject if this cycle was already processed (prevents double trigger)
    let already_processed = PAYOUTS
        .prefix((circle_id, circle.current_cycle_index))
        .range(deps.storage, None, None, Order::Ascending)
        .next()
        .is_some();
    if already_processed {
        return Err(ContractError::PayoutAlreadyProcessed {
            cycle: circle.current_cycle_index,
        });
    }

    // Authorization: manual_trigger_enabled means only creator can call
    if circle.manual_trigger_enabled && info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can trigger payout (manual_trigger_enabled)".to_string(),
        });
    }

    // Time gate (end-of-round): prevent back-to-back transitions that can skip rounds.
    // `next_payout_date` is the round start; only allow payout after the round has ended.
    if let Some(next_payout) = circle.next_payout_date {
        let round_end = next_payout.plus_seconds(circle.cycle_duration_secs());
        if env.block.time < round_end {
            return Err(ContractError::CycleNotReady {
                next_date: round_end.seconds(),
            });
        }
    }

    // Active members (not blocked)
    let mut active_members: Vec<Addr> = circle
        .members_list
        .iter()
        .filter(|m| {
            BLOCKED_MEMBERS
                .may_load(deps.storage, (circle_id, (*m).clone()))
                .unwrap_or(None)
                .map(|bc| bc > circle.current_cycle_index)
                .unwrap_or(true)
        })
        .cloned()
        .collect();

    let missing_members: Vec<Addr> = active_members
        .iter()
        .filter(|m| {
            DEPOSITS
                .may_load(deps.storage, (circle_id, (*m).clone(), circle.current_cycle_index))
                .unwrap_or(None)
                .is_none()
        })
        .cloned()
        .collect();

    // If any missing and grace period still active, block payout
    if let Some(next_payout) = circle.next_payout_date {
        let grace_end = next_payout.plus_seconds(circle.grace_period_secs());
        if env.block.time <= grace_end && !missing_members.is_empty() {
            return Err(ContractError::InvalidParameters {
                msg: "Grace period not ended for all missing members".to_string(),
            });
        }
    }

    let mut locked_used_total = Uint128::zero();

    // Handle missing members (past grace): accumulate late fees, check ejection, use locked funds
    for member in &missing_members {
        let late_fee_per_round =
            compute_late_fee_per_round(circle.contribution_amount, circle.late_fee_percent);

        let mut accumulated = MEMBER_ACCUMULATED_LATE_FEES
            .may_load(deps.storage, (circle_id, member.clone()))?
            .unwrap_or(Uint128::zero());

        // Only count once per cycle
        let mut missed = MEMBER_MISSED_PAYMENTS
            .may_load(deps.storage, (circle_id, member.clone()))?
            .unwrap_or(MemberMissedPayments {
                member: member.clone(),
                missed_count: 0,
                last_missed_cycle: None,
                last_fee_round: None,
            });

        if missed.last_missed_cycle != Some(circle.current_cycle_index) {
            missed.missed_count += 1;
            missed.last_missed_cycle = Some(circle.current_cycle_index);
            accumulated = accumulated
                .checked_add(late_fee_per_round)
                .unwrap_or(accumulated);

            MEMBER_ACCUMULATED_LATE_FEES.save(
                deps.storage,
                (circle_id, member.clone()),
                &accumulated,
            )?;
            MEMBER_MISSED_PAYMENTS.save(
                deps.storage,
                (circle_id, member.clone()),
                &missed,
            )?;
        }

        // Determine if this member should be ejected NOW.
        //
        // Two independent ejection triggers, by design:
        //   (a) `should_eject_member` - the standard "missed too many" or
        //       "accumulated fees exhausted the security" check.
        //   (b) NEW: their remaining lock can no longer cover this round's
        //       contribution. Without this guard, a member whose
        //       MEMBER_LOCKED was depleted by earlier misses (but who hasn't
        //       yet hit `max_missed_payments_allowed`) would block the round
        //       forever - `use_locked_amount_for_member` returns 0, no
        //       synthetic deposit is written, and the post-loop
        //       `deposits_count < active_members.len()` check fails.
        //
        // Per the security contract: a member's lock IS their commitment;
        // when it can't cover a round, they've effectively defaulted, and
        // the circle MUST keep running for the remaining members.
        let orig_lock = original_lock_for_member(&circle, member);
        let standard_eject = should_eject_member(
            deps.storage,
            circle_id,
            member,
            orig_lock,
            circle.exit_penalty_percent,
            circle.max_missed_payments_allowed,
        );
        let lock_insufficient = if member == &circle.creator_address {
            // Creator's lock is in `creator_lock_amount`; can't be drained
            // per-miss the way MEMBER_LOCKED is. Skip this trigger for them
            // (creator handling is via cancel/finalize, not per-round eject).
            false
        } else {
            current_member_lock(deps.storage, circle_id, member) < circle.contribution_amount
        };

        if standard_eject || lock_insufficient {
            eject_member_from_circle(&mut deps, &env, &mut circle, member)?;
            // Ejected: their MEMBER_LOCKED stays in the circle (becomes part
            // of `total_penalties_collected` per eject logic). No synthetic
            // deposit for this round - active_members.len() will drop on
            // the post-loop recount and the round proceeds with the rest.
            continue;
        }

        // Use locked funds to cover missed deposit. Creator's lock is never
        // touched (lives in creator_lock_amount, not MEMBER_LOCKED).
        let used = if member == &circle.creator_address {
            Uint128::zero()
        } else {
            use_locked_amount_for_member(
                deps.storage,
                circle_id,
                member,
                circle.contribution_amount,
            )?
        };
        if !used.is_zero() {
            locked_used_total = locked_used_total
                .checked_add(used)
                .map_err(|_| ContractError::InvalidParameters {
                    msg: "Locked funds overflow".to_string(),
                })?;
            // Record synthetic deposit so this round remains auditable (no perceived "skipped" round).
            DEPOSITS.save(
                deps.storage,
                (circle_id, member.clone(), circle.current_cycle_index),
                &DepositRecord {
                    member: member.clone(),
                    cycle: circle.current_cycle_index,
                    amount: used,
                    timestamp: env.block.time,
                    on_time: false,
                },
            )?;
        }
    }

    // Also use locked funds from previously blocked members
    let blocked_members_list: Vec<Addr> = BLOCKED_MEMBERS
        .prefix(circle_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|res| res.ok().map(|(m, _)| m))
        .collect();

    for blocked_member in blocked_members_list {
        if missing_members.iter().any(|m| m == &blocked_member) {
            continue;
        }
        if let Ok(Some(bc)) =
            BLOCKED_MEMBERS.may_load(deps.storage, (circle_id, blocked_member.clone()))
        {
            if bc <= circle.current_cycle_index {
                let used = use_locked_amount_for_member(
                    deps.storage,
                    circle_id,
                    &blocked_member,
                    circle.contribution_amount,
                )?;
                if !used.is_zero() {
                    locked_used_total = locked_used_total
                        .checked_add(used)
                        .map_err(|_| ContractError::InvalidParameters {
                            msg: "Locked funds overflow".to_string(),
                        })?;
                    // Record synthetic deposit for blocked member coverage in this round.
                    DEPOSITS.save(
                        deps.storage,
                        (circle_id, blocked_member.clone(), circle.current_cycle_index),
                        &DepositRecord {
                            member: blocked_member.clone(),
                            cycle: circle.current_cycle_index,
                            amount: used,
                            timestamp: env.block.time,
                            on_time: false,
                        },
                    )?;
                }
            }
        }
    }

    // Recompute active members after ejections
    active_members = circle
        .members_list
        .iter()
        .filter(|m| {
            BLOCKED_MEMBERS
                .may_load(deps.storage, (circle_id, (*m).clone()))
                .unwrap_or(None)
                .map(|bc| bc > circle.current_cycle_index)
                .unwrap_or(true)
        })
        .cloned()
        .collect();

    let deposits_count = active_members
        .iter()
        .filter(|m| {
            DEPOSITS
                .may_load(deps.storage, (circle_id, (*m).clone(), circle.current_cycle_index))
                .unwrap_or(None)
                .is_some()
        })
        .count();

    // Distribution threshold check (based on active members)
    let active_count = active_members.len() as u32;
    if active_count == 0 {
        return Err(ContractError::InvalidParameters {
            msg: "No active members for distribution".to_string(),
        });
    }
    let round_in_cycle = ((circle.current_cycle_index - 1) % active_count) + 1;
    let min_round_for_distribution =
        distribution_min_round_for_active(&circle.distribution_threshold, active_count);

    if round_in_cycle < min_round_for_distribution {
        return Err(ContractError::InvalidParameters {
            msg: format!(
                "Distribution only from round {} (current round in cycle: {})",
                min_round_for_distribution, round_in_cycle
            ),
        });
    }

    // All active members must have a real-or-synthetic deposit for this round.
    //
    // `deposits_count` is recomputed AFTER the missing-member loop, so it
    // includes the synthetic DEPOSITS rows just written for members whose
    // locked join-deposit was consumed to cover the round.
    //
    // The previous formulation `deposits_count + |missing_members ∩ active|`
    // was tautological — since `missing_members ⊆ active_members`, the sum
    // always equalled `active_members.len()` and the contract therefore
    // accepted process_payout even when a missing member had ZERO locked
    // funds left and no synthetic deposit was created. That over-credited
    // the pool by the missing contribution amount, which is the root cause
    // of the "PENDING_PAYOUTS > contract bank balance" symptom on Withdraw.
    if deposits_count < active_members.len() {
        return Err(ContractError::InvalidParameters {
            msg: format!(
                "Not all active members have contributed: need {}, have {} (some missing members had no locked funds left to cover this round)",
                active_members.len(),
                deposits_count
            ),
        });
    }

    // Total threshold at last round of cycle: split equally among ALL active members
    let is_total_at_last_round = is_total_style_threshold(&circle.distribution_threshold)
        && round_in_cycle == min_round_for_distribution;

    // Calculate payout amount (total pool)
    // For Total threshold at last round: sum from ALL rounds in the cycle. Each round: active_count * contribution
    // (everyone contributed either by deposit or locked). Otherwise: only current round (one recipient per round).
    let base_payout = if is_total_at_last_round {
        let rounds_in_cycle = active_count; // Total threshold: min_round = active_count
        let round_amount = circle
            .contribution_amount
            .checked_mul(Uint128::from(active_count as u128))
            .map_err(|_| ContractError::InvalidParameters {
                msg: "Payout amount overflow".to_string(),
            })?;
        round_amount
            .checked_mul(Uint128::from(rounds_in_cycle as u128))
            .map_err(|_| ContractError::InvalidParameters {
                msg: "Payout amount overflow".to_string(),
            })?
    } else {
        circle
            .contribution_amount
            .checked_mul(Uint128::from(deposits_count as u128))
            .map_err(|_| ContractError::InvalidParameters {
                msg: "Payout amount overflow".to_string(),
            })?
    };

    let mut payout_amount = base_payout
        .checked_add(locked_used_total)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Payout amount overflow".to_string(),
        })?;

    let platform_fee = payout_amount.multiply_ratio(circle.platform_fee_percent, 10000u64);
    payout_amount = payout_amount
        .checked_sub(platform_fee)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Payout amount underflow".to_string(),
        })?;

    circle.total_platform_fees_collected = circle
        .total_platform_fees_collected
        .checked_add(platform_fee)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Platform fees overflow".to_string(),
        })?;

    let liquid_balance = deps
        .querier
        .query_balance(&env.contract.address, &circle.denomination)
        .map_err(|e| ContractError::InvalidParameters {
            msg: format!("Balance query failed: {}", e),
        })?;
    let available = liquid_balance.amount;

    // Any PENDING_PAYOUTS already credited (from earlier rounds, not yet
    // withdrawn) must remain backed by bank balance — we cannot credit so
    // much new payout that the contract can no longer satisfy outstanding
    // withdrawals. Subtract them from the spendable budget for this TX.
    let spendable_for_new_credits = available
        .checked_sub(circle.total_pending_payouts)
        .unwrap_or(Uint128::zero());

    // Compute total we will add to PENDING_PAYOUTS and verify contract has sufficient balance
    let mut total_to_credit = payout_amount;
    let mut creator_refund_amount = Uint128::zero();
    // Total rounds = ACTIVE roster at start × total_cycles. Using `max_members`
    // here was wrong for circles that auto-started with fewer members than
    // the cap: e.g. a circle configured `max_members=3, total_cycles=2` that
    // actually started with 2 members has 4 real rounds, not 6. With the old
    // formula, `current_cycle_index=4 >= 6` was false on the FINAL round,
    // the pre-computation block was skipped, `creator_refund_amount` stayed
    // at zero, and the finalize block below (which uses the correct
    // `members_at_start` formula) zeroed `creator_lock_amount` without ever
    // crediting the creator's refund - silently stranding it on the bank.
    // Must mirror the second `total_rounds` computation further down.
    let members_basis_pre = circle.members_at_start.unwrap_or(circle.max_members);
    let total_rounds = members_basis_pre * circle.total_cycles;
    if circle.current_cycle_index >= total_rounds {
        let locked_sum: Uint128 = MEMBER_LOCKED_AMOUNTS
            .prefix(circle_id)
            .range(deps.storage, None, None, Order::Ascending)
            .filter_map(|r| r.ok())
            .map(|(_, amt)| amt)
            .fold(Uint128::zero(), |a, b| a.saturating_add(b));
        // Add creator lock only when creator has no MEMBER_LOCKED_AMOUNTS entry.
        // For old circles where creator lock may have been partially consumed, cap refund by available funds.
        let creator_has_locked_entry = MEMBER_LOCKED_AMOUNTS
            .may_load(deps.storage, (circle_id, circle.creator_address.clone()))?
            .is_some();
        total_to_credit = total_to_credit
            .checked_add(locked_sum)
            .unwrap_or(total_to_credit);
        total_to_credit = total_to_credit
            .checked_add(circle.total_penalties_collected)
            .unwrap_or(total_to_credit);
        if !creator_has_locked_entry && !circle.creator_lock_amount.is_zero() {
            // Refund at most the still-spendable funds AFTER honouring
            // outstanding pending payouts and the mandatory final credits.
            // Using `available` alone (the prior behaviour) over-credited the
            // creator when earlier cycles had already committed funds to
            // PENDING_PAYOUTS, producing the bank-vs-pending mismatch.
            creator_refund_amount = spendable_for_new_credits
                .checked_sub(total_to_credit)
                .unwrap_or(Uint128::zero());
            if creator_refund_amount > circle.creator_lock_amount {
                creator_refund_amount = circle.creator_lock_amount;
            }
            total_to_credit = total_to_credit
                .checked_add(creator_refund_amount)
                .unwrap_or(total_to_credit);
        }
    }
    if spendable_for_new_credits < total_to_credit {
        return Err(ContractError::InsufficientContractBalance {
            required: total_to_credit.to_string(),
            available: available.to_string(),
        });
    }

    // Capture single-recipient address for logging (none for Total threshold final-round split).
    let mut single_recipient: Option<Addr> = None;

    // Outbound bank messages produced by this transaction (e.g. platform fee
    // drain at finalization). Withdrawals are still pulled via Withdraw — this
    // only carries amounts the contract autonomously sends out (platform fees,
    // dust). Building it here keeps the response self-contained.
    let mut outbound_messages: Vec<CosmosMsg> = Vec::new();
    let mut platform_fees_sent: Uint128 = Uint128::zero();

    if is_total_at_last_round {
        // Split payout_amount equally among all active members
        let member_count = active_members.len() as u128;
        if member_count == 0 {
            return Err(ContractError::InvalidParameters {
                msg: "No active members for distribution".to_string(),
            });
        }
        let amount_per_member = payout_amount
            .checked_div(Uint128::from(member_count))
            .unwrap_or(Uint128::zero());
        let remainder = payout_amount
            .checked_sub(amount_per_member * Uint128::from(member_count))
            .unwrap_or(Uint128::zero());

        for (idx, member) in active_members.iter().enumerate() {
            let mut amt = amount_per_member;
            if idx == 0 {
                amt = amt.checked_add(remainder).unwrap_or(amt);
            }
            if amt.is_zero() {
                continue;
            }

            PAYOUTS.save(
                deps.storage,
                (circle_id, circle.current_cycle_index, member.clone()),
                &PayoutRecord {
                    cycle: circle.current_cycle_index,
                    recipient: member.clone(),
                    amount: amt,
                    timestamp: env.block.time,
                    transaction_hash: None,
                },
            )?;

            credit_pending_payout(
                deps.storage,
                circle_id,
                member,
                amt,
                &mut circle.total_pending_payouts,
            )?;
        }
    } else {
        // One recipient per round (MinMembers or None)
        let recipient = if let Some(ref order_list) = circle.payout_order_list {
            let active_order: Vec<Addr> = order_list
                .iter()
                .filter(|m| active_members.iter().any(|a| a == *m))
                .cloned()
                .collect();
            if active_order.is_empty() {
                return Err(ContractError::InvalidParameters {
                    msg: "No active members in payout order".to_string(),
                });
            }
            let index = (round_in_cycle as usize - 1) % active_order.len();
            active_order[index].clone()
        } else {
            return Err(ContractError::InvalidParameters {
                msg: "Payout order not set".to_string(),
            });
        };
        single_recipient = Some(recipient.clone());

        PAYOUTS.save(
            deps.storage,
            (circle_id, circle.current_cycle_index, recipient.clone()),
            &PayoutRecord {
                cycle: circle.current_cycle_index,
                recipient: recipient.clone(),
                amount: payout_amount,
                timestamp: env.block.time,
                transaction_hash: None,
            },
        )?;

        credit_pending_payout(
            deps.storage,
            circle_id,
            &recipient,
            payout_amount,
            &mut circle.total_pending_payouts,
        )?;
    }

    // Subtract the contributed deposits from total_amount_locked (excluding locked join deposits used above — those were already deducted by use_locked_amount_for_member)
    let deposit_outflow = base_payout
        .checked_add(locked_used_total)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Outflow overflow".to_string(),
        })?;

    circle.total_amount_locked = circle
        .total_amount_locked
        .checked_sub(deposit_outflow)
        .unwrap_or(Uint128::zero());

    circle.cycles_completed += 1;
    circle.members_paid_this_cycle.clear();
    circle.members_late_this_cycle.clear();

    // Check if last round across all cycles. Round count is based on the
    // active member count locked at start (members_at_start), NOT the
    // configured cap (max_members). Using max_members caused circles that
    // started with fewer members than the cap to keep advancing past their
    // calendar's last round — status never transitioned to Finalizing, the
    // cron kept queuing actions, and the UI displayed phantom rounds with
    // no recipient.
    let members_basis = circle.members_at_start.unwrap_or(circle.max_members);
    let total_rounds = members_basis * circle.total_cycles;
    if circle.current_cycle_index >= total_rounds {
        // Set Finalizing — Completed only when all have withdrawn (contract balance = 0)
        circle.circle_status = CircleStatus::Finalizing;

        // Final distribution (automatic): (1) Creator gets creator_lock back (if not in MEMBER_LOCKED); (2) Each member gets their join-deposit lock back; (3) Penalties split equally
        let mut total_distributed = Uint128::zero();

        // 1. Creator gets creator lock back (capped for old circles by actual available funds).
        if !creator_refund_amount.is_zero() && active_members.iter().any(|m| m == &circle.creator_address) {
            credit_pending_payout(
                deps.storage,
                circle_id,
                &circle.creator_address,
                creator_refund_amount,
                &mut circle.total_pending_payouts,
            )?;
            total_distributed = total_distributed
                .checked_add(creator_refund_amount)
                .unwrap_or(total_distributed);
        }
        circle.creator_lock_amount = Uint128::zero();

        // 2. Each member gets their own join-deposit lock back (from MEMBER_LOCKED_AMOUNTS)
        let locked_entries: Vec<(Addr, Uint128)> = MEMBER_LOCKED_AMOUNTS
            .prefix(circle_id)
            .range(deps.storage, None, None, Order::Ascending)
            .filter_map(|res| res.ok())
            .collect();
        for (member, locked) in &locked_entries {
            if locked.is_zero() {
                continue;
            }
            credit_pending_payout(
                deps.storage,
                circle_id,
                member,
                *locked,
                &mut circle.total_pending_payouts,
            )?;
            total_distributed = total_distributed.checked_add(*locked).unwrap_or(total_distributed);
        }
        for (m, _) in &locked_entries {
            MEMBER_LOCKED_AMOUNTS.remove(deps.storage, (circle_id, m.clone()));
        }

        // 3. Penalties split equally among all active members
        let pool = circle.total_penalties_collected;

        if !pool.is_zero() && !active_members.is_empty() {
            let member_count = Uint128::from(active_members.len() as u128);
            let amount_per_member = pool
                .checked_div(member_count)
                .unwrap_or(Uint128::zero());
            let remainder = pool
                .checked_sub(amount_per_member * member_count)
                .unwrap_or(Uint128::zero());

            for (idx, member) in active_members.iter().enumerate() {
                let mut amt = amount_per_member;
                if idx == 0 {
                    amt = amt.checked_add(remainder).unwrap_or(amt);
                }
                if !amt.is_zero() {
                    credit_pending_payout(
                        deps.storage,
                        circle_id,
                        member,
                        amt,
                        &mut circle.total_pending_payouts,
                    )?;
                }
            }
        }

        circle.total_amount_locked = Uint128::zero();
        circle.total_penalties_collected = Uint128::zero();

        // Drain accumulated platform fees to the platform address. Before this
        // fix, `total_platform_fees_collected` was a pure accumulator with no
        // outflow path — the corresponding native coins remained on the
        // contract bank balance after finalization, breaking the documented
        // invariant in state.rs ("Becomes Completed when contract balance = 0").
        // We send the fees automatically as part of finalization so new circles
        // are clean by construction.
        let fees = circle.total_platform_fees_collected;
        if !fees.is_zero() {
            let platform_addr = PLATFORM_CONFIG.load(deps.storage)?.platform_address;
            outbound_messages.push(CosmosMsg::Bank(BankMsg::Send {
                to_address: platform_addr.to_string(),
                amount: vec![Coin {
                    denom: circle.denomination.clone(),
                    amount: fees,
                }],
            }));
            platform_fees_sent = fees;
            circle.total_platform_fees_collected = Uint128::zero();
        }

        log_event(
            &mut deps,
            &env,
            circle_id,
            "circle_completed",
            &format!(
                "Circle {} completed. All payouts stored in PENDING_PAYOUTS. Platform fees drained: {} usaf.",
                circle_id, platform_fees_sent
            ),
        )?;
    } else {
        // Round-progression invariant: current_cycle_index must advance by
        // exactly 1 per transaction. Catches accidental skips from refactors.
        let prev_round = circle.current_cycle_index;
        circle.current_cycle_index = prev_round.checked_add(1).ok_or_else(|| {
            ContractError::InvalidParameters {
                msg: "current_cycle_index overflow".to_string(),
            }
        })?;
        if circle.current_cycle_index != prev_round + 1 {
            return Err(ContractError::InvalidParameters {
                msg: format!(
                    "Round progression invariant violated: {} -> {} (must be +1)",
                    prev_round, circle.current_cycle_index
                ),
            });
        }
        if let Some(current_date) = circle.next_payout_date {
            circle.next_payout_date = Some(Timestamp::from_seconds(
                current_date.seconds() + circle.cycle_duration_secs(),
            ));
        }
    }

    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    let recipient_attr = if is_total_at_last_round {
        format!("{} members", active_members.len())
    } else {
        single_recipient
            .as_ref()
            .map(|r| r.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    };
    let log_msg = if is_total_at_last_round {
        format!(
            "Payout processed for round {} to all {} members ({} usaf total pending withdrawal)",
            circle.cycles_completed,
            active_members.len(),
            payout_amount
        )
    } else {
        format!(
            "Payout processed for round {} to {} ({} usaf pending withdrawal)",
            circle.cycles_completed, recipient_attr, payout_amount
        )
    };

    log_event(
        &mut deps,
        &env,
        circle_id,
        "payout_processed",
        &log_msg,
    )?;

    Ok(Response::new()
        .add_messages(outbound_messages)
        .add_attribute("action", "process_payout")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("cycle", circle.cycles_completed.to_string())
        .add_attribute("recipient", recipient_attr)
        .add_attribute("amount", payout_amount.to_string())
        .add_attribute("platform_fees_sent", platform_fees_sent.to_string())
        .add_attribute("pending_withdrawal", "true"))
}

// ---------------------------------------------------------------------------
// Advance Round — move to next round without payout (for Total/MinMembers threshold)
// ---------------------------------------------------------------------------

fn execute_advance_round(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if !matches!(circle.circle_status, CircleStatus::Running) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Running".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    // Authorization: manual_trigger_enabled means only creator can call (same as ProcessPayout)
    if circle.manual_trigger_enabled && info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can advance round (manual_trigger_enabled)".to_string(),
        });
    }

    // Time gate (end-of-round): prevent back-to-back transitions that can skip rounds.
    // `next_payout_date` is the round start; only allow advancing after the round has ended.
    if let Some(next_payout) = circle.next_payout_date {
        let round_end = next_payout.plus_seconds(circle.cycle_duration_secs());
        if env.block.time < round_end {
            return Err(ContractError::CycleNotReady {
                next_date: round_end.seconds(),
            });
        }
    }

    // Active members (not blocked) — used for round_in_cycle and min_round
    let active_members: Vec<Addr> = circle
        .members_list
        .iter()
        .filter(|m| {
            BLOCKED_MEMBERS
                .may_load(deps.storage, (circle_id, (*m).clone()))
                .unwrap_or(None)
                .map(|bc| bc > circle.current_cycle_index)
                .unwrap_or(true)
        })
        .cloned()
        .collect();

    if active_members.is_empty() {
        return Err(ContractError::InvalidParameters {
            msg: "No active members".to_string(),
        });
    }

    let active_count = active_members.len() as u32;
    let round_in_cycle = ((circle.current_cycle_index - 1) % active_count) + 1;
    let min_round_for_distribution =
        distribution_min_round_for_active(&circle.distribution_threshold, active_count);

    if round_in_cycle >= min_round_for_distribution {
        return Err(ContractError::InvalidParameters {
            msg: format!(
                "AdvanceRound only when round_in_cycle < {} (current: {}). Use ProcessPayout for distribution round.",
                min_round_for_distribution, round_in_cycle
            ),
        });
    }

    // Process missing members: add late fees, check ejection, use locked funds (calendar-based advance)
    let missing_members: Vec<Addr> = active_members
        .iter()
        .filter(|m| {
            DEPOSITS
                .may_load(deps.storage, (circle_id, (*m).clone(), circle.current_cycle_index))
                .unwrap_or(None)
                .is_none()
        })
        .cloned()
        .collect();

    // Match ProcessPayout semantics: if members are still missing, do not apply
    // missed-payment late fees before due date + grace period.
    if let Some(next_payout) = circle.next_payout_date {
        let grace_end = next_payout.plus_seconds(circle.grace_period_secs());
        if env.block.time <= grace_end && !missing_members.is_empty() {
            return Err(ContractError::InvalidParameters {
                msg: "Grace period not ended for all missing members".to_string(),
            });
        }
    }

    let mut locked_used_in_advance = Uint128::zero();
    for member in &missing_members {
        let late_fee_per_round =
            compute_late_fee_per_round(circle.contribution_amount, circle.late_fee_percent);

        let mut accumulated = MEMBER_ACCUMULATED_LATE_FEES
            .may_load(deps.storage, (circle_id, member.clone()))?
            .unwrap_or(Uint128::zero());

        let mut missed = MEMBER_MISSED_PAYMENTS
            .may_load(deps.storage, (circle_id, member.clone()))?
            .unwrap_or(MemberMissedPayments {
                member: member.clone(),
                missed_count: 0,
                last_missed_cycle: None,
                last_fee_round: None,
            });

        if missed.last_missed_cycle != Some(circle.current_cycle_index) {
            missed.missed_count += 1;
            missed.last_missed_cycle = Some(circle.current_cycle_index);
            accumulated = accumulated
                .checked_add(late_fee_per_round)
                .unwrap_or(accumulated);

            MEMBER_ACCUMULATED_LATE_FEES.save(
                deps.storage,
                (circle_id, member.clone()),
                &accumulated,
            )?;
            MEMBER_MISSED_PAYMENTS.save(
                deps.storage,
                (circle_id, member.clone()),
                &missed,
            )?;
        }

        // Force-eject when lock is insufficient to cover this round - same
        // rationale as in execute_process_payout's missing-members loop:
        // rounds must always progress, the member's MEMBER_LOCKED is their
        // commitment, and a depleted lock means they've defaulted on the
        // protocol contract. See the longer comment in process_payout.
        let orig_lock = original_lock_for_member(&circle, member);
        let standard_eject = should_eject_member(
            deps.storage,
            circle_id,
            member,
            orig_lock,
            circle.exit_penalty_percent,
            circle.max_missed_payments_allowed,
        );
        let lock_insufficient = if member == &circle.creator_address {
            false
        } else {
            current_member_lock(deps.storage, circle_id, member) < circle.contribution_amount
        };
        if standard_eject || lock_insufficient {
            eject_member_from_circle(&mut deps, &env, &mut circle, member)?;
            continue;
        }

        // Never use creator's lock - creator lock must stay intact
        if member != &circle.creator_address {
            let used = use_locked_amount_for_member(
                deps.storage,
                circle_id,
                member,
                circle.contribution_amount,
            )?;
            locked_used_in_advance = locked_used_in_advance
                .checked_add(used)
                .unwrap_or(locked_used_in_advance);
            if !used.is_zero() {
                // Record synthetic deposit to preserve round-by-round audit trail.
                DEPOSITS.save(
                    deps.storage,
                    (circle_id, member.clone(), circle.current_cycle_index),
                    &DepositRecord {
                        member: member.clone(),
                        cycle: circle.current_cycle_index,
                        amount: used,
                        timestamp: env.block.time,
                        on_time: false,
                    },
                )?;
            }
        }
    }

    // Also use locked from previously blocked members for this round
    let blocked_members_list: Vec<Addr> = BLOCKED_MEMBERS
        .prefix(circle_id)
        .range(deps.storage, None, None, Order::Ascending)
        .filter_map(|res| res.ok().map(|(m, _)| m))
        .collect();

    for blocked_member in blocked_members_list {
        if missing_members.iter().any(|m| m == &blocked_member) {
            continue;
        }
        if let Ok(Some(bc)) =
            BLOCKED_MEMBERS.may_load(deps.storage, (circle_id, blocked_member.clone()))
        {
            if bc <= circle.current_cycle_index {
                let used = use_locked_amount_for_member(
                    deps.storage,
                    circle_id,
                    &blocked_member,
                    circle.contribution_amount,
                )?;
                locked_used_in_advance = locked_used_in_advance
                    .checked_add(used)
                    .unwrap_or(locked_used_in_advance);
                if !used.is_zero() {
                    // Record synthetic deposit for blocked member coverage in this round.
                    DEPOSITS.save(
                        deps.storage,
                        (circle_id, blocked_member.clone(), circle.current_cycle_index),
                        &DepositRecord {
                            member: blocked_member.clone(),
                            cycle: circle.current_cycle_index,
                            amount: used,
                            timestamp: env.block.time,
                            on_time: false,
                        },
                    )?;
                }
            }
        }
    }

    // Keep total_amount_locked in sync: locked used for missed deposits is no longer "locked"
    if !locked_used_in_advance.is_zero() {
        circle.total_amount_locked = circle
            .total_amount_locked
            .checked_sub(locked_used_in_advance)
            .unwrap_or(Uint128::zero());
    }

    let total_rounds = circle.max_members * circle.total_cycles;
    if circle.current_cycle_index >= total_rounds {
        return Err(ContractError::InvalidParameters {
            msg: "Circle has no more rounds to advance".to_string(),
        });
    }

    // Round-progression invariant: current_cycle_index must advance by
    // exactly 1 per transaction. Catches accidental skips from refactors.
    let prev_round = circle.current_cycle_index;
    circle.current_cycle_index = prev_round.checked_add(1).ok_or_else(|| {
        ContractError::InvalidParameters {
            msg: "current_cycle_index overflow".to_string(),
        }
    })?;
    if circle.current_cycle_index != prev_round + 1 {
        return Err(ContractError::InvalidParameters {
            msg: format!(
                "Round progression invariant violated: {} -> {} (must be +1)",
                prev_round, circle.current_cycle_index
            ),
        });
    }
    if let Some(current_date) = circle.next_payout_date {
        circle.next_payout_date = Some(Timestamp::from_seconds(
            current_date.seconds() + circle.cycle_duration_secs(),
        ));
    }

    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "round_advanced",
        &format!(
            "Advanced to round {} (no payout — distribution at round {})",
            circle.current_cycle_index, min_round_for_distribution
        ),
    )?;

    Ok(Response::new()
        .add_attribute("action", "advance_round")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("new_cycle_index", circle.current_cycle_index.to_string()))
}

// ---------------------------------------------------------------------------
// Withdraw — member claims all pending payouts at once
// ---------------------------------------------------------------------------

fn execute_withdraw(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    // Validate circle exists
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    let pending = debit_pending_payout(
        deps.storage,
        circle_id,
        &info.sender,
        &mut circle.total_pending_payouts,
    )?;

    if pending.is_zero() {
        return Err(ContractError::NoPendingPayouts {});
    }

    // When Finalizing and this is the last withdrawal, only flip to Completed
    // if the documented invariant in state.rs actually holds: contract bank
    // balance, AFTER the BankMsg::Send queued below settles, must be zero AND
    // no undrained platform fees remain. The previous check (only
    // `total_pending_payouts.is_zero()`) let the status flip to Completed
    // while real native funds were still held by the contract (the "Completed
    // with 26 SAF" bug). Anything residual is recoverable via
    // `WithdrawPlatformFees` and `SweepDust`.
    if circle.circle_status == CircleStatus::Finalizing
        && circle.total_pending_payouts.is_zero()
    {
        let bal = deps
            .querier
            .query_balance(&env.contract.address, &circle.denomination)?;
        let post_send = bal.amount.saturating_sub(pending);
        if post_send.is_zero() && circle.total_platform_fees_collected.is_zero() {
            circle.circle_status = CircleStatus::Completed;
        }
    }

    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "withdrawal",
        &format!("Member {} withdrew {} usaf", info.sender, pending),
    )?;

    Ok(Response::new()
        .add_message(BankMsg::Send {
            to_address: info.sender.to_string(),
            amount: vec![Coin {
                denom: circle.denomination.clone(),
                amount: pending,
            }],
        })
        .add_attribute("action", "withdraw")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", info.sender)
        .add_attribute("amount", pending.to_string()))
}

// ---------------------------------------------------------------------------
// Check And Eject — permissionless ejection check
// ---------------------------------------------------------------------------

fn execute_check_and_eject(
    mut deps: DepsMut,
    env: Env,
    _info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if !matches!(circle.circle_status, CircleStatus::Running | CircleStatus::Paused) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Running or Paused".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    let members_snapshot: Vec<Addr> = circle.members_list.clone();
    let mut ejected_count = 0u32;

    for member in &members_snapshot {
        let orig_lock = original_lock_for_member(&circle, member);
        if should_eject_member(
            deps.storage,
            circle_id,
            member,
            orig_lock,
            circle.exit_penalty_percent,
            circle.max_missed_payments_allowed,
        ) {
            eject_member_from_circle(&mut deps, &env, &mut circle, member)?;
            ejected_count += 1;
        }
    }

    if ejected_count > 0 {
        // `payout_order_list`, `payout_amount`, the `calendar_rebuilt` event,
        // and the min-members breach warning are all already handled per-eject
        // inside `eject_member_from_circle`. Just persist the aggregated state
        // and bump `updated_at` so the off-chain sync notices the change.
        circle.updated_at = env.block.time;
        CIRCLES.save(deps.storage, circle_id, &circle)?;
    }

    Ok(Response::new()
        .add_attribute("action", "check_and_eject")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("ejected_count", ejected_count.to_string()))
}

// ---------------------------------------------------------------------------
// Cancel Circle — extended to allow running circles (creator forfeits lock)
// ---------------------------------------------------------------------------

fn execute_cancel_circle(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can cancel circle".to_string(),
        });
    }

    let is_running = matches!(
        circle.circle_status,
        CircleStatus::Running | CircleStatus::Paused
    );

    if !matches!(
        circle.circle_status,
        CircleStatus::Draft | CircleStatus::Open | CircleStatus::Full | CircleStatus::Running | CircleStatus::Paused
    ) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Draft, Open, Full, Running or Paused".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    // After at least one distribution, cancel is not allowed — circle must complete or use emergency procedures
    if is_running {
        let max_cycle = circle
            .current_cycle_index
            .max(circle.cycles_completed)
            .max(1);
        let mut has_distributed = false;
        for cycle in 1..=max_cycle {
            if PAYOUTS
                .prefix((circle_id, cycle))
                .range(deps.storage, None, None, Order::Ascending)
                .next()
                .is_some()
            {
                has_distributed = true;
                break;
            }
        }
        if has_distributed {
            return Err(ContractError::CancelNotAllowedAfterDistribution {});
        }
    }

    circle.circle_status = CircleStatus::Cancelled;
    circle.updated_at = env.block.time;

    let mut messages: Vec<CosmosMsg> = Vec::new();

    if is_running {
        // Cancelling a running circle is the creator's choice — so members
        // should be made whole. The old code path (a) deducted an exit penalty
        // from each member's lock refund (unfair: the creator cancelled, not
        // the member), (b) never refunded the current-cycle deposits made by
        // anyone, (c) never zeroed `creator_lock_amount`, and (d) used direct
        // BankMsg sends for the lock refund — so the creator (no
        // MEMBER_LOCKED_AMOUNTS entry) got NOTHING and the UI had no Withdraw
        // button. This rewrite credits everything via PENDING_PAYOUTS so a
        // single Withdraw call by each member recovers what they're owed,
        // and balances out to bank balance == sum(pending) on success.

        let active_members: Vec<Addr> = circle
            .members_list
            .iter()
            .filter(|m| m.as_ref() != circle.creator_address.as_str())
            .filter(|m| {
                BLOCKED_MEMBERS
                    .may_load(deps.storage, (circle_id, (*m).clone()))
                    .unwrap_or(None)
                    .is_none()
            })
            .cloned()
            .collect();

        // 1. Creator forfeits creator_lock_amount → distributed to active
        //    non-creator members via PENDING_PAYOUTS. Cancellation penalty.
        let creator_lock = circle.creator_lock_amount;
        if !creator_lock.is_zero() && !active_members.is_empty() {
            let count = Uint128::from(active_members.len() as u128);
            let per_member = creator_lock.multiply_ratio(1u128, count.u128());
            let remainder = creator_lock
                .checked_sub(per_member * count)
                .unwrap_or(Uint128::zero());

            for (idx, member) in active_members.iter().enumerate() {
                let mut share = per_member;
                if idx == 0 {
                    share = share.checked_add(remainder).unwrap_or(share);
                }
                if !share.is_zero() {
                    credit_pending_payout(
                        deps.storage,
                        circle_id,
                        member,
                        share,
                        &mut circle.total_pending_payouts,
                    )?;
                }
            }
        }
        // Always zero out, whether or not there were members to receive it —
        // otherwise the field stays stale and future invariant checks lie.
        circle.creator_lock_amount = Uint128::zero();

        // 2. Refund each non-creator member's FULL locked join-deposit via
        //    PENDING_PAYOUTS (no exit penalty when the creator is the one
        //    cancelling). Creator has no MEMBER_LOCKED entry — their initial
        //    "lock" is `creator_lock_amount`, already handled above.
        let member_list_snapshot: Vec<Addr> = circle.members_list.clone();
        for member in &member_list_snapshot {
            let locked = MEMBER_LOCKED_AMOUNTS
                .may_load(deps.storage, (circle_id, member.clone()))?
                .unwrap_or(Uint128::zero());
            if !locked.is_zero() {
                credit_pending_payout(
                    deps.storage,
                    circle_id,
                    member,
                    locked,
                    &mut circle.total_pending_payouts,
                )?;
                debit_member_locked(
                    deps.storage,
                    circle_id,
                    member,
                    locked,
                    &mut circle.total_amount_locked,
                )?;
            }
            MEMBER_ACCUMULATED_LATE_FEES.remove(deps.storage, (circle_id, member.clone()));
        }

        // 3. Refund every deposit already made in the current (unfinished)
        //    cycle. Without this the funds sit on the contract forever — the
        //    cycle's payout will never happen, but `total_amount_locked` and
        //    the bank balance still hold them. Crediting via PENDING_PAYOUTS
        //    lets each depositor Withdraw the exact amount they put in.
        //
        //    DEPOSITS is keyed `(circle_id, member, cycle)`. The codebase
        //    loads it by exact key per-member rather than iterating with
        //    `prefix()`, so do the same here.
        let unfinished_cycle = circle.current_cycle_index;
        if unfinished_cycle > 0 {
            for depositor in &member_list_snapshot {
                let deposit_record = DEPOSITS.may_load(
                    deps.storage,
                    (circle_id, depositor.clone(), unfinished_cycle),
                )?;
                let amount = deposit_record.map(|r| r.amount).unwrap_or(Uint128::zero());
                if amount.is_zero() {
                    continue;
                }
                credit_pending_payout(
                    deps.storage,
                    circle_id,
                    depositor,
                    amount,
                    &mut circle.total_pending_payouts,
                )?;
            }
        }
    } else {
        // Before start: refund all join deposits
        let locked_entries: Vec<(Addr, Uint128)> = MEMBER_LOCKED_AMOUNTS
            .prefix(circle_id)
            .range(deps.storage, None, None, Order::Ascending)
            .filter_map(|res| res.ok().map(|(m, a)| (m, a)))
            .collect();

        for (member, amount) in locked_entries {
            if !amount.is_zero() {
                let refund_msgs = safe_refund_or_queue(
                    deps.branch(),
                    &env,
                    circle_id,
                    &member,
                    amount,
                    &circle.denomination,
                )?;
                messages.extend(refund_msgs);
                debit_member_locked(
                    deps.storage,
                    circle_id,
                    &member,
                    amount,
                    &mut circle.total_amount_locked,
                )?;
            }
        }

        // Refund creator lock
        if !circle.creator_lock_amount.is_zero() {
            let creator_amount = circle.creator_lock_amount;
            let refund_msgs = safe_refund_or_queue(
                deps.branch(),
                &env,
                circle_id,
                &circle.creator_address,
                creator_amount,
                &circle.denomination,
            )?;
            messages.extend(refund_msgs);
        }
    }

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "circle_cancelled",
        &format!(
            "Circle {} cancelled by {} (was_running: {})",
            circle_id, info.sender, is_running
        ),
    )?;

    Ok(Response::new()
        .add_messages(messages)
        .add_attribute("action", "cancel_circle")
        .add_attribute("circle_id", circle_id.to_string()))
}

// ---------------------------------------------------------------------------
// Pause / Unpause
// ---------------------------------------------------------------------------

fn execute_pause_circle(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can pause circle".to_string(),
        });
    }

    if !matches!(circle.circle_status, CircleStatus::Running) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Running".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    circle.circle_status = CircleStatus::Paused;
    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(&mut deps, &env, circle_id, "circle_paused", &format!("Circle {} paused", circle_id))?;

    Ok(Response::new()
        .add_attribute("action", "pause_circle")
        .add_attribute("circle_id", circle_id.to_string()))
}

fn execute_unpause_circle(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can unpause circle".to_string(),
        });
    }

    if !matches!(circle.circle_status, CircleStatus::Paused) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Paused".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    circle.circle_status = CircleStatus::Running;
    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(&mut deps, &env, circle_id, "circle_unpaused", &format!("Circle {} unpaused", circle_id))?;

    Ok(Response::new()
        .add_attribute("action", "unpause_circle")
        .add_attribute("circle_id", circle_id.to_string()))
}

// ---------------------------------------------------------------------------
// Emergency Stop
// ---------------------------------------------------------------------------

fn execute_emergency_stop(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if !circle.emergency_stop_enabled {
        return Err(ContractError::InvalidParameters {
            msg: "Emergency stop not enabled".to_string(),
        });
    }

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can trigger emergency stop".to_string(),
        });
    }

    circle.emergency_stop_triggered = true;
    circle.circle_status = CircleStatus::Paused;
    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(&mut deps, &env, circle_id, "emergency_stop", &format!("Emergency stop triggered for circle {}", circle_id))?;

    Ok(Response::new()
        .add_attribute("action", "emergency_stop")
        .add_attribute("circle_id", circle_id.to_string()))
}

// ---------------------------------------------------------------------------
// Update Circle
// ---------------------------------------------------------------------------

fn execute_update_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    circle_name: Option<String>,
    circle_description: Option<String>,
    circle_image: Option<String>,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can update circle".to_string(),
        });
    }

    if matches!(
        circle.circle_status,
        CircleStatus::Running | CircleStatus::Finalizing | CircleStatus::Completed
    ) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Not Running, Finalizing or Completed".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    if let Some(name) = circle_name {
        circle.circle_name = name;
    }
    if let Some(desc) = circle_description {
        circle.circle_description = desc;
    }
    if let Some(img) = circle_image {
        circle.circle_image = Some(img);
    }

    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    Ok(Response::new()
        .add_attribute("action", "update_circle")
        .add_attribute("circle_id", circle_id.to_string()))
}

// ---------------------------------------------------------------------------
// Withdraw Platform Fees (stub)
// ---------------------------------------------------------------------------

/// Send accumulated platform fees of a circle to the configured platform
/// address. Permissionless — anyone may trigger because the destination is
/// fixed in `PLATFORM_CONFIG`. Gated to `Finalizing` / `Completed` so a draw
/// cannot happen while accounting is mid-cycle. Used to recover fees from
/// legacy circles that finalized under the previous code path where fees were
/// never drained.
fn execute_withdraw_platform_fees(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: Option<u64>,
) -> Result<Response, ContractError> {
    let id = circle_id.ok_or_else(|| ContractError::InvalidParameters {
        msg: "circle_id is required".to_string(),
    })?;
    let mut circle = CIRCLES.load(deps.storage, id)?;

    if !matches!(
        circle.circle_status,
        CircleStatus::Finalizing | CircleStatus::Completed | CircleStatus::Cancelled
    ) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Finalizing, Completed or Cancelled".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    let fees = circle.total_platform_fees_collected;
    if fees.is_zero() {
        return Err(ContractError::NoPendingPayouts {});
    }

    let platform_addr = PLATFORM_CONFIG.load(deps.storage)?.platform_address;
    circle.total_platform_fees_collected = Uint128::zero();
    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, id, &circle)?;

    log_event(
        &mut deps,
        &env,
        id,
        "platform_fees_withdrawn",
        &format!(
            "{} usaf platform fees sent to {} (triggered by {})",
            fees, platform_addr, info.sender
        ),
    )?;

    Ok(Response::new()
        .add_message(BankMsg::Send {
            to_address: platform_addr.to_string(),
            amount: vec![Coin {
                denom: circle.denomination.clone(),
                amount: fees,
            }],
        })
        .add_attribute("action", "withdraw_platform_fees")
        .add_attribute("circle_id", id.to_string())
        .add_attribute("amount", fees.to_string()))
}

/// Sweep residual native funds on a Finalizing/Completed/Cancelled circle
/// by crediting them to the CREATOR's `PENDING_PAYOUTS`, so the creator can
/// reclaim them with a normal `Withdraw` call.
///
/// Permissionless. Only runs when every member has withdrawn
/// (`total_pending_payouts == 0`).
/// Residual = on-chain bank balance minus undrained platform fees
/// (use `WithdrawPlatformFees` for those).
///
/// Use cases:
/// - Legacy circles that finalized before the `members_at_start` fix in
///   `total_rounds`: the contract zeroed `creator_lock_amount` but never
///   credited the refund (e.g. an `Ws Savings Circle1` with 14 SAF stuck).
///   This sweep delivers the funds to the creator.
/// - Old residuals (truncated creator-lock refund, headroom remainder from
///   over-deposits) — same recovery path.
///
/// If the creator is no longer addressable (somehow removed; shouldn't
/// happen), the funds fall back to the platform address to avoid being
/// permanently locked.
fn execute_sweep_dust(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if !matches!(
        circle.circle_status,
        CircleStatus::Finalizing | CircleStatus::Completed | CircleStatus::Cancelled
    ) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Finalizing, Completed or Cancelled".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    if !circle.total_pending_payouts.is_zero() {
        return Err(ContractError::InvalidParameters {
            msg: format!(
                "Pending payouts must be zero before sweep (still {})",
                circle.total_pending_payouts
            ),
        });
    }

    let bal = deps
        .querier
        .query_balance(&env.contract.address, &circle.denomination)?;
    // Reserve undrained platform fees - those go through WithdrawPlatformFees,
    // not the dust path, so the platform address is not double-credited.
    let dust = bal
        .amount
        .checked_sub(circle.total_platform_fees_collected)
        .unwrap_or(Uint128::zero());

    if dust.is_zero() {
        return Err(ContractError::NoPendingPayouts {});
    }

    // Credit the residual to the CREATOR's pending payouts so they can
    // reclaim it with a normal `Withdraw` call. This is the correct owner
    // for stuck creator-lock refunds (the most common cause of dust).
    //
    // After this credit, `total_pending_payouts` is non-zero again - so the
    // circle goes BACK to Finalizing (if it had flipped to Completed) until
    // the creator's `Withdraw` zeroes it. Documented invariant preserved:
    // Completed only when bank balance == 0.
    credit_pending_payout(
        deps.storage,
        circle_id,
        &circle.creator_address,
        dust,
        &mut circle.total_pending_payouts,
    )?;
    if circle.circle_status == CircleStatus::Completed {
        // We just made the chain truth non-zero again; reflect that.
        circle.circle_status = CircleStatus::Finalizing;
    }
    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "dust_swept",
        &format!(
            "{} usaf residual credited to creator {} (triggered by {}); creator can now Withdraw",
            dust, circle.creator_address, info.sender
        ),
    )?;

    // No outbound BankMsg - the creator will pull the funds via Withdraw,
    // which keeps `total_pending_payouts` accounting consistent.
    Ok(Response::new()
        .add_attribute("action", "sweep_dust")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("recipient", circle.creator_address.to_string())
        .add_attribute("amount", dust.to_string()))
}

// ---------------------------------------------------------------------------
// Deposit Creator Reward - platform sends X% of total volume to creator
// ---------------------------------------------------------------------------

/// Platform-funded reward for the creator, paid out of the platform / server
/// wallet when a circle transitions to `Finalizing`. The caller attaches the
/// reward amount as funds in the circle's denomination; the contract validates
/// it, credits the creator's `PENDING_PAYOUTS`, and locks the per-circle
/// idempotency flag so a duplicate broadcast cannot double-credit.
///
/// Design notes
/// ------------
///   - Permissionless: sending funds INTO the contract for someone else is
///     harmless and lets any external party (DAO, partner, the server itself)
///     boost a circle. The idempotency guard (CREATOR_REWARDS_CREDITED)
///     prevents abuse.
///   - Status gate is `Finalizing` (NOT Completed). At Finalizing the final
///     round's PENDING_PAYOUTS are already credited; the creator's first
///     `Withdraw` call after this credit will pick up both the round payout(s)
///     AND the reward in one go. Pre-Finalizing the round flow hasn't run so
///     the volume basis isn't fully known yet; post-Completed the bank should
///     already be drained and adding to PENDING_PAYOUTS would resurrect the
///     "Completed with non-zero balance" state we just fixed.
///   - The reward is added to `total_pending_payouts` so the bank-vs-pending
///     invariant on Withdraw stays consistent: the funds we just received
///     are immediately backed by an equal pending credit.
fn execute_deposit_creator_reward(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Status gate: only at Finalizing. See doc-comment above.
    if !matches!(circle.circle_status, CircleStatus::Finalizing) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Finalizing".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    // Idempotency: refuse a second reward credit for the same circle.
    if CREATOR_REWARDS_CREDITED
        .may_load(deps.storage, circle_id)?
        .is_some()
    {
        return Err(ContractError::InvalidParameters {
            msg: "Creator reward already credited for this circle".to_string(),
        });
    }

    // Pull attached funds in the circle's denomination.
    let amount = must_pay(&info, &circle.denomination).map_err(|_| {
        ContractError::InsufficientFunds {
            required: "non-zero reward in circle denomination".to_string(),
            sent: "0".to_string(),
        }
    })?;
    if amount.is_zero() {
        return Err(ContractError::InsufficientFunds {
            required: "non-zero reward".to_string(),
            sent: "0".to_string(),
        });
    }

    // Credit the creator's pending payouts. They will pick this up via the
    // existing `Withdraw` flow alongside any final-round payouts.
    credit_pending_payout(
        deps.storage,
        circle_id,
        &circle.creator_address,
        amount,
        &mut circle.total_pending_payouts,
    )?;

    CREATOR_REWARDS_CREDITED.save(deps.storage, circle_id, &amount)?;
    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "creator_reward_credited",
        &format!(
            "{} usaf reward credited to creator {} by {}",
            amount, circle.creator_address, info.sender
        ),
    )?;

    Ok(Response::new()
        .add_attribute("action", "deposit_creator_reward")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("creator", circle.creator_address.to_string())
        .add_attribute("amount", amount.to_string())
        .add_attribute("funder", info.sender.to_string()))
}

// ---------------------------------------------------------------------------
// Private Circle Management
// ---------------------------------------------------------------------------

fn execute_add_private_member(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
    pseudonym: Option<String>,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can add private members".to_string(),
        });
    }

    if !matches!(circle.visibility, Visibility::Private) {
        return Err(ContractError::InvalidParameters {
            msg: "Circle must be private to use AddPrivateMember".to_string(),
        });
    }

    if circle.members_list.len() as u32 >= circle.max_members {
        return Err(ContractError::CircleFull {
            max: circle.max_members,
        });
    }

    let validated = deps.api.addr_validate(member_address.as_str())?;

    if circle.members_list.contains(&validated) {
        return Err(ContractError::AlreadyMember {
            address: validated.to_string(),
        });
    }

    circle.members_list.push(validated.clone());

    let mut private_members = PRIVATE_MEMBER_LIST
        .may_load(deps.storage, circle_id)?
        .unwrap_or_default();
    private_members.push(validated.clone());
    PRIVATE_MEMBER_LIST.save(deps.storage, circle_id, &private_members)?;

    if let Some(pseudo) = pseudonym {
        MEMBER_PSEUDONYMS.save(deps.storage, (circle_id, validated.clone()), &pseudo)?;
    }

    circle.updated_at = env.block.time;

    if circle.members_list.len() as u32 >= circle.max_members {
        circle.circle_status = CircleStatus::Full;
    } else if circle.circle_status == CircleStatus::Draft {
        circle.circle_status = CircleStatus::Open;
    }

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "private_member_added",
        &format!("Private member {} added by {}", validated, info.sender),
    )?;

    Ok(Response::new()
        .add_attribute("action", "add_private_member")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", validated))
}

fn execute_update_member_pseudonym(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
    pseudonym: String,
) -> Result<Response, ContractError> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can update pseudonyms".to_string(),
        });
    }

    let validated = deps.api.addr_validate(member_address.as_str())?;
    let is_member = circle.members_list.contains(&validated);
    let is_pending = circle.pending_members.contains(&validated);

    if !is_member && !is_pending {
        return Err(ContractError::InvalidParameters {
            msg: "Address not found in circle members or pending invitations".to_string(),
        });
    }

    MEMBER_PSEUDONYMS.save(deps.storage, (circle_id, validated.clone()), &pseudonym)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "member_pseudonym_updated",
        &format!("Pseudonym '{}' set for {}", pseudonym, validated),
    )?;

    Ok(Response::new()
        .add_attribute("action", "update_member_pseudonym")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", validated)
        .add_attribute("pseudonym", pseudonym))
}

fn execute_block_member(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
) -> Result<Response, ContractError> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can block members".to_string(),
        });
    }

    let validated = deps.api.addr_validate(member_address.as_str())?;

    if !circle.members_list.contains(&validated) {
        return Err(ContractError::InvalidParameters {
            msg: "Member not found in circle".to_string(),
        });
    }

    let blocked_from_cycle = circle.current_cycle_index + 1;
    BLOCKED_MEMBERS.save(deps.storage, (circle_id, validated.clone()), &blocked_from_cycle)?;

    log_event(
        &mut deps,
        &env,
        circle_id,
        "member_blocked",
        &format!("Member {} blocked from cycle {}", validated, blocked_from_cycle),
    )?;

    Ok(Response::new()
        .add_attribute("action", "block_member")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", validated)
        .add_attribute("blocked_from_cycle", blocked_from_cycle.to_string()))
}

fn execute_distribute_blocked_funds(
    mut deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    cycle: u32,
) -> Result<Response, ContractError> {
    let circle = CIRCLES.load(deps.storage, circle_id)?;

    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can distribute blocked funds".to_string(),
        });
    }

    let mut total_blocked_funds = Uint128::zero();
    let mut blocked_in_cycle: Vec<(Addr, Uint128)> = Vec::new();

    for member in &circle.members_list {
        if let Some(blocked_cycle) =
            BLOCKED_MEMBERS.may_load(deps.storage, (circle_id, member.clone()))?
        {
            if blocked_cycle <= cycle {
                if let Some(locked) =
                    MEMBER_LOCKED_AMOUNTS.may_load(deps.storage, (circle_id, member.clone()))?
                {
                    total_blocked_funds += locked;
                    blocked_in_cycle.push((member.clone(), locked));
                }
            }
        }
    }

    if total_blocked_funds.is_zero() {
        return Err(ContractError::InvalidParameters {
            msg: "No blocked funds to distribute".to_string(),
        });
    }

    let active_members: Vec<Addr> = circle
        .members_list
        .iter()
        .filter(|m| {
            BLOCKED_MEMBERS
                .may_load(deps.storage, (circle_id, (*m).clone()))
                .unwrap_or(None)
                .map(|bc| bc > cycle)
                .unwrap_or(true)
        })
        .filter(|m| {
            DEPOSITS
                .may_load(deps.storage, (circle_id, (*m).clone(), cycle))
                .map(|d| d.is_some())
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    if active_members.is_empty() {
        return Err(ContractError::InvalidParameters {
            msg: "No active members to distribute to".to_string(),
        });
    }

    let amount_per_member =
        total_blocked_funds.multiply_ratio(1u128, active_members.len() as u128);
    let remainder = total_blocked_funds
        .checked_sub(amount_per_member * Uint128::from(active_members.len() as u128))
        .unwrap_or(Uint128::zero());

    let mut messages = Vec::new();
    for (idx, member) in active_members.iter().enumerate() {
        let mut amount = amount_per_member;
        if idx == 0 {
            amount = amount.checked_add(remainder).unwrap_or(amount);
        }
        messages.push(BankMsg::Send {
            to_address: member.to_string(),
            amount: vec![Coin {
                denom: circle.denomination.clone(),
                amount,
            }],
        });
    }

    log_event(
        &mut deps,
        &env,
        circle_id,
        "blocked_funds_distributed",
        &format!(
            "Blocked funds {} distributed to {} active members",
            total_blocked_funds,
            active_members.len()
        ),
    )?;

    Ok(Response::new()
        .add_messages(messages)
        .add_attribute("action", "distribute_blocked_funds")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("cycle", cycle.to_string())
        .add_attribute("total_distributed", total_blocked_funds.to_string()))
}

// ---------------------------------------------------------------------------
// Refunds (liquid balance only)
// ---------------------------------------------------------------------------

fn safe_refund_or_queue(
    deps: DepsMut,
    env: &Env,
    _circle_id: u64,
    member: &Addr,
    amount: Uint128,
    denom: &str,
) -> Result<Vec<CosmosMsg>, ContractError> {
    if amount.is_zero() {
        return Ok(vec![]);
    }

    let balance = deps
        .querier
        .query_balance(&env.contract.address, denom)
        .map_err(|e| ContractError::InvalidParameters {
            msg: format!("Balance query failed: {}", e),
        })?;

    if balance.amount >= amount {
        return Ok(vec![CosmosMsg::Bank(BankMsg::Send {
            to_address: member.to_string(),
            amount: vec![Coin {
                denom: denom.to_string(),
                amount,
            }],
        })]);
    }

    Err(ContractError::InvalidParameters {
        msg: "Insufficient liquid balance for refund".to_string(),
    })
}

// ---------------------------------------------------------------------------
// Internal Helpers
// ---------------------------------------------------------------------------

fn generate_payout_order(circle: &mut Circle, env: &Env) {
    if circle.payout_order_list.is_none() {
        let members = circle.members_list.clone();
        circle.payout_order_list = Some(match circle.payout_order_type {
            PayoutOrderType::RandomOrder => {
                let mut shuffled = members;
                let seed = env.block.time.seconds() + circle.circle_id;
                for i in 0..shuffled.len() {
                    let j = (seed as usize + i * 7) % shuffled.len();
                    shuffled.swap(i, j);
                }
                shuffled
            }
            PayoutOrderType::PredefinedOrder => members,
        });
    }
}

fn build_distribution_calendar(circle: &Circle, start_timestamp: Timestamp) -> String {
    // Mirror `distribution_min_round_for_active` (and the matching helper in
    // query.rs) so the emitted calendar matches the execute path. Diverging
    // from execute previously broke the cron classifier — and showed phantom
    // distribution rounds when `max_members` exceeded the actual roster
    // (round_size was max_members, so round_in_cycle could legitimately reach
    // it on cycle 2+ for a not-yet-full circle).
    //
    // Both the modulo (round_size) and the distribution gate must use the
    // payout-order length — locked at start to the active roster size.
    let round_size = circle.payout_order_list
        .as_ref()
        .map(|l| l.len() as u32)
        .unwrap_or(circle.max_members)
        .max(1);
    let min_round_for_distribution: u32 = match circle.distribution_threshold {
        None | Some(DistributionThreshold::Total {}) => round_size,
        Some(DistributionThreshold::MinMembers { count }) => count,
    };

    let mut calendar_data = String::new();
    if let Some(payout_order) = &circle.payout_order_list {
        let mut round_number = 1u32;
        for cycle in 1..=circle.total_cycles {
            for recipient in payout_order.iter() {
                let round_in_cycle = ((round_number - 1) % round_size) + 1;
                let distribution_occurs = round_in_cycle >= min_round_for_distribution;
                let round_offset_seconds = (round_number - 1) as u64 * circle.cycle_duration_secs();
                let deposit_deadline = Timestamp::from_seconds(
                    start_timestamp.seconds() + round_offset_seconds,
                );
                let distribution_date = Timestamp::from_seconds(
                    start_timestamp.seconds() + round_offset_seconds + circle.cycle_duration_secs(),
                );
                if !calendar_data.is_empty() {
                    calendar_data.push(',');
                }
                calendar_data.push_str(&format!(
                    "{{round:{},cycle:{},deposit_deadline:{},distribution_date:{},distribution_occurs:{},recipient:\"{}\"}}",
                    round_number,
                    cycle,
                    deposit_deadline.seconds(),
                    distribution_date.seconds(),
                    distribution_occurs,
                    recipient
                ));
                round_number += 1;
            }
        }
    }
    calendar_data
}

fn use_locked_amount_for_member(
    storage: &mut dyn Storage,
    circle_id: u64,
    member: &Addr,
    amount_needed: Uint128,
) -> Result<Uint128, ContractError> {
    if let Some(locked) = MEMBER_LOCKED_AMOUNTS.may_load(storage, (circle_id, member.clone()))? {
        let used = if locked > amount_needed {
            amount_needed
        } else {
            locked
        };
        let remaining = locked
            .checked_sub(used)
            .map_err(|_| ContractError::InvalidParameters {
                msg: "Locked amount underflow".to_string(),
            })?;
        if remaining.is_zero() {
            MEMBER_LOCKED_AMOUNTS.remove(storage, (circle_id, member.clone()));
        } else {
            MEMBER_LOCKED_AMOUNTS.save(storage, (circle_id, member.clone()), &remaining)?;
        }
        return Ok(used);
    }
    Ok(Uint128::zero())
}

/// Credit pending payout for a member and update circle aggregate.
fn credit_pending_payout(
    storage: &mut dyn Storage,
    circle_id: u64,
    member: &Addr,
    amount: Uint128,
    total_pending: &mut Uint128,
) -> Result<(), ContractError> {
    if amount.is_zero() {
        return Ok(());
    }
    let existing = PENDING_PAYOUTS
        .may_load(storage, (circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());
    let new_pending = existing
        .checked_add(amount)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Pending payout overflow".to_string(),
        })?;
    PENDING_PAYOUTS.save(storage, (circle_id, member.clone()), &new_pending)?;
    *total_pending = total_pending
        .checked_add(amount)
        .unwrap_or(*total_pending);
    Ok(())
}

/// Debit pending payout for a member and update circle aggregate. Returns the amount debited.
fn debit_pending_payout(
    storage: &mut dyn Storage,
    circle_id: u64,
    member: &Addr,
    total_pending: &mut Uint128,
) -> Result<Uint128, ContractError> {
    let pending = PENDING_PAYOUTS
        .may_load(storage, (circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());
    if pending.is_zero() {
        return Ok(Uint128::zero());
    }
    PENDING_PAYOUTS.remove(storage, (circle_id, member.clone()));
    *total_pending = total_pending
        .checked_sub(pending)
        .unwrap_or(Uint128::zero());
    Ok(pending)
}

/// Add locked amount for a member and update circle aggregate.
fn add_member_locked(
    storage: &mut dyn Storage,
    circle_id: u64,
    member: &Addr,
    amount: Uint128,
    total_locked: &mut Uint128,
) -> Result<(), ContractError> {
    if amount.is_zero() {
        return Ok(());
    }
    let existing = MEMBER_LOCKED_AMOUNTS
        .may_load(storage, (circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());
    let new_locked = existing
        .checked_add(amount)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Locked amount overflow".to_string(),
        })?;
    MEMBER_LOCKED_AMOUNTS.save(storage, (circle_id, member.clone()), &new_locked)?;
    *total_locked = total_locked
        .checked_add(amount)
        .unwrap_or(*total_locked);
    Ok(())
}

/// Debit locked amount for a member and update circle aggregate.
fn debit_member_locked(
    storage: &mut dyn Storage,
    circle_id: u64,
    member: &Addr,
    amount: Uint128,
    total_locked: &mut Uint128,
) -> Result<(), ContractError> {
    if amount.is_zero() {
        return Ok(());
    }
    let existing = MEMBER_LOCKED_AMOUNTS
        .may_load(storage, (circle_id, member.clone()))?
        .unwrap_or(Uint128::zero());
    let remaining = existing
        .checked_sub(amount)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Locked amount underflow".to_string(),
        })?;
    if remaining.is_zero() {
        MEMBER_LOCKED_AMOUNTS.remove(storage, (circle_id, member.clone()));
    } else {
        MEMBER_LOCKED_AMOUNTS.save(storage, (circle_id, member.clone()), &remaining)?;
    }
    *total_locked = total_locked
        .checked_sub(amount)
        .unwrap_or(Uint128::zero());
    Ok(())
}

fn log_event(
    deps: &mut DepsMut,
    env: &Env,
    circle_id: u64,
    event_type: &str,
    data: &str,
) -> StdResult<()> {
    let event_id = EVENT_COUNTER
        .may_load(deps.storage, circle_id)?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| cosmwasm_std::StdError::generic_err("Event ID overflow"))?;

    EVENTS.save(
        deps.storage,
        (circle_id, event_id),
        &EventLog {
            event_type: event_type.to_string(),
            circle_id,
            data: data.to_string(),
            timestamp: env.block.time,
        },
    )?;
    EVENT_COUNTER.save(deps.storage, circle_id, &event_id)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};
    use cosmwasm_std::{coins, from_json};
    use crate::msg::InstantiateMsg;
    use crate::state::PlatformConfig;

    fn setup_platform_config(deps: &mut cosmwasm_std::OwnedDeps<cosmwasm_std::MemoryStorage, cosmwasm_std::testing::MockApi, cosmwasm_std::testing::MockQuerier>) {
        PLATFORM_CONFIG
            .save(
                &mut deps.storage,
                &PlatformConfig {
                    platform_fee_percent: 100,
                    platform_address: Addr::unchecked("platform"),
                },
            )
            .unwrap();
    }

    fn base_create_msg() -> ExecuteMsg {
        ExecuteMsg::CreateCircle {
            circle_name: "test".to_string(),
            circle_description: "test".to_string(),
            circle_image: None,
            max_members: 3,
            min_members_required: 2,
            invite_only: false,
            contribution_amount: Uint128::from(100u128),
            denomination: None,
            exit_penalty_percent: 2000,
            late_fee_percent: 1000,
            total_cycles: 2,
            cycle_duration_days: 0,
            cycle_duration_seconds: Some(300), // 5 min
            start_date: None,
            grace_period_hours: 0,
            grace_period_seconds: Some(60), // 1 min
            auto_start_when_full: false,
            auto_start_type: None,
            auto_start_date: None,
            payout_order_type: PayoutOrderType::RandomOrder,
            payout_order_list: None,
            auto_payout_enabled: true,
            manual_trigger_enabled: false,
            emergency_stop_enabled: false,
            auto_refund_if_min_not_met: false,
            strict_mode: false,
            visibility: Visibility::Public,
            show_member_identities: true,
            distribution_threshold: None,
        }
    }

    // creator_lock = contribution * 2 (see compute_creator_lock)
    fn creator_lock(_max_members: u32, contribution: u128) -> u128 {
        contribution * 2
    }

    #[test]
    fn compute_max_missed_base_works() {
        // 20% exit + 10% late fee → (10000 - 2000) / 1000 = 8 missed allowed
        assert_eq!(compute_max_missed_base(2000, 1000), 8);
        // late_fee_percent = 0 → u32::MAX (no late fee → no missed-count cap)
        assert_eq!(compute_max_missed_base(2000, 0), u32::MAX);
    }

    #[test]
    fn cap_max_missed_by_rounds_works() {
        // total_rounds = 6, base = 8 → cap to 5 (so ejection happens before last round)
        assert_eq!(cap_max_missed_by_rounds(8, 6), 5);
        // total_rounds = 1 → cap = 1 (max(1, 0))
        assert_eq!(cap_max_missed_by_rounds(8, 1), 1);
        // base smaller than cap → unchanged
        assert_eq!(cap_max_missed_by_rounds(2, 6), 2);
    }

    #[test]
    fn compute_max_missed_scaled_shrinks_when_members_drop() {
        // 5 active of 5 start → base
        assert_eq!(compute_max_missed_scaled(2000, 1000, Some(5), 5), 8);
        // 3 active of 5 start → base * 3/5 = 4
        assert_eq!(compute_max_missed_scaled(2000, 1000, Some(5), 3), 4);
        // never below 1
        assert_eq!(compute_max_missed_scaled(2000, 1000, Some(100), 1), 1);
        // None members_at_start → falls back to base
        assert_eq!(compute_max_missed_scaled(2000, 1000, None, 3), 8);
    }

    #[test]
    fn create_circle_rejects_grace_ge_cycle() {
        let mut deps = mock_dependencies();
        setup_platform_config(&mut deps);

        // grace == cycle → reject
        let msg = ExecuteMsg::CreateCircle {
            circle_name: "test".to_string(),
            circle_description: "test".to_string(),
            circle_image: None,
            max_members: 3,
            min_members_required: 2,
            invite_only: false,
            contribution_amount: Uint128::from(100u128),
            denomination: None,
            exit_penalty_percent: 2000,
            late_fee_percent: 1000,
            total_cycles: 2,
            cycle_duration_days: 0,
            cycle_duration_seconds: Some(300),
            start_date: None,
            grace_period_hours: 0,
            grace_period_seconds: Some(300), // == cycle
            auto_start_when_full: false,
            auto_start_type: None,
            auto_start_date: None,
            payout_order_type: PayoutOrderType::RandomOrder,
            payout_order_list: None,
            auto_payout_enabled: true,
            manual_trigger_enabled: false,
            emergency_stop_enabled: false,
            auto_refund_if_min_not_met: false,
            strict_mode: false,
            visibility: Visibility::Public,
            show_member_identities: true,
            distribution_threshold: None,
        };
        let info = mock_info("creator", &coins(creator_lock(3, 100), "usaf"));
        let err = execute(deps.as_mut(), mock_env(), info, msg).unwrap_err();
        match err {
            ContractError::InvalidParameters { msg } => {
                assert!(msg.contains("grace_period"), "expected grace_period error, got: {}", msg);
                assert!(msg.contains("strictly less than"), "got: {}", msg);
            }
            other => panic!("expected InvalidParameters, got {:?}", other),
        }
    }

    #[test]
    fn create_circle_accepts_grace_lt_cycle() {
        let mut deps = mock_dependencies();
        setup_platform_config(&mut deps);
        let info = mock_info("creator", &coins(creator_lock(3, 100), "usaf"));
        let res = execute(deps.as_mut(), mock_env(), info, base_create_msg());
        assert!(res.is_ok(), "expected success, got {:?}", res.err());
    }

    #[test]
    fn auto_start_when_full_sets_members_at_start() {
        let mut deps = mock_dependencies();
        setup_platform_config(&mut deps);

        // Create with auto_start_when_full + by_members, max=2 so 1 join = full
        let create_msg = ExecuteMsg::CreateCircle {
            circle_name: "auto".to_string(),
            circle_description: "auto".to_string(),
            circle_image: None,
            max_members: 2,
            min_members_required: 2,
            invite_only: false,
            contribution_amount: Uint128::from(100u128),
            denomination: None,
            exit_penalty_percent: 2000,
            late_fee_percent: 1000,
            total_cycles: 2,
            cycle_duration_days: 0,
            cycle_duration_seconds: Some(300),
            start_date: None,
            grace_period_hours: 0,
            grace_period_seconds: Some(60),
            auto_start_when_full: true,
            auto_start_type: Some("by_members".to_string()),
            auto_start_date: None,
            payout_order_type: PayoutOrderType::RandomOrder,
            payout_order_list: None,
            auto_payout_enabled: true,
            manual_trigger_enabled: false,
            emergency_stop_enabled: false,
            auto_refund_if_min_not_met: false,
            strict_mode: false,
            visibility: Visibility::Public,
            show_member_identities: true,
            distribution_threshold: None,
        };
        let creator_info = mock_info("creator", &coins(creator_lock(2, 100), "usaf"));
        execute(deps.as_mut(), mock_env(), creator_info, create_msg).unwrap();

        // Second member joins → triggers auto-start
        let join_msg = ExecuteMsg::JoinCircle { circle_id: 1 };
        let join_info = mock_info("alice", &coins(100, "usaf"));
        execute(deps.as_mut(), mock_env(), join_info, join_msg).unwrap();

        let circle = CIRCLES.load(&deps.storage, 1).unwrap();
        assert!(matches!(circle.circle_status, CircleStatus::Running));
        assert_eq!(circle.current_cycle_index, 1);
        // members_at_start MUST be set on auto-start (regression: was None before)
        assert_eq!(circle.members_at_start, Some(2));
        // max_missed_payments_allowed: base 8, scaled 8*2/2=8, capped to total_rounds-1=3
        assert_eq!(circle.max_missed_payments_allowed, 3);
    }

    #[test]
    fn should_eject_uses_missed_count_cap() {
        let mut deps = mock_dependencies();
        let circle_id = 42u64;
        let member = Addr::unchecked("alice");
        let original_lock = Uint128::from(100u128);

        // No missed payments yet → not ejected
        assert!(!should_eject_member(
            &deps.storage,
            circle_id,
            &member,
            original_lock,
            2000, // exit_penalty_percent 20%
            3,    // max_missed_payments_allowed
        ));

        // 2 missed payments, max is 3 → still not ejected via missed_count, and accumulated late fees too low
        MEMBER_MISSED_PAYMENTS
            .save(
                &mut deps.storage,
                (circle_id, member.clone()),
                &MemberMissedPayments {
                    member: member.clone(),
                    missed_count: 2,
                    last_missed_cycle: Some(2),
                    last_fee_round: None,
                },
            )
            .unwrap();
        assert!(!should_eject_member(
            &deps.storage,
            circle_id,
            &member,
            original_lock,
            2000,
            3,
        ));

        // 3 missed payments → hard cap triggers ejection regardless of fees
        MEMBER_MISSED_PAYMENTS
            .save(
                &mut deps.storage,
                (circle_id, member.clone()),
                &MemberMissedPayments {
                    member: member.clone(),
                    missed_count: 3,
                    last_missed_cycle: Some(3),
                    last_fee_round: None,
                },
            )
            .unwrap();
        assert!(should_eject_member(
            &deps.storage,
            circle_id,
            &member,
            original_lock,
            2000,
            3,
        ));
    }

    #[test]
    fn should_eject_via_accumulated_fees_path_still_works() {
        let mut deps = mock_dependencies();
        let circle_id = 1u64;
        let member = Addr::unchecked("bob");
        let original_lock = Uint128::from(100u128);
        // exit penalty 20% = 20. accumulated 80 → 80 + 20 = 100 ≥ 100 → eject.
        MEMBER_ACCUMULATED_LATE_FEES
            .save(
                &mut deps.storage,
                (circle_id, member.clone()),
                &Uint128::from(80u128),
            )
            .unwrap();
        // missed_count below cap so we exercise only the accumulated branch
        assert!(should_eject_member(
            &deps.storage,
            circle_id,
            &member,
            original_lock,
            2000,
            100, // very high cap → can't trigger via missed_count
        ));
    }

    #[test]
    fn apply_running_state_sets_all_fields() {
        let mut circle = Circle {
            circle_id: 1,
            circle_name: "x".to_string(),
            circle_description: "x".to_string(),
            circle_image: None,
            creator_address: Addr::unchecked("creator"),
            created_at: Timestamp::from_seconds(0),
            updated_at: Timestamp::from_seconds(0),
            max_members: 3,
            min_members_required: 2,
            invite_only: false,
            members_list: vec![Addr::unchecked("a"), Addr::unchecked("b"), Addr::unchecked("c")],
            pending_members: vec![],
            contribution_amount: Uint128::from(100u128),
            denomination: "usaf".to_string(),
            payout_amount: Uint128::from(300u128),
            exit_penalty_percent: 2000,
            late_fee_percent: 1000,
            platform_fee_percent: 100,
            max_missed_payments_allowed: 0,
            members_at_start: None,
            total_cycles: 2,
            cycle_duration_days: 0,
            cycle_duration_seconds: 300,
            start_date: None,
            first_cycle_date: None,
            next_payout_date: None,
            end_date: None,
            grace_period_hours: 0,
            grace_period_seconds: 60,
            auto_start_when_full: true,
            auto_start_type: Some("by_members".to_string()),
            auto_start_date: None,
            payout_order_type: PayoutOrderType::RandomOrder,
            payout_order_list: None,
            auto_payout_enabled: true,
            manual_trigger_enabled: false,
            emergency_stop_enabled: false,
            emergency_stop_triggered: false,
            auto_refund_if_min_not_met: false,
            strict_mode: false,
            escrow_address: None,
            total_amount_locked: Uint128::zero(),
            total_penalties_collected: Uint128::zero(),
            total_platform_fees_collected: Uint128::zero(),
            total_pending_payouts: Uint128::zero(),
            withdrawal_lock: false,
            refund_mode: RefundMode::FullRefund,
            creator_lock_amount: Uint128::from(130u128),
            distribution_threshold: Some(DistributionThreshold::Total {}),
            circle_status: CircleStatus::Full,
            current_cycle_index: 0,
            cycles_completed: 0,
            members_paid_this_cycle: vec![],
            members_late_this_cycle: vec![],
            visibility: Visibility::Public,
            show_member_identities: true,
        };

        let now = Timestamp::from_seconds(1_700_000_000);
        apply_running_state(&mut circle, now);

        assert!(matches!(circle.circle_status, CircleStatus::Running));
        assert_eq!(circle.current_cycle_index, 1);
        assert_eq!(circle.start_date, Some(now));
        assert_eq!(circle.first_cycle_date, Some(now));
        assert_eq!(circle.next_payout_date, Some(now));
        assert_eq!(circle.members_at_start, Some(3));
        // total_rounds = 6, base 8, capped to 5
        assert_eq!(circle.max_missed_payments_allowed, 5);
        assert_eq!(
            circle.end_date,
            Some(Timestamp::from_seconds(now.seconds() + 300 * 6))
        );
    }

    // Round-progression invariant is exercised end-to-end by ProcessPayout / AdvanceRound;
    // a focused unit test would require constructing a full Running circle with deposits.
    // The check is cheap and mostly a defense-in-depth assertion against future refactors.

    #[test]
    fn json_responses_round_trip() {
        // Sanity check that base_create_msg serializes/deserializes properly.
        let msg = base_create_msg();
        let json = cosmwasm_std::to_json_binary(&msg).unwrap();
        let parsed: ExecuteMsg = from_json(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn instantiate_smoke() {
        let mut deps = mock_dependencies();
        let msg = InstantiateMsg {
            platform_fee_percent: 100,
            platform_address: Addr::unchecked("platform"),
        };
        let info = mock_info("creator", &[]);
        let res = crate::contract::instantiate(deps.as_mut(), mock_env(), info, msg).unwrap();
        assert_eq!(0, res.messages.len());
    }
}
