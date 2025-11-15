use cosmwasm_std::{
    BankMsg, Coin, DepsMut, Env, MessageInfo, Response, StdResult, Timestamp, Uint128,
    Addr,
};
use cw_utils::must_pay;

use crate::error::ContractError;
use crate::msg::ExecuteMsg;
use crate::state::{
    Circle, CircleStatus, DepositRecord, EventLog, PayoutOrderType,
    PayoutRecord, PenaltyRecord, RefundMode, Visibility, CIRCLE_COUNTER, CIRCLES, DEPOSITS, EVENTS,
    EVENT_COUNTER, PAYOUTS, PENALTIES,
};

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
            penalty_fee_amount,
            late_fee_amount,
            total_cycles,
            cycle_duration_days,
            start_date,
            grace_period_hours,
            auto_start_when_full,
            payout_order_type,
            payout_order_list,
            auto_payout_enabled,
            manual_trigger_enabled,
            arbiter_address,
            emergency_stop_enabled,
            auto_refund_if_min_not_met,
            max_missed_payments_allowed,
            strict_mode,
            member_exit_allowed_before_start,
            visibility,
            show_member_identities,
            arbiter_fee_percent,
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
            penalty_fee_amount,
            late_fee_amount,
            total_cycles,
            cycle_duration_days,
            start_date,
            grace_period_hours,
            auto_start_when_full,
            payout_order_type,
            payout_order_list,
            auto_payout_enabled,
            manual_trigger_enabled,
            arbiter_address,
            emergency_stop_enabled,
            auto_refund_if_min_not_met,
            max_missed_payments_allowed,
            strict_mode,
            member_exit_allowed_before_start,
            visibility,
            show_member_identities,
            arbiter_fee_percent,
        ),
        ExecuteMsg::JoinCircle { circle_id } => execute_join_circle(deps, env, info, circle_id),
        ExecuteMsg::InviteMember {
            circle_id,
            member_address,
        } => execute_invite_member(deps, env, info, circle_id, member_address),
        ExecuteMsg::AcceptInvite { circle_id } => execute_accept_invite(deps, env, info, circle_id),
        ExecuteMsg::ExitCircle { circle_id } => execute_exit_circle(deps, env, info, circle_id),
        ExecuteMsg::StartCircle { circle_id } => execute_start_circle(deps, env, info, circle_id),
        ExecuteMsg::DepositContribution { circle_id } => {
            execute_deposit_contribution(deps, env, info, circle_id)
        }
        ExecuteMsg::ProcessPayout { circle_id } => {
            execute_process_payout(deps, env, info, circle_id)
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
    }
}

