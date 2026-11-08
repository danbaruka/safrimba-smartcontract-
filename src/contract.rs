use cosmwasm_std::{
    entry_point, to_json_binary, Binary, Deps, DepsMut, Env, Event, MessageInfo, Reply,
    Response, StdResult, SubMsgResult, Uint128,
};
use cw2::{get_contract_version, set_contract_version};

use crate::error::ContractError;
use crate::msg::{ContractVersionResponse, ExecuteMsg, InstantiateMsg, MigrateMsg, QueryMsg};
use crate::query::*;
use crate::state::{
    CircleStatus, PlatformConfig, CIRCLES, PENDING_PAYOUTS, PLATFORM_CONFIG, SEND_BATCH,
};

const CONTRACT_NAME: &str = "crates.io:safrimba-contract";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 5 = time-driven engine: GetCircleView, Tick, pushed payouts, lazy settlement.
pub const CONTRACT_API_VERSION: u8 = 5;
/// Platform fee hard cap (basis points).
const MAX_PLATFORM_FEE_BPS: u64 = 2_000;

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    if msg.platform_fee_percent > MAX_PLATFORM_FEE_BPS {
        return Err(ContractError::InvalidParameters {
            msg: format!("platform_fee_percent cannot exceed {}", MAX_PLATFORM_FEE_BPS),
        });
    }
    let platform_address = deps.api.addr_validate(msg.platform_address.as_str())?;
    PLATFORM_CONFIG.save(
        deps.storage,
        &PlatformConfig {
            platform_fee_percent: msg.platform_fee_percent,
            platform_address: platform_address.clone(),
        },
    )?;
    Ok(Response::new()
        .add_attribute("method", "instantiate")
        .add_attribute("engine", crate::state::ENGINE_VERSION.to_string())
        .add_attribute("platform_address", platform_address.to_string())
        .add_attribute("platform_fee_percent", msg.platform_fee_percent.to_string()))
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    crate::execute::execute(deps, env, info, msg)
}

/// Every bank send is a sub-message with `reply_on_error` (PLAN_V5 C5): a send
/// the chain refuses (blocked module account, …) must not revert the settlement
/// of everyone else. The amount stays in the contract, credited to the
/// recipient's pending ledger, and is released by `Withdraw`.
#[entry_point]
pub fn reply(deps: DepsMut, _env: Env, msg: Reply) -> Result<Response, ContractError> {
    let SubMsgResult::Err(error) = msg.result else {
        return Ok(Response::new());
    };
    let batch = SEND_BATCH.load(deps.storage)?;
    let (to, amount) = batch
        .sends
        .get(msg.id as usize)
        .cloned()
        .ok_or_else(|| ContractError::InvalidParameters {
            msg: format!("unknown send reply id {}", msg.id),
        })?;
    PENDING_PAYOUTS.update(deps.storage, (batch.circle_id, to.clone()), |v| -> StdResult<Uint128> {
        Ok(v.unwrap_or_default() + amount)
    })?;
    let mut circle = CIRCLES.load(deps.storage, batch.circle_id)?;
    circle.total_pending_payouts += amount;
    // A completed circle that still owes a withdrawal is Finalizing until
    // `Withdraw` releases it (same as when the debt existed before closing).
    if circle.circle_status == CircleStatus::Completed {
        circle.circle_status = CircleStatus::Finalizing;
    }
    CIRCLES.save(deps.storage, batch.circle_id, &circle)?;
    let error: String = error.chars().take(200).collect();
    Ok(Response::new()
        .add_attribute("action", "payout_deferred")
        .add_event(
            Event::new("safrimba_payout_deferred")
                .add_attribute("circle_id", batch.circle_id.to_string())
                .add_attribute("to", to.to_string())
                .add_attribute("amount", amount.to_string())
                .add_attribute("denom", batch.denom)
                .add_attribute("error", error),
        ))
}

/// Same-engine upgrades only. A v4 instance keeps its v4 state layout and must
/// stay on the v4 code id (the keeper drives both engines): converting a live
/// v4 ledger in place would silently re-interpret members' money.
#[entry_point]
pub fn migrate(deps: DepsMut, _env: Env, _msg: MigrateMsg) -> Result<Response, ContractError> {
    let previous = get_contract_version(deps.storage)?;
    let major_minor = |v: &str| -> (u64, u64) {
        let mut it = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        (it.next().unwrap_or(0), it.next().unwrap_or(0))
    };
    if previous.contract != CONTRACT_NAME {
        return Err(ContractError::InvalidParameters {
            msg: format!("cannot migrate from {}", previous.contract),
        });
    }
    if major_minor(&previous.version) < (0, 5) {
        return Err(ContractError::InvalidParameters {
            msg: format!(
                "in-place migration from engine v4 ({}) is not supported; keep v4 circles on their code id",
                previous.version
            ),
        });
    }
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    Ok(Response::new()
        .add_attribute("action", "migrate")
        .add_attribute("previous_version", previous.version)
        .add_attribute("new_version", CONTRACT_VERSION))
}

