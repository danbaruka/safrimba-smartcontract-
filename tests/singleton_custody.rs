#![cfg(not(target_arch = "wasm32"))]

use cosmwasm_std::{Addr, Coin, Empty, Uint128};
use cw_multi_test::{App, Contract, ContractWrapper, Executor};

use safrimba_contract::msg::{ExecuteMsg, InstantiateMsg, QueryMsg};
use safrimba_contract::state::{CircleStatus, PayoutOrderType, Visibility};

fn contract() -> Box<dyn Contract<Empty>> {
    let contract = ContractWrapper::new(
        safrimba_contract::contract::execute,
        safrimba_contract::contract::instantiate,
        safrimba_contract::contract::query,
    );
    Box::new(contract)
}

fn usaf(amount: u128) -> Coin {
    Coin {
        denom: "usaf".to_string(),
        amount: Uint128::new(amount),
    }
}

/// Smoke: instantiate → create one circle → refuse second CreateCircle (singleton escrow).
#[test]
fn singleton_create_circle_blocks_co_tenancy() {
    let mut app = App::default();
    let creator = Addr::unchecked("addr_safro_creator");
    let platform = Addr::unchecked("addr_safro_platform");

    app.init_modules(|router, _, storage| {
        router
            .bank
            .init_balance(storage, &creator, vec![usaf(1_000_000_000_000)])
            .unwrap();
    });

    let code_id = app.store_code(contract());
    let contract_addr = app
        .instantiate_contract(
            code_id,
            creator.clone(),
            &InstantiateMsg {
                platform_address: platform,
                platform_fee_percent: 100,
            },
            &[],
            "safrimba",
            None,
        )
        .unwrap();

    let contrib = Uint128::new(1_000_000);
    // creator lock = contribution * 2
    let creator_lock = Uint128::new(2_000_000);

    let create = ExecuteMsg::CreateCircle {
        circle_name: "A".to_string(),
        circle_description: "d".to_string(),
        circle_image: None,
        max_members: 2,
        min_members_required: 2,
        invite_only: true,
        contribution_amount: contrib,
        denomination: Some("usaf".to_string()),
        exit_penalty_percent: 2000,
        late_fee_percent: 500,
        total_cycles: 1,
        cycle_duration_days: 1,
        cycle_duration_seconds: Some(3600),
        start_date: None,
        grace_period_hours: 1,
        grace_period_seconds: Some(600),
        auto_start_when_full: false,
        auto_start_type: None,
        auto_start_date: None,
        payout_order_type: PayoutOrderType::RandomOrder,
        payout_order_list: None,
        auto_payout_enabled: true,
        manual_trigger_enabled: false,
        emergency_stop_enabled: false,
        auto_refund_if_min_not_met: true,
        strict_mode: false,
        visibility: Visibility::Private,
        show_member_identities: true,
        distribution_threshold: None,
        ejection_policy: None,
        co_admin_address: None,
    };

    app.execute_contract(
        creator.clone(),
        contract_addr.clone(),
        &create,
        &[usaf(creator_lock.u128())],
    )
    .unwrap();

    let err = app
        .execute_contract(
            creator.clone(),
            contract_addr.clone(),
            &create,
            &[usaf(creator_lock.u128())],
        )
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already hosts a circle") || msg.contains("Instantiate a new contract"),
        "unexpected error: {msg}"
    );

    let status: safrimba_contract::msg::CircleResponse = app
        .wrap()
        .query_wasm_smart(contract_addr, &QueryMsg::GetCircle { circle_id: 1 })
        .unwrap();
    assert!(matches!(
        status.circle.circle_status,
        CircleStatus::Draft | CircleStatus::Open
    ));
}