fn execute_create_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_name: String,
    circle_description: String,
    circle_image: Option<String>,
    max_members: u32,
    min_members_required: u32,
    invite_only: bool,
    contribution_amount: Uint128,
    penalty_fee_amount: Uint128,
    late_fee_amount: Uint128,
    total_cycles: u32,
    cycle_duration_days: u32,
    start_date: Option<Timestamp>,
    grace_period_hours: u32,
    auto_start_when_full: bool,
    payout_order_type: PayoutOrderType,
    payout_order_list: Option<Vec<Addr>>,
    auto_payout_enabled: bool,
    manual_trigger_enabled: bool,
    arbiter_address: Option<Addr>,
    emergency_stop_enabled: bool,
    auto_refund_if_min_not_met: bool,
    max_missed_payments_allowed: u32,
    strict_mode: bool,
    member_exit_allowed_before_start: bool,
    visibility: Visibility,
    show_member_identities: bool,
    arbiter_fee_percent: Option<u64>,
) -> Result<Response, ContractError> {
    // Validation
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
    if let Some(ref order_list) = payout_order_list {
        if order_list.len() as u32 != max_members {
            return Err(ContractError::InvalidParameters {
                msg: "payout_order_list length must match max_members".to_string(),
            });
        }
    }

    // Get next circle ID
    let circle_id = CIRCLE_COUNTER
        .may_load(deps.storage)?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| ContractError::InvalidParameters {
            msg: "Circle ID overflow".to_string(),
        })?;

    // Calculate payout amount
    let payout_amount = contribution_amount
        .checked_mul(Uint128::from(max_members))
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Payout amount overflow".to_string(),
        })?;

    // Calculate end date if start_date is provided
    let end_date = start_date.map(|start| {
        Timestamp::from_seconds(
            start.seconds()
                + (cycle_duration_days as u64 * total_cycles as u64 * 86400),
        )
    });

    // Initialize payout order list if random
    let final_payout_order = match payout_order_type {
        PayoutOrderType::RandomOrder => None, // Will be generated when circle starts
        PayoutOrderType::PredefinedOrder => payout_order_list,
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
        members_list: vec![info.sender.clone()], // Creator is first member
        pending_members: vec![],
        member_exit_allowed_before_start,
        contribution_amount,
        denomination: "saf".to_string(),
        payout_amount,
        penalty_fee_amount,
        late_fee_amount,
        platform_fee_percent: 0, // Will be set from config
        arbiter_fee_percent,
        total_cycles,
        cycle_duration_days,
        start_date,
        first_cycle_date: start_date,
        next_payout_date: start_date,
        end_date,
        grace_period_hours,
        auto_start_when_full,
        payout_order_type,
        payout_order_list: final_payout_order,
        auto_payout_enabled,
        manual_trigger_enabled,
        arbiter_address: if let Some(addr) = arbiter_address {
            Some(deps.api.addr_validate(addr.as_str())?)
        } else {
            None
        },
        emergency_stop_enabled,
        emergency_stop_triggered: false,
        auto_refund_if_min_not_met,
        max_missed_payments_allowed,
        strict_mode,
        escrow_address: Some(env.contract.address.clone()),
        total_amount_locked: Uint128::zero(),
        total_penalties_collected: Uint128::zero(),
        total_platform_fees_collected: Uint128::zero(),
        withdrawal_lock: false,
        refund_mode: RefundMode::FullRefund,
        circle_status: CircleStatus::Draft,
        current_cycle_index: 0,
        cycles_completed: 0,
        members_paid_this_cycle: vec![],
        members_late_this_cycle: vec![],
        visibility,
        show_member_identities,
    };

    CIRCLES.save(deps.storage, circle_id, &circle)?;
    CIRCLE_COUNTER.save(deps.storage, &circle_id)?;

    log_event(
        deps,
        &env,
        circle_id,
        "circle_created",
        &format!("Circle {} created by {}", circle_id, info.sender),
    )?;

    Ok(Response::new()
        .add_attribute("action", "create_circle")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("creator", info.sender))
}

fn execute_join_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Check if circle is open for joining
    if !matches!(circle.circle_status, CircleStatus::Draft | CircleStatus::Open) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Draft or Open".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    // Check if circle is full
    if circle.members_list.len() as u32 >= circle.max_members {
        return Err(ContractError::CircleFull {
            max: circle.max_members,
        });
    }

    // Check if already a member
    if circle.members_list.contains(&info.sender) {
        return Err(ContractError::AlreadyMember {
            address: info.sender.to_string(),
        });
    }

    // Check if invite-only
    if circle.invite_only {
        if !circle.pending_members.contains(&info.sender) {
            return Err(ContractError::InviteOnly { circle_id });
        }
        // Remove from pending
        circle.pending_members.retain(|m| m != &info.sender);
    }

    // Add member
    circle.members_list.push(info.sender.clone());
    circle.updated_at = env.block.time;

    // Update status
    if circle.members_list.len() as u32 >= circle.max_members {
        circle.circle_status = CircleStatus::Full;
        if circle.auto_start_when_full {
            // Auto-start logic would go here
        }
    } else if circle.circle_status == CircleStatus::Draft {
        circle.circle_status = CircleStatus::Open;
    }

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        deps,
        &env,
        circle_id,
        "member_joined",
        &format!("Member {} joined circle {}", info.sender, circle_id),
    )?;

    Ok(Response::new()
        .add_attribute("action", "join_circle")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", info.sender))
}