#[entry_point]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::GetCircleView { circle_id, member } => {
            to_json_binary(&query_circle_view(deps, env, circle_id, member)?)
        }
        QueryMsg::GetCircle { circle_id } => to_json_binary(&query_circle(deps, env, circle_id)?),
        QueryMsg::GetCircles {
            start_after,
            limit,
            status,
            creator,
        } => to_json_binary(&query_circles(deps, env, start_after, limit, status, creator)?),
        QueryMsg::GetCircleMembers { circle_id } => {
            to_json_binary(&query_circle_members(deps, env, circle_id)?)
        }
        QueryMsg::GetCircleStatus { circle_id } => {
            to_json_binary(&query_circle_status(deps, env, circle_id)?)
        }
        QueryMsg::GetCurrentCycle { circle_id } => {
            to_json_binary(&query_current_cycle(deps, env, circle_id)?)
        }
        QueryMsg::GetCycleDeposits { circle_id, cycle } => {
            to_json_binary(&query_cycle_deposits(deps, env, circle_id, cycle)?)
        }
        QueryMsg::GetMemberDeposits { circle_id, member } => {
            to_json_binary(&query_member_deposits(deps, env, circle_id, member)?)
        }
        QueryMsg::GetPayouts { circle_id } => to_json_binary(&query_payouts(deps, env, circle_id)?),
        QueryMsg::GetPayoutHistory { circle_id, cycle } => {
            to_json_binary(&query_payout_history(deps, env, circle_id, cycle)?)
        }
        QueryMsg::GetCircleBalance { circle_id } => {
            to_json_binary(&query_circle_balance(deps, env, circle_id)?)
        }
        QueryMsg::GetMemberBalance { circle_id, member } => {
            to_json_binary(&query_member_balance(deps, env, circle_id, member)?)
        }
        QueryMsg::GetPenalties { circle_id, member } => {
            to_json_binary(&query_penalties(deps, env, circle_id, member)?)
        }
        QueryMsg::GetRefunds { circle_id } => to_json_binary(&query_refunds(deps, env, circle_id)?),
        QueryMsg::GetPendingPayout { circle_id, member } => {
            to_json_binary(&query_pending_payout(deps, env, circle_id, member)?)
        }
        QueryMsg::GetMemberAccumulatedLateFees { circle_id, member } => to_json_binary(
            &query_member_accumulated_late_fees(deps, env, circle_id, member)?,
        ),
        QueryMsg::GetDepositRequirement { circle_id, member } => {
            to_json_binary(&query_deposit_requirement(deps, env, circle_id, member)?)
        }
        QueryMsg::GetEvents { circle_id, limit } => {
            to_json_binary(&query_events(deps, env, circle_id, limit)?)
        }
        QueryMsg::GetCircleStats { circle_id } => {
            to_json_binary(&query_circle_stats(deps, env, circle_id)?)
        }
        QueryMsg::GetMemberStats { circle_id, member } => {
            to_json_binary(&query_member_stats(deps, env, circle_id, member)?)
        }
        QueryMsg::GetMemberLockedAmount { circle_id, member } => {
            to_json_binary(&query_member_locked_amount(deps, env, circle_id, member)?)
        }
        QueryMsg::GetBlockedMembers { circle_id } => {
            to_json_binary(&query_blocked_members(deps, env, circle_id)?)
        }
        QueryMsg::GetMemberPseudonym { circle_id, member } => {
            to_json_binary(&query_member_pseudonym(deps, env, circle_id, member)?)
        }
        QueryMsg::GetPrivateMembers { circle_id } => {
            to_json_binary(&query_private_members(deps, env, circle_id)?)
        }
        QueryMsg::GetDistributionCalendar { circle_id } => {
            to_json_binary(&query_distribution_calendar(deps, env, circle_id)?)
        }
        QueryMsg::GetArchivedDate { circle_id } => {
            to_json_binary(&query_archived_date(deps, env, circle_id)?)
        }
        QueryMsg::GetLockReserve { circle_id, member } => {
            to_json_binary(&query_lock_reserve(deps, env, circle_id, member)?)
        }
        QueryMsg::GetLockReserves { circle_id } => {
            to_json_binary(&query_lock_reserves(deps, env, circle_id)?)
        }
        QueryMsg::PreviewEndCycle { circle_id } => {
            to_json_binary(&query_preview_end_cycle(deps, env, circle_id)?)
        }
        QueryMsg::GetWalletReputation { address } => {
            to_json_binary(&query_wallet_reputation(deps, env, address)?)
        }
        QueryMsg::GetContractVersion {} => to_json_binary(&ContractVersionResponse {
            api_version: CONTRACT_API_VERSION,
        }),
    }
}
