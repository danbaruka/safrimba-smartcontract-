//! Execute handlers (engine v5). Every handler that touches a circle loads the
//! in-memory model, applies the clock (`sync`) first, then its own operation,
//! then persists. Funds always move by push (BankMsg) inside the same tx.

use cosmwasm_std::{
    Addr, DepsMut, Env, MessageInfo, Response, Storage, Timestamp, Uint128,
};
use cw_utils::{must_pay, nonpayable};

use crate::engine::{compute_max_missed, Model, MAX_PAUSE_REASON_LEN, MAX_ROUNDS_PER_TX};
use crate::error::ContractError;
use crate::msg::ExecuteMsg;
use crate::state::{
    Circle, CircleStatus, DepositRecord, DistributionThreshold, EjectionPolicy, EventLog,
    MemberRating, MemberState, MemberStatus, PayoutOrderType, RefundMode, Visibility,
    CIRCLES, CIRCLE_COUNTER, CREATOR_REWARDS_CREDITED, ENGINE_VERSION, EVENTS, EVENT_COUNTER,
    MEMBERS, MEMBER_PSEUDONYMS, MEMBER_RATINGS, PENDING_PAYOUTS, PLATFORM_CONFIG,
    PRIVATE_MEMBER_LIST, WALLET_RATING_COUNT, WALLET_RATING_INDEX,
};

const MAX_MEMBERS_CAP: u32 = 100;
const MAX_TOTAL_CYCLES_CAP: u32 = 120;
const SAF_DENOM: &str = "usaf";
const USDC_DENOM_TESTNET: &str =
    "ibc/FC6C34533ECF1AAD296E41A70D8F16089E90D436904B56EDE19342D6DE172B82";