fn execute_invite_member(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Only creator or arbiter can invite
    if info.sender != circle.creator_address
        && circle.arbiter_address.as_ref() != Some(&info.sender)
    {
        return Err(ContractError::Unauthorized {
            msg: "Only creator or arbiter can invite members".to_string(),
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
        deps,
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

fn execute_accept_invite(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    // Same as join_circle but with invite check
    execute_join_circle(deps, env, info, circle_id)
}

fn execute_exit_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Check if exit is allowed
    if !circle.member_exit_allowed_before_start {
        return Err(ContractError::ExitNotAllowed { circle_id });
    }

    // Check if circle has started
    if matches!(
        circle.circle_status,
        CircleStatus::Running | CircleStatus::Full
    ) {
        return Err(ContractError::ExitNotAllowed { circle_id });
    }

    // Check if member is in the circle
    if !circle.members_list.contains(&info.sender) {
        return Err(ContractError::Unauthorized {
            msg: "Not a member of this circle".to_string(),
        });
    }

    // Remove member
    circle.members_list.retain(|m| m != &info.sender);
    circle.updated_at = env.block.time;

    // Update status
    if (circle.members_list.len() as u32) < circle.min_members_required {
        if circle.auto_refund_if_min_not_met {
            circle.circle_status = CircleStatus::Cancelled;
        }
    } else if (circle.members_list.len() as u32) < circle.max_members
        && circle.circle_status == CircleStatus::Full
    {
        circle.circle_status = CircleStatus::Open;
    }

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        deps,
        &env,
        circle_id,
        "member_exited",
        &format!("Member {} exited circle {}", info.sender, circle_id),
    )?;

    Ok(Response::new()
        .add_attribute("action", "exit_circle")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", info.sender))
}

fn execute_start_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Only creator can start
    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can start circle".to_string(),
        });
    }

    // Check status
    if !matches!(
        circle.circle_status,
        CircleStatus::Open | CircleStatus::Full
    ) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Open or Full".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    // Check minimum members
    if (circle.members_list.len() as u32) < circle.min_members_required {
        return Err(ContractError::MinMembersNotMet {
            required: circle.min_members_required,
            current: circle.members_list.len() as u32,
        });
    }

    // Generate random payout order if needed
    // Note: In production, use a proper random oracle or commit-reveal scheme
    // For now, we'll use a deterministic shuffle based on block time and circle_id
    if matches!(circle.payout_order_type, PayoutOrderType::RandomOrder) {
        let mut members = circle.members_list.clone();
        // Simple deterministic shuffle using block time as seed
        // This is not cryptographically secure but works for demo purposes
        let seed = env.block.time.seconds() + circle_id as u64;
        for i in 0..members.len() {
            let j = (seed as usize + i * 7) % members.len();
            members.swap(i, j);
        }
        circle.payout_order_list = Some(members);
    }

    // Set start date if not set
    if circle.start_date.is_none() {
        circle.start_date = Some(env.block.time);
        circle.first_cycle_date = Some(env.block.time);
        circle.next_payout_date = Some(env.block.time);
    }

    circle.circle_status = CircleStatus::Running;
    circle.current_cycle_index = 1;
    circle.updated_at = env.block.time;

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        deps,
        &env,
        circle_id,
        "circle_started",
        &format!("Circle {} started", circle_id),
    )?;

    Ok(Response::new()
        .add_attribute("action", "start_circle")
        .add_attribute("circle_id", circle_id.to_string()))
}

fn execute_deposit_contribution(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Check if circle is running
    if !matches!(circle.circle_status, CircleStatus::Running) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Running".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    // Check if member
    if !circle.members_list.contains(&info.sender) {
        return Err(ContractError::Unauthorized {
            msg: "Not a member of this circle".to_string(),
        });
    }

    // Check if already deposited for this cycle
    if DEPOSITS
        .may_load(deps.storage, (circle_id, info.sender.clone(), circle.current_cycle_index))?
        .is_some()
    {
        return Err(ContractError::AlreadyDeposited {
            address: info.sender.to_string(),
            cycle: circle.current_cycle_index,
        });
    }

    // Check payment amount
    let payment = must_pay(&info, &circle.denomination)
        .map_err(|_| ContractError::InsufficientFunds {
            required: circle.contribution_amount.to_string(),
            sent: "0".to_string(),
        })?;

    let mut amount_to_pay = circle.contribution_amount;
    let mut is_late = false;

    // Check if late
    if let Some(next_payout) = circle.next_payout_date {
        let grace_period_end = next_payout.plus_seconds(circle.grace_period_hours as u64 * 3600);
        if env.block.time > grace_period_end {
            is_late = true;
            if circle.strict_mode {
                return Err(ContractError::MemberLate {
                    address: info.sender.to_string(),
                });
            }
            amount_to_pay = amount_to_pay
                .checked_add(circle.late_fee_amount)
                .map_err(|_| ContractError::InvalidParameters {
                    msg: "Amount overflow".to_string(),
                })?;
        }
    }

    if payment < amount_to_pay {
        return Err(ContractError::InsufficientFunds {
            required: amount_to_pay.to_string(),
            sent: payment.to_string(),
        });
    }

    // Record deposit
    let deposit = DepositRecord {
        member: info.sender.clone(),
        cycle: circle.current_cycle_index,
        amount: circle.contribution_amount,
        timestamp: env.block.time,
        on_time: !is_late,
    };

    DEPOSITS.save(
        deps.storage,
        (circle_id, info.sender.clone(), circle.current_cycle_index),
        &deposit,
    )?;

    // Update circle state
    circle.total_amount_locked = circle
        .total_amount_locked
        .checked_add(circle.contribution_amount)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Total amount overflow".to_string(),
        })?;

    if is_late {
        circle.members_late_this_cycle.push(info.sender.clone());
        circle.total_penalties_collected = circle
            .total_penalties_collected
            .checked_add(circle.late_fee_amount)
            .map_err(|_| ContractError::InvalidParameters {
                msg: "Penalties overflow".to_string(),
            })?;

        // Record penalty
        let penalty = PenaltyRecord {
            member: info.sender.clone(),
            cycle: circle.current_cycle_index,
            amount: circle.late_fee_amount,
            reason: "Late payment".to_string(),
            timestamp: env.block.time,
        };
        PENALTIES.save(
            deps.storage,
            (circle_id, info.sender.clone(), circle.current_cycle_index),
            &penalty,
        )?;
    } else {
        circle.members_paid_this_cycle.push(info.sender.clone());
    }

    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        deps,
        &env,
        circle_id,
        "contribution_deposited",
        &format!(
            "Member {} deposited for cycle {}",
            info.sender, circle.current_cycle_index
        ),
    )?;

    Ok(Response::new()
        .add_attribute("action", "deposit_contribution")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", info.sender)
        .add_attribute("cycle", circle.current_cycle_index.to_string())
        .add_attribute("amount", circle.contribution_amount.to_string()))
}