const USDC_DENOM_MAINNET: &str =
    "ibc/8E27BA2D5493AF5636760E354E46004562C46AB7EC0CC4C1CA14E9E20E2545B5";

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
            auto_payout_enabled: _,
            manual_trigger_enabled: _,
            emergency_stop_enabled,
            auto_refund_if_min_not_met,
            strict_mode,
            visibility,
            show_member_identities,
            distribution_threshold,
            ejection_policy,
            co_admin_address,
        } => create_circle(
            deps,
            env,
            info,
            CreateParams {
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
                emergency_stop_enabled,
                auto_refund_if_min_not_met,
                strict_mode,
                visibility,
                show_member_identities,
                distribution_threshold,
                ejection_policy,
                co_admin_address,
            },
        ),
        ExecuteMsg::JoinCircle { circle_id } | ExecuteMsg::AcceptInvite { circle_id } => {
            join_circle(deps, env, info, circle_id)
        }
        ExecuteMsg::InviteMember {
            circle_id,
            member_address,
        } => invite_member(deps, env, info, circle_id, member_address),
        ExecuteMsg::ExitCircle { circle_id } => exit_circle(deps, env, info, circle_id),
        ExecuteMsg::StartCircle { circle_id } => start_circle(deps, env, info, circle_id),
        ExecuteMsg::DepositContribution { circle_id } => deposit(deps, env, info, circle_id),
        ExecuteMsg::Tick { circle_id }
        | ExecuteMsg::ProcessPayout { circle_id }
        | ExecuteMsg::AdvanceRound { circle_id }
        | ExecuteMsg::CheckAndEject { circle_id } => tick(deps, env, info, circle_id),
        ExecuteMsg::Withdraw { circle_id } => withdraw(deps, env, info, circle_id),
        ExecuteMsg::EjectMember {
            circle_id,
            member_address,
        } => eject_member(deps, env, info, circle_id, member_address),
        ExecuteMsg::SetCoAdmin {
            circle_id,
            co_admin_address,
        } => set_co_admin(deps, env, info, circle_id, co_admin_address),
        ExecuteMsg::ClearCoAdmin { circle_id } => clear_co_admin(deps, env, info, circle_id),
        ExecuteMsg::ReanchorSchedule { .. } => Err(ContractError::Removed {
            msg: "deadlines are derived from the clock; overdue rounds settle automatically"
                .to_string(),
        }),
        ExecuteMsg::EndCycle { circle_id } => end_cycle(deps, env, info, circle_id),
        ExecuteMsg::RateMember {
            circle_id,
            member_address,
            score,
            note,
        } => rate_member(deps, env, info, circle_id, member_address, score, note),
        ExecuteMsg::PauseCircle { circle_id, reason } => {
            pause(deps, env, info, circle_id, reason, false)
        }
        ExecuteMsg::EmergencyStop { circle_id, reason } => {
            pause(deps, env, info, circle_id, reason, true)
        }
        ExecuteMsg::UnpauseCircle { circle_id } => unpause(deps, env, info, circle_id),
        ExecuteMsg::CancelCircle { circle_id } => cancel_circle(deps, env, info, circle_id),
        ExecuteMsg::RepayArrears { circle_id } => repay_arrears(deps, env, info, circle_id),
        ExecuteMsg::UpdateCircle {
            circle_id,
            circle_name,
            circle_description,
            circle_image,
        } => update_circle(
            deps,
            env,
            info,
            circle_id,
            circle_name,
            circle_description,
            circle_image,
        ),
        ExecuteMsg::WithdrawPlatformFees { .. } => Err(ContractError::Removed {
            msg: "platform fees are sent automatically at payout time".to_string(),
        }),
        ExecuteMsg::SweepDust { circle_id } => sweep_dust(deps, env, info, circle_id),
        ExecuteMsg::DepositCreatorReward { circle_id } => {
            deposit_creator_reward(deps, env, info, circle_id)
        }
        ExecuteMsg::AddPrivateMember {
            circle_id,
            member_address,
            pseudonym,
        } => add_private_member(deps, env, info, circle_id, member_address, pseudonym),
        ExecuteMsg::UpdateMemberPseudonym {
            circle_id,
            member_address,
            pseudonym,
        } => update_member_pseudonym(deps, env, info, circle_id, member_address, pseudonym),
        ExecuteMsg::BlockMember { .. } => Err(ContractError::Removed {
            msg: "use EjectMember (penalties are applied by the engine)".to_string(),
        }),
        ExecuteMsg::DistributeBlockedFunds { .. } => Err(ContractError::Removed {
            msg: "pooled penalties are distributed automatically at the end".to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn finish(
    model: Model,
    storage: &mut dyn Storage,
    action: &str,
    extra: Vec<(&str, String)>,
) -> Result<Response, ContractError> {
    let id = model.circle.circle_id;
    let status = format!("{:?}", model.circle.circle_status);
    let next = model
        .next_action_at()
        .map(|t| t.seconds().to_string())
        .unwrap_or_default();
    let settled = model.circle.rounds_settled;
    let settled_now = model.fx.rounds_settled_now;
    let behind = model.rounds_behind();
    let (msgs, events) = model.persist(storage)?;
    let mut res = Response::new()
        .add_submessages(msgs)
        .add_events(events)
        .add_attribute("action", action)
        .add_attribute("circle_id", id.to_string())
        .add_attribute("engine", ENGINE_VERSION.to_string())
        .add_attribute("circle_status", status)
        .add_attribute("rounds_settled", settled.to_string())
        .add_attribute("rounds_settled_now", settled_now.to_string())
        .add_attribute("rounds_behind", behind.to_string())
        .add_attribute("next_action_at", next);
    for (k, v) in extra {
        res = res.add_attribute(k, v);
    }
    Ok(res)
}

fn load_synced(storage: &dyn Storage, env: &Env, circle_id: u64) -> Result<Model, ContractError> {
    let mut model = Model::load(storage, env, circle_id)?;
    model.sync(MAX_ROUNDS_PER_TX)?;
    Ok(model)
}

/// Admin powers are resolved dynamically (PLAN_V5 3.4): the creator and the
/// co-admin while they are active members, else the active member with the
/// lowest slot. They organise; they never touch other members' money.
fn require_admin(model: &Model, sender: &Addr) -> Result<(), ContractError> {
    if model.is_admin(sender) {
        return Ok(());
    }
    if model.circle.co_admin_address.as_ref() == Some(sender) && model.member(sender).is_none() {
        return Err(ContractError::Unauthorized {
            msg: "Co-admin must join the circle first".to_string(),
        });
    }
    Err(ContractError::Unauthorized {
        msg: "Only the circle admins can perform this action".to_string(),
    })
}

/// Every action that changes the roster or the schedule acts on an up-to-date
/// state (PLAN_V5 C8): a circle more than `MAX_ROUNDS_PER_TX` rounds behind must
/// be ticked first.
fn require_caught_up(model: &Model) -> Result<(), ContractError> {
    if model.rounds_behind() > 0 {
        return Err(ContractError::CatchingUp {});
    }
    Ok(())
}

fn require_running(model: &Model) -> Result<(), ContractError> {
    if !matches!(
        model.circle.circle_status,
        CircleStatus::Running | CircleStatus::Paused
    ) {
        return Err(status_err("Running or Paused", &model.circle));
    }
    Ok(())
}

/// Reject contract addresses where a member must be a person (PLAN_V5 C5/C16).
fn require_not_contract(deps: &DepsMut, addr: &Addr) -> Result<(), ContractError> {
    if deps.querier.query_wasm_contract_info(addr.as_str()).is_ok() {
        return Err(invalid(format!(
            "{} is a smart contract; add it with an invitation it accepts itself",
            addr
        )));
    }
    Ok(())
}

fn status_err(expected: &str, circle: &Circle) -> ContractError {
    ContractError::InvalidCircleStatus {
        expected: expected.to_string(),
        actual: format!("{:?}", circle.circle_status),
    }
}

fn exact_payment(info: &MessageInfo, denom: &str, required: Uint128) -> Result<(), ContractError> {
    let paid = must_pay(info, denom).map_err(|_| ContractError::InsufficientFunds {
        required: format!("{}{}", required, denom),
        sent: "0".to_string(),
    })?;
    if paid != required {
        return Err(ContractError::InsufficientFunds {
            required: format!("{}{}", required, denom),
            sent: format!("{}{}", paid, denom),
        });
    }
    Ok(())
}

fn append_log(
    storage: &mut dyn Storage,
    circle_id: u64,
    kind: &str,
    data: String,
    now: Timestamp,
) -> Result<(), ContractError> {
    let n = EVENT_COUNTER.may_load(storage, circle_id)?.unwrap_or(0) + 1;
    EVENTS.save(
        storage,
        (circle_id, n),
        &EventLog {
            event_type: kind.to_string(),
            circle_id,
            data,
            timestamp: now,
        },
    )?;
    EVENT_COUNTER.save(storage, circle_id, &n)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

pub struct CreateParams {
    pub circle_name: String,
    pub circle_description: String,
    pub circle_image: Option<String>,
    pub max_members: u32,
    pub min_members_required: u32,
    pub invite_only: bool,
    pub contribution_amount: Uint128,
    pub denomination: Option<String>,
    pub exit_penalty_percent: u64,
    pub late_fee_percent: u64,
    pub total_cycles: u32,
    pub cycle_duration_days: u32,
    pub cycle_duration_seconds: Option<u64>,
    pub start_date: Option<Timestamp>,
    pub grace_period_hours: u32,
    pub grace_period_seconds: Option<u64>,
    pub auto_start_when_full: bool,
    pub auto_start_type: Option<String>,
    pub auto_start_date: Option<Timestamp>,
    pub payout_order_type: PayoutOrderType,
    pub payout_order_list: Option<Vec<Addr>>,
    pub emergency_stop_enabled: bool,
    pub auto_refund_if_min_not_met: bool,
    pub strict_mode: bool,
    pub visibility: Visibility,
    pub show_member_identities: bool,
    pub distribution_threshold: Option<DistributionThreshold>,
    pub ejection_policy: Option<EjectionPolicy>,
    pub co_admin_address: Option<Addr>,
}

fn invalid(msg: impl Into<String>) -> ContractError {
    ContractError::InvalidParameters { msg: msg.into() }
}

fn create_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    p: CreateParams,
) -> Result<Response, ContractError> {
    if p.circle_name.trim().is_empty() || p.circle_name.len() > 128 {
        return Err(invalid("circle_name must be 1-128 characters"));
    }
    if p.circle_description.len() > 2_000 {
        return Err(invalid("circle_description is limited to 2000 characters"));
    }
    if p.max_members < 2 || p.min_members_required == 0 {
        return Err(invalid(
            "max_members must be at least 2 and min_members_required greater than 0",
        ));
    }
    if p.max_members > MAX_MEMBERS_CAP {
        return Err(invalid(format!("max_members cannot exceed {}", MAX_MEMBERS_CAP)));
    }
    if p.min_members_required > p.max_members {
        return Err(invalid("min_members_required cannot exceed max_members"));
    }
    if p.total_cycles == 0 || p.total_cycles > MAX_TOTAL_CYCLES_CAP {
        return Err(invalid(format!(
            "total_cycles must be between 1 and {}",
            MAX_TOTAL_CYCLES_CAP
        )));
    }
    if p.contribution_amount.is_zero() {
        return Err(invalid("contribution_amount must be greater than 0"));
    }
    if p.exit_penalty_percent > 9_000 {
        return Err(invalid("exit_penalty_percent cannot exceed 90% (9000 basis points)"));
    }
    if p.late_fee_percent == 0 || p.late_fee_percent > 5_000 {
        return Err(invalid("late_fee_percent must be between 1 and 5000 basis points"));
    }
    if p.exit_penalty_percent + p.late_fee_percent > 10_000 {
        return Err(invalid("exit_penalty_percent + late_fee_percent cannot exceed 100%"));
    }
    if matches!(p.visibility, Visibility::Public) {
        return Err(invalid(
            "Public circles are temporarily disabled. Please create a Private (invite-only) circle.",
        ));
    }
    let denom = match p.denomination.as_deref() {
        None | Some("") => SAF_DENOM.to_string(),
        Some(d) if d == SAF_DENOM || d == USDC_DENOM_TESTNET || d == USDC_DENOM_MAINNET => {
            d.to_string()
        }
        Some(other) => {
            return Err(invalid(format!(
                "denomination '{}' is not supported. Allowed: '{}', '{}', or '{}'.",
                other, SAF_DENOM, USDC_DENOM_TESTNET, USDC_DENOM_MAINNET
            )))
        }
    };
    let threshold = match p.distribution_threshold {
        Some(DistributionThreshold::MinMembers { count }) => {
            if count == 0 {
                return Err(invalid("distribution_threshold MinMembers count must be > 0"));
            }
            if count > p.min_members_required.max(1) {
                return Err(invalid(format!(
                    "distribution_threshold MinMembers count ({}) cannot exceed min_members_required ({})",
                    count, p.min_members_required
                )));
            }
            Some(DistributionThreshold::MinMembers { count })
        }
        _ => Some(DistributionThreshold::Total {}),
    };
    if let Some(list) = &p.payout_order_list {
        if list.len() as u32 > p.max_members {
            return Err(invalid("payout_order_list cannot be longer than max_members"));
        }
        for a in list {
            deps.api.addr_validate(a.as_str())?;
        }
    }
    let cycle_secs = p
        .cycle_duration_seconds
        .filter(|&s| s > 0)
        .unwrap_or(p.cycle_duration_days as u64 * 86_400);
    if cycle_secs < 60 {
        return Err(invalid("round duration must be at least 60 seconds"));
    }
    let grace_secs = p
        .grace_period_seconds
        .filter(|&s| s > 0)
        .unwrap_or(p.grace_period_hours as u64 * 3_600);
    if grace_secs >= cycle_secs {
        return Err(invalid(format!(
            "grace_period ({}s) must be strictly less than cycle_duration ({}s)",
            grace_secs, cycle_secs
        )));
    }
    let auto_type = p.auto_start_type.clone().filter(|t| !t.is_empty());
    if let Some(t) = &auto_type {
        if t != "by_members" && t != "by_date" {
            return Err(invalid("auto_start_type must be 'by_members' or 'by_date'"));
        }
        if t == "by_date" {
            match p.auto_start_date {
                Some(d) if d > env.block.time => {}
                _ => return Err(invalid("auto_start_date must be in the future for by_date")),
            }
        }
    }
    if CIRCLES
        .keys(deps.storage, None, None, cosmwasm_std::Order::Ascending)
        .next()
        .is_some()
    {
        return Err(invalid(
            "This contract instance already hosts a circle. Instantiate a new contract for each circle.",
        ));
    }

    let creator_lock = p
        .contribution_amount
        .checked_mul(Uint128::from(2u64))
        .map_err(|_| invalid("Creator lock overflow"))?;
    exact_payment(&info, &denom, creator_lock)?;

    let mut pending_members: Vec<Addr> = vec![];
    let co_admin = match p.co_admin_address {
        Some(a) => {
            let v = deps.api.addr_validate(a.as_str())?;
            if v == info.sender {
                return Err(invalid("Co-admin cannot be the creator"));
            }
            pending_members.push(v.clone());
            Some(v)
        }
        None => None,
    };

    let platform = PLATFORM_CONFIG.load(deps.storage)?;
    let circle_id = CIRCLE_COUNTER.may_load(deps.storage)?.unwrap_or(0) + 1;
    let now = env.block.time;
    let preview_rounds = p.max_members * p.total_cycles;
    let circle = Circle {
        circle_id,
        circle_name: p.circle_name,
        circle_description: p.circle_description,
        circle_image: p.circle_image,
        creator_address: info.sender.clone(),
        created_at: now,
        updated_at: now,
        max_members: p.max_members,
        min_members_required: p.min_members_required,
        invite_only: p.invite_only || matches!(p.visibility, Visibility::Private),
        members_list: vec![info.sender.clone()],
        pending_members,
        contribution_amount: p.contribution_amount,
        denomination: denom,
        payout_amount: p.contribution_amount * Uint128::from(p.max_members),
        exit_penalty_percent: p.exit_penalty_percent,
        late_fee_percent: p.late_fee_percent,
        platform_fee_percent: platform.platform_fee_percent,
        max_missed_payments_allowed: compute_max_missed(
            p.exit_penalty_percent,
            p.late_fee_percent,
            preview_rounds,
        ),
        members_at_start: None,
        total_cycles: p.total_cycles,
        cycle_duration_days: p.cycle_duration_days,
        cycle_duration_seconds: p.cycle_duration_seconds.unwrap_or(0),
        start_date: p.start_date,
        first_cycle_date: None,
        next_payout_date: None,
        end_date: None,
        grace_period_hours: p.grace_period_hours,
        grace_period_seconds: p.grace_period_seconds.unwrap_or(0),
        auto_start_when_full: p.auto_start_when_full,
        auto_start_type: auto_type,
        auto_start_date: p.auto_start_date,
        payout_order_type: p.payout_order_type,
        payout_order_list: p.payout_order_list,
        auto_payout_enabled: true,
        manual_trigger_enabled: false,
        emergency_stop_enabled: p.emergency_stop_enabled,
        emergency_stop_triggered: false,
        auto_refund_if_min_not_met: p.auto_refund_if_min_not_met,
        strict_mode: p.strict_mode,
        escrow_address: Some(env.contract.address.clone()),
        total_amount_locked: creator_lock,
        total_penalties_collected: Uint128::zero(),
        total_platform_fees_collected: Uint128::zero(),
        total_pending_payouts: Uint128::zero(),
        withdrawal_lock: false,
        refund_mode: RefundMode::FullRefund,
        creator_lock_amount: creator_lock,
        distribution_threshold: threshold,
        circle_status: CircleStatus::Draft,
        current_cycle_index: 0,
        cycles_completed: 0,
        members_paid_this_cycle: vec![],
        members_late_this_cycle: vec![],
        visibility: p.visibility,
        show_member_identities: p.show_member_identities,
        ejection_policy: p.ejection_policy.unwrap_or_default(),
        co_admin_address: co_admin,
        engine_version: ENGINE_VERSION,
        rounds_settled: 0,
        paused_secs_total: 0,
        paused_at: None,
        carry_pot: Uint128::zero(),
        common_pool: Uint128::zero(),
        total_distributed: Uint128::zero(),
        total_arrears: Uint128::zero(),
        closed_at: None,
        end_after_cycle: None,
        pause_reason: None,
        comp_pool: Uint128::zero(),
    };
    CIRCLES.save(deps.storage, circle_id, &circle)?;
    CIRCLE_COUNTER.save(deps.storage, &circle_id)?;
    MEMBERS.save(
        deps.storage,
        (circle_id, &info.sender),
        &MemberState::new(info.sender.clone(), creator_lock, now),
    )?;
    append_log(
        deps.storage,
        circle_id,
        "circle_created",
        format!(
            "Circle {} created by {} (creator_lock: {})",
            circle_id, info.sender, creator_lock
        ),
        now,
    )?;
    Ok(Response::new()
        .add_attribute("action", "create_circle")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("engine", ENGINE_VERSION.to_string())
        .add_attribute("creator", info.sender)
        .add_attribute("creator_lock_amount", creator_lock.to_string())
        .add_attribute(
            "next_action_at",
            circle
                .auto_start_date
                .filter(|_| circle.auto_start_type.as_deref() == Some("by_date"))
                .map(|t| t.seconds().to_string())
                .unwrap_or_default(),
        ))
}

// ---------------------------------------------------------------------------
// Membership
// ---------------------------------------------------------------------------

fn join_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    if !matches!(
        model.circle.circle_status,
        CircleStatus::Draft | CircleStatus::Open
    ) {
        return Err(status_err("Draft or Open", &model.circle));
    }
    if model.members.len() as u32 >= model.circle.max_members {
        return Err(ContractError::CircleFull {
            max: model.circle.max_members,
        });
    }
    if model.index_of(&info.sender).is_some() {
        return Err(ContractError::AlreadyMember {
            address: info.sender.to_string(),
        });
    }
    if model.circle.invite_only || matches!(model.circle.visibility, Visibility::Private) {
        if !model.circle.pending_members.contains(&info.sender) {
            return Err(ContractError::InviteOnly { circle_id });
        }
        model.circle.pending_members.retain(|m| m != info.sender);
    }
    let lock = model.circle.contribution_amount;
    exact_payment(&info, &model.circle.denomination.clone(), lock)?;
    model.push_member(MemberState::new(info.sender.clone(), lock, env.block.time));
    model.circle.members_list.push(info.sender.clone());
    model.log(
        "member_joined",
        format!("Member {} joined (locked: {})", info.sender, lock),
    );
    model.emit(
        "safrimba_member_joined",
        vec![("member", info.sender.to_string()), ("lock", lock.to_string())],
    );
    model.after_admission()?;
    let started = model.fx.started_now;
    finish(
        model,
        deps.storage,
        "join_circle",
        vec![
            ("member", info.sender.to_string()),
            ("locked_amount", lock.to_string()),
            ("auto_started", started.to_string()),
        ],
    )
}

fn invite_member(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    if !matches!(
        model.circle.circle_status,
        CircleStatus::Draft | CircleStatus::Open
    ) {
        return Err(status_err("Draft or Open", &model.circle));
    }
    let addr = deps.api.addr_validate(member_address.as_str())?;
    if model.index_of(&addr).is_some() {
        return Err(ContractError::AlreadyMember {
            address: addr.to_string(),
        });
    }
    if model.circle.pending_members.contains(&addr) {
        return Err(invalid("Member already invited"));
    }
    model.circle.pending_members.push(addr.clone());
    model.log("member_invited", format!("Member {} invited", addr));
    model.emit("safrimba_invited", vec![("member", addr.to_string())]);
    finish(
        model,
        deps.storage,
        "invite_member",
        vec![("member", addr.to_string())],
    )
}

fn add_private_member(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
    pseudonym: Option<String>,
) -> Result<Response, ContractError> {
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    if !matches!(model.circle.visibility, Visibility::Private) {
        return Err(invalid("Circle must be private to use AddPrivateMember"));
    }
    if !matches!(
        model.circle.circle_status,
        CircleStatus::Draft | CircleStatus::Open
    ) {
        return Err(status_err("Draft or Open", &model.circle));
    }
    if model.members.len() as u32 >= model.circle.max_members {
        return Err(ContractError::CircleFull {
            max: model.circle.max_members,
        });
    }
    let addr = deps.api.addr_validate(member_address.as_str())?;
    if model.index_of(&addr).is_some() {
        return Err(ContractError::AlreadyMember {
            address: addr.to_string(),
        });
    }
    require_not_contract(&deps, &addr)?;
    let lock = model.circle.contribution_amount;
    exact_payment(&info, &model.circle.denomination.clone(), lock)?;
    model.circle.pending_members.retain(|m| m != addr);
    model.push_member(MemberState::new(addr.clone(), lock, env.block.time));
    model.circle.members_list.push(addr.clone());
    let mut private = PRIVATE_MEMBER_LIST
        .may_load(deps.storage, circle_id)?
        .unwrap_or_default();
    if !private.contains(&addr) {
        private.push(addr.clone());
    }
    PRIVATE_MEMBER_LIST.save(deps.storage, circle_id, &private)?;
    if let Some(p) = pseudonym {
        MEMBER_PSEUDONYMS.save(deps.storage, (circle_id, addr.clone()), &p)?;
    }
    model.log(
        "private_member_added",
        format!("Private member {} added by {} (lock {})", addr, info.sender, lock),
    );
    model.emit(
        "safrimba_member_joined",
        vec![("member", addr.to_string()), ("lock", lock.to_string())],
    );
    model.after_admission()?;
    finish(
        model,
        deps.storage,
        "add_private_member",
        vec![("member", addr.to_string()), ("join_lock", lock.to_string())],
    )
}

fn exit_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    let i = model
        .index_of(&info.sender)
        .filter(|&i| model.members[i].is_active())
        .ok_or_else(|| ContractError::Unauthorized {
            msg: "Not an active member of this circle".to_string(),
        })?;
    let is_creator = info.sender == model.circle.creator_address;
    match model.circle.circle_status {
        CircleStatus::Draft | CircleStatus::Open | CircleStatus::Full => {
            nonpayable(&info)?;
            if is_creator {
                return Err(invalid("The creator cannot leave; cancel the circle instead"));
            }
            let lock = model.members[i].lock;
            model.remove_member(i);
            model.circle.members_list.retain(|a| a != info.sender);
            if !lock.is_zero() {
                model.push_send(&info.sender, lock);
            }
            if model.circle.circle_status == CircleStatus::Full {
                model.circle.circle_status = CircleStatus::Open;
            }
            if model.members.len() <= 1 {
                model.circle.circle_status = CircleStatus::Draft;
            }
            model.log(
                "member_exited",
                format!("Member {} left before start (refund {})", info.sender, lock),
            );
            model.emit(
                "safrimba_member_left",
                vec![
                    ("member", info.sender.to_string()),
                    ("round", "0".to_string()),
                    ("reason", "exit_before_start".to_string()),
                    ("penalty", "0".to_string()),
                ],
            );
            finish(
                model,
                deps.storage,
                "exit_circle",
                vec![("refund_amount", lock.to_string()), ("started", "false".to_string())],
            )
        }
        CircleStatus::Running | CircleStatus::Paused => {
            require_caught_up(&model)?;
            if model.circle.strict_mode && !model.at_cycle_boundary() {
                return Err(ContractError::StrictModeNoExit { circle_id });
            }
            let settled = model.circle.rounds_settled;
            let at = settled + 1;
            // Same computation as `GetCircleView.me.exit_quote`.
            let plan = model.departure_plan(i, at, settled, true);
            if plan.to_attach.is_zero() {
                nonpayable(&info)?;
            } else {
                let denom = model.circle.denomination.clone();
                exact_payment(&info, &denom, plan.to_attach)?;
            }
            let sender = info.sender.to_string();
            let plan = model.leave(i, at, settled, "voluntary", MemberStatus::Exited, &sender, true);
            if model.active_count() < 2 {
                model.close(CircleStatus::Ended, "not_enough_active_members", None)?;
            }
            finish(
                model,
                deps.storage,
                "exit_circle",
                vec![
                    ("started", "true".to_string()),
                    ("served", plan.served.to_string()),
                    ("penalty", plan.penalty.to_string()),
                    ("claim", plan.claim.to_string()),
                    ("lock_refund", plan.lock_refund.to_string()),
                    ("settled", plan.owed.to_string()),
                ],
            )
        }
        _ => Err(status_err("Draft, Open, Full, Running or Paused", &model.circle)),
    }
}