fn execute_process_payout(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Check if circle is running
    if !matches!(circle.circle_status, CircleStatus::Running) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Running".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    // Check authorization
    if circle.manual_trigger_enabled {
        if info.sender != circle.creator_address
            && circle.arbiter_address.as_ref() != Some(&info.sender)
        {
            return Err(ContractError::Unauthorized {
                msg: "Only creator or arbiter can trigger payout".to_string(),
            });
        }
    }

    // Check if cycle is ready
    if let Some(next_payout) = circle.next_payout_date {
        if env.block.time < next_payout {
            return Err(ContractError::CycleNotReady {
                next_date: next_payout.seconds(),
            });
        }
    }

    // Verify all members have paid
    let all_paid = circle
        .members_list
        .iter()
        .all(|member| {
            DEPOSITS
                .may_load(
                    deps.storage,
                    (circle_id, member.clone(), circle.current_cycle_index),
                )
                .unwrap_or(None)
                .is_some()
        });

    if !all_paid {
        return Err(ContractError::InvalidParameters {
            msg: "Not all members have paid for this cycle".to_string(),
        });
    }

    // Get payout recipient
    let recipient = if let Some(ref order_list) = circle.payout_order_list {
        let index = (circle.current_cycle_index as usize - 1) % order_list.len();
        order_list[index].clone()
    } else {
        return Err(ContractError::InvalidParameters {
            msg: "Payout order not set".to_string(),
        });
    };

    // Calculate fees
    let platform_fee = circle
        .payout_amount
        .multiply_ratio(circle.platform_fee_percent, 10000u64);
    let arbiter_fee = circle.arbiter_fee_percent.map(|percent| {
        circle
            .payout_amount
            .multiply_ratio(percent, 10000u64)
    });

    let mut payout_amount = circle.payout_amount;
    payout_amount = payout_amount
        .checked_sub(platform_fee)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Payout amount underflow".to_string(),
        })?;

    if let Some(arb_fee) = arbiter_fee {
        payout_amount = payout_amount
            .checked_sub(arb_fee)
            .map_err(|_| ContractError::InvalidParameters {
                msg: "Payout amount underflow".to_string(),
            })?;
    }

    // Record payout
    let payout = PayoutRecord {
        cycle: circle.current_cycle_index,
        recipient: recipient.clone(),
        amount: payout_amount,
        timestamp: env.block.time,
        transaction_hash: None,
    };

    PAYOUTS.save(
        deps.storage,
        (circle_id, circle.current_cycle_index),
        &payout,
    )?;

    // Update circle state
    circle.total_amount_locked = circle
        .total_amount_locked
        .checked_sub(circle.payout_amount)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Total amount underflow".to_string(),
        })?;

    circle.total_platform_fees_collected = circle
        .total_platform_fees_collected
        .checked_add(platform_fee)
        .map_err(|_| ContractError::InvalidParameters {
            msg: "Platform fees overflow".to_string(),
        })?;

    circle.cycles_completed += 1;
    circle.members_paid_this_cycle.clear();
    circle.members_late_this_cycle.clear();

    // Check if last cycle
    if circle.current_cycle_index >= circle.total_cycles {
        circle.circle_status = CircleStatus::Completed;
    } else {
        circle.current_cycle_index += 1;
        // Set next payout date
        if let Some(current_date) = circle.next_payout_date {
            circle.next_payout_date = Some(Timestamp::from_seconds(
                current_date.seconds() + (circle.cycle_duration_days as u64 * 86400),
            ));
        }
    }

    circle.updated_at = env.block.time;
    CIRCLES.save(deps.storage, circle_id, &circle)?;

    // Build bank message for payout
    let messages = vec![BankMsg::Send {
        to_address: recipient.to_string(),
        amount: vec![Coin {
            denom: circle.denomination.clone(),
            amount: payout_amount,
        }],
    }];

    log_event(
        deps,
        &env,
        circle_id,
        "payout_processed",
        &format!(
            "Payout processed for cycle {} to {}",
            payout.cycle, recipient
        ),
    )?;

    Ok(Response::new()
        .add_messages(messages)
        .add_attribute("action", "process_payout")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("cycle", payout.cycle.to_string())
        .add_attribute("recipient", recipient)
        .add_attribute("amount", payout_amount.to_string()))
}

fn execute_pause_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Only creator or arbiter can pause
    if info.sender != circle.creator_address
        && circle.arbiter_address.as_ref() != Some(&info.sender)
    {
        return Err(ContractError::Unauthorized {
            msg: "Only creator or arbiter can pause circle".to_string(),
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

    log_event(
        deps,
        &env,
        circle_id,
        "circle_paused",
        &format!("Circle {} paused by {}", circle_id, info.sender),
    )?;

    Ok(Response::new()
        .add_attribute("action", "pause_circle")
        .add_attribute("circle_id", circle_id.to_string()))
}

fn execute_unpause_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Only creator or arbiter can unpause
    if info.sender != circle.creator_address
        && circle.arbiter_address.as_ref() != Some(&info.sender)
    {
        return Err(ContractError::Unauthorized {
            msg: "Only creator or arbiter can unpause circle".to_string(),
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

    log_event(
        deps,
        &env,
        circle_id,
        "circle_unpaused",
        &format!("Circle {} unpaused by {}", circle_id, info.sender),
    )?;

    Ok(Response::new()
        .add_attribute("action", "unpause_circle")
        .add_attribute("circle_id", circle_id.to_string()))
}

fn execute_emergency_stop(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    if !circle.emergency_stop_enabled {
        return Err(ContractError::InvalidParameters {
            msg: "Emergency stop not enabled for this circle".to_string(),
        });
    }

    // Only arbiter can trigger emergency stop
    if circle.arbiter_address.as_ref() != Some(&info.sender) {
        return Err(ContractError::ArbiterOnly {});
    }

    circle.emergency_stop_triggered = true;
    circle.circle_status = CircleStatus::Paused;
    circle.updated_at = env.block.time;

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    log_event(
        deps,
        &env,
        circle_id,
        "emergency_stop",
        &format!("Emergency stop triggered for circle {}", circle_id),
    )?;

    Ok(Response::new()
        .add_attribute("action", "emergency_stop")
        .add_attribute("circle_id", circle_id.to_string()))
}

fn execute_cancel_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut circle = CIRCLES.load(deps.storage, circle_id)?;

    // Only creator can cancel before start, creator or arbiter after start
    if info.sender != circle.creator_address
        && circle.arbiter_address.as_ref() != Some(&info.sender)
    {
        return Err(ContractError::Unauthorized {
            msg: "Only creator or arbiter can cancel circle".to_string(),
        });
    }

    // Can only cancel if not completed
    if matches!(circle.circle_status, CircleStatus::Completed) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Not Completed".to_string(),
            actual: format!("{:?}", circle.circle_status),
        });
    }

    circle.circle_status = CircleStatus::Cancelled;
    circle.updated_at = env.block.time;

    CIRCLES.save(deps.storage, circle_id, &circle)?;

    // Process refunds based on refund mode
    // This is a simplified version - full implementation would handle all refunds
    log_event(
        deps,
        &env,
        circle_id,
        "circle_cancelled",
        &format!("Circle {} cancelled by {}", circle_id, info.sender),
    )?;

    Ok(Response::new()
        .add_attribute("action", "cancel_circle")
        .add_attribute("circle_id", circle_id.to_string()))
}

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

    // Only creator can update
    if info.sender != circle.creator_address {
        return Err(ContractError::Unauthorized {
            msg: "Only creator can update circle".to_string(),
        });
    }

    // Can only update if not started
    if matches!(
        circle.circle_status,
        CircleStatus::Running | CircleStatus::Completed
    ) {
        return Err(ContractError::InvalidCircleStatus {
            expected: "Not Running or Completed".to_string(),
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

fn execute_withdraw_platform_fees(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _circle_id: Option<u64>,
) -> Result<Response, ContractError> {
    // This would require storing platform address in config
    // Simplified implementation
    Err(ContractError::InvalidParameters {
        msg: "Platform fee withdrawal not yet implemented".to_string(),
    })
}

// Helper function to log events
fn log_event(
    deps: DepsMut,
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

    let event = EventLog {
        event_type: event_type.to_string(),
        circle_id,
        data: data.to_string(),
        timestamp: env.block.time,
    };

    EVENTS.save(deps.storage, (circle_id, event_id), &event)?;
    EVENT_COUNTER.save(deps.storage, circle_id, &event_id)?;

    Ok(())
}