fn start_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    if !matches!(
        model.circle.circle_status,
        CircleStatus::Draft | CircleStatus::Open | CircleStatus::Full
    ) {
        return Err(status_err("Open or Full", &model.circle));
    }
    let n = model.members.len() as u32;
    if n < model.min_to_start() {
        return Err(ContractError::MinMembersNotMet {
            required: model.min_to_start(),
            current: n,
        });
    }
    model.start(env.block.time, "admin")?;
    let rounds = model.circle.total_rounds();
    finish(
        model,
        deps.storage,
        "start_circle",
        vec![
            ("start_date", env.block.time.seconds().to_string()),
            ("total_rounds", rounds.to_string()),
        ],
    )
}

// ---------------------------------------------------------------------------
// Money
// ---------------------------------------------------------------------------

fn deposit(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    if model.rounds_behind() > 0 {
        return Err(ContractError::CatchingUp {});
    }
    if !matches!(
        model.circle.circle_status,
        CircleStatus::Running | CircleStatus::Paused
    ) {
        return Err(status_err("Running", &model.circle));
    }
    let i = model
        .index_of(&info.sender)
        .filter(|&i| model.members[i].is_active())
        .ok_or_else(|| ContractError::Unauthorized {
            msg: "Not an active member of this circle".to_string(),
        })?;
    let clock = model.clock().ok_or(ContractError::CircleNotStarted { circle_id })?;
    let now_s = env.block.time.seconds();
    let target = model.members[i].paid_through + 1;
    if target > clock.total_rounds {
        return Err(ContractError::NothingToPay {});
    }
    if !clock.is_open(target, now_s) {
        return Err(ContractError::RoundNotOpen {
            round: target,
            opens_at: clock.to_wall(clock.opens(target), now_s),
        });
    }
    let contribution = model.circle.contribution_amount;
    let denom = model.circle.denomination.clone();
    let arrears = model.members[i].arrears;
    let refill = model.members[i].refill_needed();
    let min = contribution;
    let max = contribution + arrears + refill;
    let paid = must_pay(&info, &denom).map_err(|_| ContractError::InvalidPayment {
        min: format!("{}{}", min, denom),
        max: format!("{}{}", max, denom),
        sent: "0".to_string(),
    })?;
    if paid < min || paid > max {
        return Err(ContractError::InvalidPayment {
            min: format!("{}{}", min, denom),
            max: format!("{}{}", max, denom),
            sent: format!("{}{}", paid, denom),
        });
    }
    let mut extra = paid - contribution;
    let recovered = extra.min(arrears);
    extra -= recovered;
    let refilled = extra.min(refill);
    let on_time = clock.eff(now_s) <= clock.due(target);
    {
        let m = model.m(i);
        m.arrears -= recovered;
        m.lock += refilled;
        m.extra_paid += paid - contribution;
        m.paid_through = target;
        m.contributed += contribution;
        m.paid_count += 1;
        if !on_time {
            m.late_count += 1;
        }
        if m.arrears.is_zero() && m.lock >= m.lock_original {
            m.strikes = 0;
        }
    }
    model.circle.common_pool += recovered;
    model.circle.total_arrears = model.circle.total_arrears.saturating_sub(recovered);
    model.fx.deposits.push(DepositRecord {
        member: info.sender.clone(),
        cycle: target,
        amount: contribution,
        timestamp: env.block.time,
        on_time,
    });
    model.log(
        "contribution_deposited",
        format!(
            "Member {} paid round {} ({}{}; arrears {}, lock refill {}, on_time: {})",
            info.sender, target, contribution, denom, recovered, refilled, on_time
        ),
    );
    model.emit(
        "safrimba_deposit",
        vec![
            ("member", info.sender.to_string()),
            ("round", target.to_string()),
            ("amount", contribution.to_string()),
            ("arrears_paid", recovered.to_string()),
            ("lock_refill", refilled.to_string()),
            ("on_time", on_time.to_string()),
        ],
    );
    finish(
        model,
        deps.storage,
        "deposit_contribution",
        vec![
            ("member", info.sender.to_string()),
            ("cycle", target.to_string()),
            ("amount", paid.to_string()),
            ("on_time", on_time.to_string()),
        ],
    )
}

fn tick(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = Model::load(deps.storage, &env, circle_id)?;
    let before_status = model.circle.circle_status.clone();
    let settled = model.sync(MAX_ROUNDS_PER_TX)?;
    if settled == 0 && model.circle.circle_status == before_status && !model.fx.started_now {
        return Err(ContractError::NothingToDo {
            next_action_at: model.next_action_at().map(|t| t.seconds()).unwrap_or(0),
        });
    }
    finish(model, deps.storage, "tick", vec![])
}

fn withdraw(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    let pending = PENDING_PAYOUTS
        .may_load(deps.storage, (circle_id, info.sender.clone()))?
        .unwrap_or_default();
    if pending.is_zero() {
        return Err(ContractError::NoPendingPayouts {});
    }
    PENDING_PAYOUTS.remove(deps.storage, (circle_id, info.sender.clone()));
    model.circle.total_pending_payouts =
        model.circle.total_pending_payouts.saturating_sub(pending);
    // If this send fails again, `reply` credits it back to the pending ledger.
    model.push_send(&info.sender, pending);
    if model.circle.circle_status == CircleStatus::Finalizing
        && model.circle.total_pending_payouts.is_zero()
    {
        model.circle.circle_status = CircleStatus::Completed;
    }
    model.log(
        "withdrawal",
        format!("Member {} withdrew {}", info.sender, pending),
    );
    finish(
        model,
        deps.storage,
        "withdraw",
        vec![("member", info.sender.to_string()), ("amount", pending.to_string())],
    )
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

fn eject_member(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    if !model.circle.manual_eject_enabled() {
        return Err(ContractError::Unauthorized {
            msg: "Manual eject is disabled (ejection_policy is auto_only)".to_string(),
        });
    }
    require_running(&model)?;
    require_caught_up(&model)?;
    let member = deps.api.addr_validate(member_address.as_str())?;
    if member == info.sender {
        return Err(invalid("You cannot eject yourself; use ExitCircle"));
    }
    let i = model
        .index_of(&member)
        .filter(|&i| model.members[i].is_active())
        .ok_or_else(|| invalid("Member is not in the active roster"))?;
    // For cause only (PLAN_V5 3.10): a strike or arrears.
    if model.members[i].strikes == 0 && model.members[i].arrears.is_zero() {
        return Err(ContractError::NoCause {
            member: member.to_string(),
        });
    }
    let settled = model.circle.rounds_settled;
    let by = info.sender.to_string();
    let plan = model.leave(i, settled + 1, settled, "admin", MemberStatus::Ejected, &by, false);
    if model.active_count() < 2 {
        model.close(CircleStatus::Ended, "not_enough_active_members", None)?;
    }
    finish(
        model,
        deps.storage,
        "eject_member",
        vec![
            ("member", member.to_string()),
            ("ejected_by", info.sender.to_string()),
            ("served", plan.served.to_string()),
            ("claim", plan.claim.to_string()),
        ],
    )
}

fn set_co_admin(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    co_admin_address: Addr,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    let new_admin = deps.api.addr_validate(co_admin_address.as_str())?;
    if new_admin == model.circle.creator_address {
        return Err(invalid("Co-admin cannot be the creator"));
    }
    if !model.circle.members_list.contains(&new_admin) {
        return Err(invalid("Co-admin must be an active member"));
    }
    require_admin(&model, &info.sender)?;
    model.circle.co_admin_address = Some(new_admin.clone());
    model.log(
        "co_admin_set",
        format!("Co-admin set to {} by {}", new_admin, info.sender),
    );
    finish(
        model,
        deps.storage,
        "set_co_admin",
        vec![("co_admin", new_admin.to_string())],
    )
}

fn clear_co_admin(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    if model.circle.co_admin_address.is_none() {
        return Err(invalid("No co-admin is set"));
    }
    model.circle.co_admin_address = None;
    model.log("co_admin_cleared", format!("Co-admin cleared by {}", info.sender));
    finish(model, deps.storage, "clear_co_admin", vec![])
}

fn end_cycle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    require_running(&model)?;
    require_caught_up(&model)?;
    // PLAN_V5 3.8: never a confiscation. Before the first distribution of the
    // cycle, end now (contributions of the cycle come back); otherwise the
    // request means "do not start another cycle" and can be withdrawn.
    if model.end_now_allowed() {
        model.close(CircleStatus::Ended, "admin_end_now", None)?;
        return finish(
            model,
            deps.storage,
            "end_cycle",
            vec![("mode", "immediate".to_string())],
        );
    }
    let cycle = model.current_cycle();
    let by = info.sender.to_string();
    if model.circle.end_after_cycle == Some(cycle) {
        model.circle.end_after_cycle = None;
        model.log(
            "end_cancelled",
            format!("End after cycle {} withdrawn by {}", cycle, by),
        );
        model.emit(
            "safrimba_end_cancelled",
            vec![("after_cycle", cycle.to_string()), ("by", by)],
        );
        finish(
            model,
            deps.storage,
            "end_cycle",
            vec![("mode", "cancelled".to_string())],
        )
    } else {
        model.circle.end_after_cycle = Some(cycle);
        model.log(
            "end_requested",
            format!("Circle will stop after cycle {} (requested by {})", cycle, by),
        );
        model.emit(
            "safrimba_end_requested",
            vec![("after_cycle", cycle.to_string()), ("by", by)],
        );
        finish(
            model,
            deps.storage,
            "end_cycle",
            vec![
                ("mode", "after_cycle".to_string()),
                ("after_cycle", cycle.to_string()),
            ],
        )
    }
}

fn pause(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    reason: Option<String>,
    emergency: bool,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    if emergency && !model.circle.emergency_stop_enabled {
        return Err(invalid("Emergency stop not enabled"));
    }
    require_admin(&model, &info.sender)?;
    if model.circle.circle_status != CircleStatus::Running {
        return Err(status_err("Running", &model.circle));
    }
    require_caught_up(&model)?;
    let reason = reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| {
            if emergency { "emergency" } else { "unspecified" }.to_string()
        });
    if reason.chars().count() > MAX_PAUSE_REASON_LEN {
        return Err(invalid(format!(
            "pause reason is limited to {} characters",
            MAX_PAUSE_REASON_LEN
        )));
    }
    // PLAN_V5 3.9: bounded pause, total budget of two maximal pauses.
    let clock = model
        .clock()
        .ok_or(ContractError::CircleNotStarted { circle_id })?;
    if clock.pause_limit() == 0 {
        return Err(ContractError::PauseBudgetExhausted {});
    }
    model.begin_pause(reason.clone(), emergency);
    let until = model
        .clock()
        .and_then(|c| c.paused_until())
        .unwrap_or_default();
    finish(
        model,
        deps.storage,
        if emergency { "emergency_stop" } else { "pause_circle" },
        vec![
            ("reason", reason),
            ("paused_until", until.to_string()),
            ("by", info.sender.to_string()),
        ],
    )
}

fn unpause(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    if model.circle.circle_status != CircleStatus::Paused {
        return Err(status_err("Paused", &model.circle));
    }
    require_caught_up(&model)?;
    let clock = model
        .clock()
        .ok_or(ContractError::CircleNotStarted { circle_id })?;
    let paused = clock
        .paused_secs_at(env.block.time.seconds())
        .saturating_sub(clock.paused_total);
    model.end_pause(paused, "admin_resume");
    model.sync(MAX_ROUNDS_PER_TX)?;
    finish(
        model,
        deps.storage,
        "unpause_circle",
        vec![("paused_secs", paused.to_string()), ("by", info.sender.to_string())],
    )
}

fn cancel_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    // PLAN_V5 C12: only the creator (still a member) can cancel.
    let creator = model.circle.creator_address.clone();
    let creator_active = model.member(&creator).map(|m| m.is_active()).unwrap_or(false);
    if info.sender != creator || !creator_active {
        return Err(ContractError::Unauthorized {
            msg: "Only the creator can cancel the circle".to_string(),
        });
    }
    if matches!(
        model.circle.circle_status,
        CircleStatus::Running | CircleStatus::Paused
    ) {
        require_caught_up(&model)?;
    }
    match model.circle.circle_status {
        CircleStatus::Draft | CircleStatus::Open | CircleStatus::Full => {
            model.cancel_before_start("cancelled_by_admin")?
        }
        CircleStatus::Running | CircleStatus::Paused => model.cancel_running()?,
        _ => return Err(status_err("Draft, Open, Full, Running or Paused", &model.circle)),
    }
    finish(model, deps.storage, "cancel_circle", vec![])
}

fn update_circle(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    circle_name: Option<String>,
    circle_description: Option<String>,
    circle_image: Option<String>,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    if matches!(
        model.circle.circle_status,
        CircleStatus::Running
            | CircleStatus::Paused
            | CircleStatus::Finalizing
            | CircleStatus::Completed
            | CircleStatus::Ended
    ) {
        return Err(status_err("Draft, Open or Full", &model.circle));
    }
    if let Some(n) = circle_name {
        if n.trim().is_empty() || n.len() > 128 {
            return Err(invalid("circle_name must be 1-128 characters"));
        }
        model.circle.circle_name = n;
    }
    if let Some(d) = circle_description {
        model.circle.circle_description = d;
    }
    if let Some(img) = circle_image {
        model.circle.circle_image = Some(img);
    }
    finish(model, deps.storage, "update_circle", vec![])
}

fn sweep_dust(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    if !model.circle.is_terminal() {
        return Err(status_err("Completed, Cancelled or Ended", &model.circle));
    }
    let denom = model.circle.denomination.clone();
    let bank = deps
        .querier
        .query_balance(&env.contract.address, &denom)?
        .amount;
    let residual = bank.saturating_sub(model.obligations());
    if residual.is_zero() {
        return Err(ContractError::NothingToDo { next_action_at: 0 });
    }
    let platform = model.platform.platform_address.clone();
    model.push_send(&platform, residual);
    model.log(
        "dust_swept",
        format!("Swept {}{} of stray funds to {}", residual, denom, platform),
    );
    finish(
        model,
        deps.storage,
        "sweep_dust",
        vec![("amount", residual.to_string())],
    )
}

fn deposit_creator_reward(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    if !matches!(
        model.circle.circle_status,
        CircleStatus::Completed | CircleStatus::Ended | CircleStatus::Finalizing
    ) {
        return Err(status_err("Completed or Ended", &model.circle));
    }
    if CREATOR_REWARDS_CREDITED
        .may_load(deps.storage, circle_id)?
        .is_some()
    {
        return Err(invalid("Creator reward already credited for this circle"));
    }
    let amount = must_pay(&info, &model.circle.denomination).map_err(|_| {
        ContractError::InsufficientFunds {
            required: "non-zero reward in circle denomination".to_string(),
            sent: "0".to_string(),
        }
    })?;
    CREATOR_REWARDS_CREDITED.save(deps.storage, circle_id, &amount)?;
    let creator = model.circle.creator_address.clone();
    model.push_send(&creator, amount);
    model.log(
        "creator_reward_credited",
        format!("{} reward sent to creator {} by {}", amount, creator, info.sender),
    );
    finish(
        model,
        deps.storage,
        "deposit_creator_reward",
        vec![("creator", creator.to_string()), ("amount", amount.to_string())],
    )
}

fn update_member_pseudonym(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
    pseudonym: String,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    let model = Model::load(deps.storage, &env, circle_id)?;
    require_admin(&model, &info.sender)?;
    let addr = deps.api.addr_validate(member_address.as_str())?;
    if model.index_of(&addr).is_none() && !model.circle.pending_members.contains(&addr) {
        return Err(invalid(
            "Address not found in circle members or pending invitations",
        ));
    }
    if pseudonym.len() > 64 {
        return Err(invalid("pseudonym is limited to 64 characters"));
    }
    MEMBER_PSEUDONYMS.save(deps.storage, (circle_id, addr.clone()), &pseudonym)?;
    append_log(
        deps.storage,
        circle_id,
        "member_pseudonym_updated",
        format!("Pseudonym '{}' set for {}", pseudonym, addr),
        env.block.time,
    )?;
    Ok(Response::new()
        .add_attribute("action", "update_member_pseudonym")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("member", addr)
        .add_attribute("pseudonym", pseudonym))
}

fn rate_member(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
    member_address: Addr,
    score: u8,
    note: Option<String>,
) -> Result<Response, ContractError> {
    nonpayable(&info)?;
    if !(1..=5).contains(&score) {
        return Err(invalid("score must be between 1 and 5"));
    }
    let model = Model::load(deps.storage, &env, circle_id)?;
    let ratee = deps.api.addr_validate(member_address.as_str())?;
    let left = model
        .member(&ratee)
        .map(|m| !m.is_active())
        .unwrap_or(false);
    if !left {
        return Err(invalid("Can only rate members who were ejected or left"));
    }
    let is_admin = require_admin(&model, &info.sender).is_ok();
    let is_active = model
        .member(&info.sender)
        .map(|m| m.is_active())
        .unwrap_or(false);
    if !is_admin && !is_active {
        return Err(ContractError::Unauthorized {
            msg: "Only creator, co-admin, or active members can rate".to_string(),
        });
    }
    if info.sender == ratee {
        return Err(invalid("Cannot rate yourself"));
    }
    if MEMBER_RATINGS
        .may_load(deps.storage, (circle_id, ratee.clone(), info.sender.clone()))?
        .is_some()
    {
        return Err(invalid("Already rated this member for this circle"));
    }
    if note.as_ref().map(|n| n.len() > 280).unwrap_or(false) {
        return Err(invalid("note is limited to 280 characters"));
    }
    MEMBER_RATINGS.save(
        deps.storage,
        (circle_id, ratee.clone(), info.sender.clone()),
        &MemberRating {
            circle_id,
            ratee: ratee.clone(),
            rater: info.sender.clone(),
            score,
            note,
            timestamp: env.block.time,
        },
    )?;
    let count = WALLET_RATING_COUNT
        .may_load(deps.storage, ratee.clone())?
        .unwrap_or(0);
    WALLET_RATING_INDEX.save(
        deps.storage,
        (ratee.clone(), count),
        &(circle_id, info.sender.clone()),
    )?;
    WALLET_RATING_COUNT.save(deps.storage, ratee.clone(), &(count + 1))?;
    append_log(
        deps.storage,
        circle_id,
        "member_rated",
        format!("Member {} rated {} by {}", ratee, score, info.sender),
        env.block.time,
    )?;
    Ok(Response::new()
        .add_attribute("action", "rate_member")
        .add_attribute("circle_id", circle_id.to_string())
        .add_attribute("ratee", ratee)
        .add_attribute("score", score.to_string()))
}

/// Repay arrears at any time (PLAN_V5 C10): after the last round, or after
/// leaving. For active members the rest refills the lock. Recovered arrears go
/// to the common pool (shared at the end), exactly like netting at a payout.
fn repay_arrears(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    circle_id: u64,
) -> Result<Response, ContractError> {
    let mut model = load_synced(deps.storage, &env, circle_id)?;
    require_running(&model)?;
    require_caught_up(&model)?;
    let i = model
        .index_of(&info.sender)
        .ok_or_else(|| ContractError::Unauthorized {
            msg: "Not a member of this circle".to_string(),
        })?;
    let (arrears, refill, active) = {
        let m = &model.members[i];
        let refill = if m.is_active() { m.refill_needed() } else { Uint128::zero() };
        (m.arrears, refill, m.is_active())
    };
    let max = arrears + refill;
    if max.is_zero() {
        return Err(ContractError::NothingToPay {});
    }
    let denom = model.circle.denomination.clone();
    let paid = must_pay(&info, &denom).map_err(|_| ContractError::InvalidPayment {
        min: format!("1{}", denom),
        max: format!("{}{}", max, denom),
        sent: "0".to_string(),
    })?;
    if paid > max {
        return Err(ContractError::InvalidPayment {
            min: format!("1{}", denom),
            max: format!("{}{}", max, denom),
            sent: format!("{}{}", paid, denom),
        });
    }
    let to_arrears = paid.min(arrears);
    let to_lock = paid - to_arrears;
    {
        let m = model.m(i);
        m.arrears -= to_arrears;
        m.lock += to_lock;
        m.extra_paid += paid;
        if active && m.arrears.is_zero() && m.lock >= m.lock_original {
            m.strikes = 0;
        }
    }
    model.circle.common_pool += to_arrears;
    model.circle.total_arrears = model.circle.total_arrears.saturating_sub(to_arrears);
    model.log(
        "arrears_repaid",
        format!(
            "Member {} repaid {} (arrears {}, lock refill {})",
            info.sender, paid, to_arrears, to_lock
        ),
    );
    model.emit(
        "safrimba_arrears_repaid",
        vec![
            ("member", info.sender.to_string()),
            ("amount", to_arrears.to_string()),
            ("lock_refill", to_lock.to_string()),
        ],
    );
    finish(
        model,
        deps.storage,
        "repay_arrears",
        vec![
            ("member", info.sender.to_string()),
            ("amount", paid.to_string()),
        ],
    )
}
