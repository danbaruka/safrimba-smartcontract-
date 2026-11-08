#![cfg(not(target_arch = "wasm32"))]
//! End-to-end lifecycle tests for the v5 engine (cw-multi-test).
//!
//! Every scenario checks the money invariant after each step:
//!   bank(contract) == Σ locks + carry_pot + common_pool
//!                   + paid-but-unsettled contributions + deferred (pending) payouts
//! and that a finished circle holds nothing but deferred payouts.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use anyhow::bail;
use cosmwasm_std::{
    Addr, Api, BankMsg, BankQuery, Binary, BlockInfo, Coin, CustomMsg, CustomQuery, Empty, Event,
    Querier, Storage, Timestamp, Uint128,
};
use cw_multi_test::{
    App, AppBuilder, AppResponse, Bank, BankKeeper, BankSudo, Contract, ContractWrapper,
    CosmosRouter, Executor, Module,
};
use serde::de::DeserializeOwned;

use safrimba_contract::msg::{
    CalendarRound, CircleViewResponse, DistributionCalendarResponse, ExecuteMsg, ExitQuote,
    InstantiateMsg, QueryMsg, RoundKind, RoundPhase,
};
use safrimba_contract::state::{
    CircleStatus, DistributionThreshold, EjectionPolicy, MemberStatus, PayoutOrderType, Visibility,
};

const DENOM: &str = "usaf";
const C: u128 = 1_000_000; // 1 SAF
const ROUND: u64 = 3_600;
const GRACE: u64 = 600;
const FUND: u128 = 1_000_000_000_000;

// ---------------------------------------------------------------------------
// A bank that can refuse to credit given addresses (like the SDK's blocked
// module accounts), to exercise deferred payouts.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct BlockingBank {
    inner: BankKeeper,
    blocked: Rc<RefCell<HashSet<String>>>,
}

impl Bank for BlockingBank {}

impl Module for BlockingBank {
    type ExecT = BankMsg;
    type QueryT = BankQuery;
    type SudoT = BankSudo;

    fn execute<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        sender: Addr,
        msg: BankMsg,
    ) -> anyhow::Result<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        if let BankMsg::Send { to_address, .. } = &msg {
            if self.blocked.borrow().contains(to_address) {
                bail!("{} is not allowed to receive funds", to_address);
            }
        }
        self.inner.execute(api, storage, router, block, sender, msg)
    }

    fn query(
        &self,
        api: &dyn Api,
        storage: &dyn Storage,
        querier: &dyn Querier,
        block: &BlockInfo,
        request: BankQuery,
    ) -> anyhow::Result<Binary> {
        self.inner.query(api, storage, querier, block, request)
    }

    fn sudo<ExecC, QueryC>(
        &self,
        api: &dyn Api,
        storage: &mut dyn Storage,
        router: &dyn CosmosRouter<ExecC = ExecC, QueryC = QueryC>,
        block: &BlockInfo,
        msg: BankSudo,
    ) -> anyhow::Result<AppResponse>
    where
        ExecC: CustomMsg + DeserializeOwned + 'static,
        QueryC: CustomQuery + DeserializeOwned + 'static,
    {
        self.inner.sudo(api, storage, router, block, msg)
    }
}

type TestApp = App<BlockingBank>;

fn contract() -> Box<dyn Contract<Empty>> {
    Box::new(
        ContractWrapper::new(
            safrimba_contract::contract::execute,
            safrimba_contract::contract::instantiate,
            safrimba_contract::contract::query,
        )
        .with_reply(safrimba_contract::contract::reply),
    )
}

fn coin(amount: u128) -> Coin {
    Coin {
        denom: DENOM.to_string(),
        amount: Uint128::new(amount),
    }
}

#[derive(Clone)]
struct Cfg {
    members: usize, // including creator
    cycles: u32,
    threshold: Option<DistributionThreshold>,
    min_members: u32,
    policy: EjectionPolicy,
    exit_bps: u64,
    late_bps: u64,
    platform_bps: u64,
    strict: bool,
    auto_start: Option<(&'static str, u64)>, // (type, seconds from now)
    auto_refund: bool,
    order: PayoutOrderType,
    co_admin: Option<usize>,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            members: 3,
            cycles: 1,
            threshold: Some(DistributionThreshold::MinMembers { count: 1 }),
            min_members: 2,
            policy: EjectionPolicy::AutoAndAdmin,
            exit_bps: 2_000,
            late_bps: 1_000,
            platform_bps: 0,
            strict: false,
            auto_start: None,
            auto_refund: false,
            order: PayoutOrderType::PredefinedOrder,
            co_admin: None,
        }
    }
}

struct H {
    app: TestApp,
    blocked: Rc<RefCell<HashSet<String>>>,
    contract: Addr,
    platform: Addr,
    /// index 0 = creator
    who: Vec<Addr>,
    cfg: Cfg,
    /// Every event of every successful transaction.
    events: Vec<Event>,
}

impl H {
    fn new(cfg: Cfg) -> H {
        let who: Vec<Addr> = (0..cfg.members.max(2) + 2)
            .map(|i| Addr::unchecked(format!("addr_safro_member{:02}", i)))
            .collect();
        let platform = Addr::unchecked("addr_safro_platform");
        let blocked = Rc::new(RefCell::new(HashSet::new()));
        let bank = BlockingBank {
            inner: BankKeeper::new(),
            blocked: blocked.clone(),
        };
        let mut app: TestApp = AppBuilder::new().with_bank(bank).build(|router, _, storage| {
            for a in &who {
                router
                    .bank
                    .inner
                    .init_balance(storage, a, vec![coin(FUND)])
                    .unwrap();
            }
        });
        let code_id = app.store_code(contract());
        let contract = app
            .instantiate_contract(
                code_id,
                who[0].clone(),
                &InstantiateMsg {
                    platform_fee_percent: cfg.platform_bps,
                    platform_address: platform.clone(),
                },
                &[],
                "safrimba",
                None,
            )
            .unwrap();
        let now = app.block_info().time;
        let (auto_type, auto_date) = match cfg.auto_start {
            Some((t, secs)) => (Some(t.to_string()), Some(now.plus_seconds(secs))),
            None => (None, None),
        };
        let create = ExecuteMsg::CreateCircle {
            circle_name: "Tontine".to_string(),
            circle_description: "test".to_string(),
            circle_image: None,
            max_members: cfg.members as u32,
            min_members_required: cfg.min_members,
            invite_only: true,
            contribution_amount: Uint128::new(C),
            denomination: None,
            exit_penalty_percent: cfg.exit_bps,
            late_fee_percent: cfg.late_bps,
            total_cycles: cfg.cycles,
            cycle_duration_days: 0,
            cycle_duration_seconds: Some(ROUND),
            start_date: None,
            grace_period_hours: 0,
            grace_period_seconds: Some(GRACE),
            auto_start_when_full: matches!(cfg.auto_start, Some(("by_members", _))),
            auto_start_type: auto_type,
            auto_start_date: auto_date,
            payout_order_type: cfg.order.clone(),
            payout_order_list: None,
            auto_payout_enabled: true,
            manual_trigger_enabled: false,
            emergency_stop_enabled: true,
            auto_refund_if_min_not_met: cfg.auto_refund,
            strict_mode: cfg.strict,
            visibility: Visibility::Private,
            show_member_identities: true,
            distribution_threshold: cfg.threshold.clone(),
            ejection_policy: Some(cfg.policy.clone()),
            co_admin_address: cfg.co_admin.map(|i| who[i].clone()),
        };
        app.execute_contract(who[0].clone(), contract.clone(), &create, &[coin(2 * C)])
            .unwrap();
        H {
            app,
            blocked,
            contract,
            platform,
            who,
            cfg,
            events: vec![],
        }
    }

    fn exec(&mut self, i: usize, msg: &ExecuteMsg, funds: &[Coin]) -> anyhow::Result<AppResponse> {
        let sender = self.who[i].clone();
        self.exec_as(&sender, msg, funds)
    }

    fn exec_as(
        &mut self,
        sender: &Addr,
        msg: &ExecuteMsg,
        funds: &[Coin],
    ) -> anyhow::Result<AppResponse> {
        let r = self
            .app
            .execute_contract(sender.clone(), self.contract.clone(), msg, funds);
        if let Ok(res) = &r {
            self.events.extend(res.events.iter().cloned());
        }
        r
    }

    fn idx(&self, a: &Addr) -> usize {
        self.who.iter().position(|w| w == a).expect("known address")
    }

    fn invite_and_join(&mut self, i: usize) {
        if self.cfg.co_admin != Some(i) {
            let addr = self.who[i].clone();
            self.exec(
                0,
                &ExecuteMsg::InviteMember {
                    circle_id: 1,
                    member_address: addr,
                },
                &[],
            )
            .unwrap();
        }
        self.exec(i, &ExecuteMsg::JoinCircle { circle_id: 1 }, &[coin(C)])
            .unwrap();
    }

    fn join_all(&mut self) {
        for i in 1..self.cfg.members {
            self.invite_and_join(i);
        }
    }

    fn start(&mut self) {
        self.exec(0, &ExecuteMsg::StartCircle { circle_id: 1 }, &[])
            .unwrap();
    }

    fn now(&self) -> Timestamp {
        self.app.block_info().time
    }

    fn advance(&mut self, secs: u64) {
        self.app.update_block(|b| {
            b.time = b.time.plus_seconds(secs);
            b.height += secs / 5 + 1;
        });
    }

    fn advance_to(&mut self, t: Timestamp) {
        let now = self.now().seconds();
        if t.seconds() > now {
            self.advance(t.seconds() - now);
        }
    }

    fn tick(&mut self) -> anyhow::Result<AppResponse> {
        let keeper = self.who[self.cfg.members + 1].clone();
        self.exec_as(&keeper, &ExecuteMsg::Tick { circle_id: 1 }, &[])
    }

    fn try_tick(&mut self) {
        if let Err(e) = self.tick() {
            let s = format!("{} / {:?}", e.root_cause(), e.root_cause());
            assert!(
                s.contains("Nothing to settle") || s.contains("NothingToDo"),
                "tick failed: {}",
                s
            );
        }
    }

    /// Tick until the stored state has caught up with the clock.
    fn settle_all(&mut self) {
        for _ in 0..500 {
            let v = self.view(None);
            if v.rounds_behind == 0 && !v.tick_due {
                return;
            }
            self.try_tick();
        }
        panic!("could not catch up with the clock");
    }

    fn pay(&mut self, i: usize, amount: u128) -> anyhow::Result<AppResponse> {
        self.exec(
            i,
            &ExecuteMsg::DepositContribution { circle_id: 1 },
            &[coin(amount)],
        )
    }

    fn view(&self, member: Option<usize>) -> CircleViewResponse {
        self.app
            .wrap()
            .query_wasm_smart(
                &self.contract,
                &QueryMsg::GetCircleView {
                    circle_id: 1,
                    member: member.map(|i| self.who[i].clone()),
                },
            )
            .unwrap()
    }

    fn quote(&self, i: usize) -> ExitQuote {
        self.view(Some(i)).me.unwrap().exit_quote.unwrap()
    }

    /// Exit as quoted by the live view.
    fn exit(&mut self, i: usize) -> anyhow::Result<AppResponse> {
        let q = self.quote(i);
        let funds = if q.to_attach.is_zero() {
            vec![]
        } else {
            vec![coin(q.to_attach.u128())]
        };
        self.exec(i, &ExecuteMsg::ExitCircle { circle_id: 1 }, &funds)
    }

    fn pause(&mut self, i: usize, reason: &str) -> anyhow::Result<AppResponse> {
        self.exec(
            i,
            &ExecuteMsg::PauseCircle {
                circle_id: 1,
                reason: Some(reason.to_string()),
            },
            &[],
        )
    }

    fn eject(&mut self, by: usize, target: usize) -> anyhow::Result<AppResponse> {
        let member_address = self.who[target].clone();
        self.exec(
            by,
            &ExecuteMsg::EjectMember {
                circle_id: 1,
                member_address,
            },
            &[],
        )
    }

    fn calendar(&self) -> Vec<CalendarRound> {
        let r: DistributionCalendarResponse = self
            .app
            .wrap()
            .query_wasm_smart(
                &self.contract,
                &QueryMsg::GetDistributionCalendar { circle_id: 1 },
            )
            .unwrap();
        r.rounds
    }

    fn bank(&self, a: &Addr) -> u128 {
        self.app.wrap().query_balance(a, DENOM).unwrap().amount.u128()
    }

    fn contract_bank(&self) -> u128 {
        self.bank(&self.contract)
    }

    /// Materialize due transitions, then check bank == obligations.
    fn check(&mut self) {
        self.settle_all();
        let v = self.view(None);
        assert_eq!(v.rounds_behind, 0, "state must be fully settled after tick");
        let locks: u128 = v.members.iter().map(|m| m.lock.u128()).sum();
        let unsettled: u128 = v
            .members
            .iter()
            .filter(|m| m.slot.is_some())
            .map(|m| C * (m.paid_through.saturating_sub(v.rounds_settled)) as u128)
            .sum();
        let pending = v.pending_payouts_total.u128();
        let expected = locks
            + v.carry_pot.u128()
            + v.common_pool.u128()
            + v.comp_pool.u128()
            + unsettled
            + pending;
        assert_eq!(
            self.contract_bank(),
            expected,
            "money invariant broken: bank {} vs obligations {} (locks {}, carry {}, pool {}, comp {}, unsettled {}, pending {}) status {:?}",
            self.contract_bank(),
            expected,
            locks,
            v.carry_pot,
            v.common_pool,
            v.comp_pool,
            unsettled,
            pending,
            v.status
        );
        if terminal(&v.status) {
            if self.contract_bank() != pending {
                for m in &v.members {
                    eprintln!(
                        "  {} status {:?} slot {:?} lock {} paid_through {} claim {} arrears {}",
                        m.address, m.status, m.slot, m.lock, m.paid_through, m.claim, m.arrears
                    );
                }
                eprintln!(
                    "  settled {} carry {} pool {} comp {} pending {}",
                    v.rounds_settled, v.carry_pot, v.common_pool, v.comp_pool, pending
                );
            }
            assert_eq!(
                self.contract_bank(),
                pending,
                "closed circle must hold nothing but deferred payouts"
            );
        }
    }

    /// Everyone active pays the round they owe right now (minimum amount).
    fn everyone_pays(&mut self) {
        let v = self.view(None);
        for i in 0..self.cfg.members {
            let active = v
                .members
                .iter()
                .any(|m| m.address == self.who[i] && m.status == MemberStatus::Active);
            if active {
                let due = self.view(Some(i)).my_due.unwrap();
                if due.can_pay_now {
                    self.pay(i, C).unwrap();
                }
            }
        }
    }

    /// Everyone active pays except `skip`.
    fn everyone_pays_but(&mut self, skip: &[usize]) {
        let v = self.view(None);
        for i in 0..self.cfg.members {
            if skip.contains(&i) {
                continue;
            }
            let active = v
                .members
                .iter()
                .any(|m| m.address == self.who[i] && m.status == MemberStatus::Active);
            if active && self.view(Some(i)).my_due.unwrap().can_pay_now {
                self.pay(i, C).unwrap();
            }
        }
    }

    /// Move to the focus round's settlement time and check.
    fn next_round(&mut self) {
        if let Some(r) = self.view(None).round {
            self.advance_to(r.settles_at);
        }
        self.check();
    }

    fn total_supply(&self) -> u128 {
        let mut t = self.contract_bank() + self.bank(&self.platform);
        for a in &self.who {
            t += self.bank(a);
        }
        t
    }

    fn net(&self, i: usize) -> i128 {
        self.bank(&self.who[i]) as i128 - FUND as i128
    }

    fn events_of(&self, ty: &str) -> Vec<&Event> {
        let full = format!("wasm-{}", ty);
        self.events.iter().filter(|e| e.ty == full).collect()
    }
}

fn attr(e: &Event, key: &str) -> String {
    e.attributes
        .iter()
        .find(|a| a.key == key)
        .map(|a| a.value.clone())
        .unwrap_or_default()
}

fn terminal(s: &CircleStatus) -> bool {
    matches!(
        s,
        CircleStatus::Completed
            | CircleStatus::Cancelled
            | CircleStatus::Ended
            | CircleStatus::Finalizing
    )
}

fn status(h: &H) -> CircleStatus {
    h.view(None).status
}

fn err_text(r: anyhow::Result<AppResponse>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => format!("{} / {:?}", e.root_cause(), e.root_cause()),
    }
}

// ---------------------------------------------------------------------------
// Core schedule
// ---------------------------------------------------------------------------

#[test]
fn rotating_all_pay_completes_automatically_with_exact_timing() {
    let mut h = H::new(Cfg {
        cycles: 2,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    let supply = h.total_supply();
    let v = h.view(Some(1));
    assert_eq!(v.status, CircleStatus::Running);
    assert_eq!(v.total_rounds, 6);
    let r1 = v.round.clone().unwrap();
    assert_eq!(r1.phase, RoundPhase::Open);
    assert_eq!(r1.kind, RoundKind::Turn);
    assert_eq!(r1.settles_at.seconds() - r1.opens_at.seconds(), ROUND + GRACE);

    for round in 1..=6u32 {
        h.everyone_pays();
        h.check();
        let v = h.view(None);
        let r = v.round.clone().unwrap();
        assert_eq!(r.round, round);
        // One second before settlement: nothing to do.
        h.advance_to(r.settles_at.minus_seconds(1));
        assert!(h.tick().is_err(), "tick before settlement must be refused");
        h.advance(1);
        h.tick().unwrap();
        assert_eq!(h.view(None).rounds_settled, round);
        h.check();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    assert_eq!(h.contract_bank(), 0);
    assert_eq!(h.total_supply(), supply);
    // Fair rotation: everyone paid 6C and received 6C → back to initial balance.
    for i in 0..3 {
        assert_eq!(h.net(i), 0, "member {} should be whole", i);
    }
}

#[test]
fn total_mode_splits_cycle_pot_equally() {
    let mut h = H::new(Cfg {
        members: 4,
        threshold: Some(DistributionThreshold::Total {}),
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    for _ in 0..4 {
        h.everyone_pays();
        h.next_round();
    }
    let v = h.view(None);
    assert_eq!(v.status, CircleStatus::Completed);
    for i in 0..4 {
        assert_eq!(h.net(i), 0);
    }
}

#[test]
fn n_rounds_mode_pays_everyone_exactly_once_per_cycle() {
    let mut h = H::new(Cfg {
        members: 4,
        min_members: 4,
        cycles: 2,
        threshold: Some(DistributionThreshold::MinMembers { count: 2 }),
        platform_bps: 100,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    let cal = h.calendar();
    assert_eq!(cal.len(), 8);
    assert_eq!(cal[0].kind, Some(RoundKind::Accumulate));
    assert_eq!(cal[1].kind, Some(RoundKind::Turn));
    assert_eq!(cal[3].kind, Some(RoundKind::Group));
    assert_eq!(cal[3].recipients.len(), 2);
    for _ in 0..8 {
        h.everyone_pays();
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    // 1% platform fee on 2 × 4C received per member.
    let fee_each = (8 * C) / 100;
    for i in 0..4 {
        assert_eq!(h.net(i), -(fee_each as i128), "member {}", i);
    }
    assert_eq!(h.bank(&h.platform), 4 * fee_each);
}

#[test]
fn lazy_settlement_view_is_correct_without_any_transaction() {
    let mut h = H::new(Cfg {
        members: 3,
        cycles: 2,
        policy: EjectionPolicy::AdminOnly,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    // Keeper down: 4 rounds elapse with no transaction at all.
    h.advance(4 * ROUND + GRACE + 1);
    let v = h.view(None);
    assert_eq!(v.rounds_settled, 4, "view settles virtually");
    assert_eq!(v.rounds_behind, 4);
    assert!(v.tick_due);
    // One tick materializes all of it.
    h.tick().unwrap();
    let v = h.view(None);
    assert_eq!(v.rounds_settled, 4);
    assert_eq!(v.rounds_behind, 0);
    h.check();
}

#[test]
fn legacy_messages_are_aliases_of_tick() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    h.everyone_pays();
    let r = h.view(None).round.unwrap();
    h.advance_to(r.settles_at);
    h.exec(2, &ExecuteMsg::ProcessPayout { circle_id: 1 }, &[]).unwrap();
    assert_eq!(h.view(None).rounds_settled, 1);
    assert!(h.exec(2, &ExecuteMsg::AdvanceRound { circle_id: 1 }, &[]).is_err());
    assert!(h
        .exec(
            0,
            &ExecuteMsg::BlockMember {
                circle_id: 1,
                member_address: h.who[1].clone()
            },
            &[]
        )
        .is_err());
}

#[test]
fn deposits_cannot_skip_ahead_or_be_attached_to_free_calls() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    h.pay(1, C).unwrap();
    // Round 2 is not open yet.
    assert!(h.pay(1, C).is_err());
    // Funds attached to a free call are refused (never silently absorbed).
    assert!(h.exec(1, &ExecuteMsg::Tick { circle_id: 1 }, &[coin(1)]).is_err());
    h.check();
}

#[test]
fn random_order_is_a_permutation() {
    let mut h = H::new(Cfg {
        members: 6,
        order: PayoutOrderType::RandomOrder,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    let v = h.view(None);
    let mut slots: Vec<u32> = v.members.iter().map(|m| m.slot.unwrap()).collect();
    slots.sort();
    assert_eq!(slots, (0..6).collect::<Vec<u32>>());
}

// ---------------------------------------------------------------------------
// Starting and cancelling
// ---------------------------------------------------------------------------

#[test]
fn auto_start_by_date_anchors_at_the_advertised_instant() {
    let mut h = H::new(Cfg {
        auto_start: Some(("by_date", 10_000)),
        ..Cfg::default()
    });
    h.join_all();
    let at = h.view(None).auto_start_at.unwrap();
    assert_eq!(status(&h), CircleStatus::Full);
    h.advance(10_000 + 120);
    // Virtually running before anyone sends a transaction.
    let v = h.view(Some(1));
    assert_eq!(v.status, CircleStatus::Running);
    assert_eq!(v.start_at, Some(at));
    assert!(v.my_due.unwrap().can_pay_now);
    // A member's deposit materializes the start and pays round 1.
    h.pay(1, C).unwrap();
    let v = h.view(None);
    assert_eq!(v.start_at, Some(at));
    assert_eq!(v.round.unwrap().paid.len(), 1);
    h.check();
}

#[test]
fn auto_start_by_date_cancels_and_refunds_when_minimum_is_not_met() {
    let mut h = H::new(Cfg {
        members: 4,
        min_members: 4,
        auto_start: Some(("by_date", 5_000)),
        auto_refund: true,
        ..Cfg::default()
    });
    h.invite_and_join(1);
    h.advance(6_000);
    h.tick().unwrap();
    assert_eq!(status(&h), CircleStatus::Cancelled);
    assert_eq!(h.contract_bank(), 0);
    assert_eq!(h.net(0), 0);
    assert_eq!(h.net(1), 0);
}

#[test]
fn auto_start_when_full() {
    let mut h = H::new(Cfg {
        auto_start: Some(("by_members", 0)),
        ..Cfg::default()
    });
    h.join_all();
    assert_eq!(status(&h), CircleStatus::Running);
    h.check();
}

#[test]
fn cancel_before_start_refunds_everyone() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.exec(0, &ExecuteMsg::CancelCircle { circle_id: 1 }, &[]).unwrap();
    assert_eq!(status(&h), CircleStatus::Cancelled);
    for i in 0..3 {
        assert_eq!(h.net(i), 0);
    }
    assert_eq!(h.events_of("safrimba_closed").len(), 1);
}

/// PLAN_V5 3.6: the creator pays the standard exit penalty, not the whole lock.
#[test]
fn cancel_running_before_any_distribution_creator_pays_standard_penalty() {
    let mut h = H::new(Cfg {
        members: 4,
        threshold: Some(DistributionThreshold::Total {}),
        exit_bps: 2_000,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    for _ in 0..2 {
        h.everyone_pays();
        h.next_round();
    }
    h.everyone_pays();
    h.exec(0, &ExecuteMsg::CancelCircle { circle_id: 1 }, &[]).unwrap();
    assert_eq!(status(&h), CircleStatus::Cancelled);
    assert_eq!(h.contract_bank(), 0);
    // 20% of the creator's 2C lock, shared by the three other members.
    let penalty = (2 * C / 5) as i128;
    assert_eq!(h.net(0), -penalty);
    let others: i128 = (1..4).map(|i| h.net(i)).sum();
    assert_eq!(others, penalty);
    for i in 1..4 {
        assert!(h.net(i) >= penalty / 3, "member {} gets a share", i);
    }
}

#[test]
fn distribution_after_payout_blocks_cancel() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    h.everyone_pays();
    h.next_round();
    assert!(h
        .exec(0, &ExecuteMsg::CancelCircle { circle_id: 1 }, &[])
        .is_err());
}

// ---------------------------------------------------------------------------
// Missed rounds, debts
// ---------------------------------------------------------------------------

#[test]
fn missed_round_is_covered_by_lock_then_member_is_ejected() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    // Round 1: member 2 misses (lock covers it).
    h.everyone_pays_but(&[2]);
    h.next_round();
    let me = h.view(Some(2)).me.unwrap();
    assert_eq!(me.status, MemberStatus::Active);
    assert_eq!(me.lock.u128(), 0, "lock consumed to cover the round");
    assert_eq!(me.strikes, 1);
    // Round 2: member 2 misses again → cannot be covered → auto-ejection. They
    // had not been served yet: their round-1 contribution (via the lock) becomes
    // a claim, the round-2 shortfall is set off against it.
    h.everyone_pays_but(&[2]);
    h.next_round();
    let v = h.view(Some(2));
    let me = v.me.unwrap();
    assert_eq!(me.status, MemberStatus::Ejected);
    assert_eq!(me.arrears.u128(), 0, "arrears set off against the claim");
    assert_eq!(me.claim.u128(), C);
    assert_eq!(v.active_members, 2);
    // Round 3 was member 2's turn: the claim is paid from it, the rest makes
    // member 1 (whose pot lacked member 2's contribution) whole.
    h.everyone_pays();
    h.next_round();
    assert_eq!(status(&h), CircleStatus::Completed);
    for i in 0..3 {
        assert_eq!(h.net(i), 0, "member {} whole", i);
    }
    let paid = h.events_of("safrimba_claim_paid");
    assert_eq!(paid.len(), 1);
    assert_eq!(attr(paid[0], "amount"), C.to_string());
    assert_eq!(attr(paid[0], "source"), "turn");
}

#[test]
fn catch_up_payment_repays_arrears_and_refills_lock() {
    let mut h = H::new(Cfg {
        members: 4,
        policy: EjectionPolicy::AdminOnly,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    // Member 3 misses rounds 1 and 2 (lock covers round 1, round 2 becomes arrears).
    for _ in 0..2 {
        h.everyone_pays_but(&[3]);
        h.next_round();
    }
    let due = h.view(Some(3)).my_due.unwrap();
    assert_eq!(due.arrears.u128(), C);
    assert_eq!(due.lock_refill.u128(), C);
    assert_eq!(due.max_payment.u128(), 3 * C);
    // Over-payment and under-payment are rejected.
    assert!(h.pay(3, 3 * C + 1).is_err());
    assert!(h.pay(3, C - 1).is_err());
    h.pay(3, 3 * C).unwrap();
    let me = h.view(Some(3)).me.unwrap();
    assert_eq!(me.arrears.u128(), 0);
    assert_eq!(me.lock.u128(), C);
    assert_eq!(me.strikes, 0, "strikes reset once debts are cleared");
    h.check();
}

#[test]
fn admin_only_policy_with_absent_member_still_completes() {
    let mut h = H::new(Cfg {
        members: 3,
        policy: EjectionPolicy::AdminOnly,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    for _ in 0..3 {
        h.everyone_pays_but(&[2]);
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
}

/// PLAN_V5 3.4: the creator is a member like the others.
#[test]
fn defaulting_creator_is_ejected_like_any_member() {
    let mut h = H::new(Cfg {
        members: 3,
        cycles: 2,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    let supply = h.total_supply();
    for _ in 0..6 {
        h.everyone_pays_but(&[0]);
        h.next_round();
    }
    let v = h.view(Some(0));
    assert_eq!(v.status, CircleStatus::Completed);
    assert_eq!(v.me.unwrap().status, MemberStatus::Ejected);
    assert_eq!(h.contract_bank(), 0);
    assert_eq!(h.total_supply(), supply);
    // The honest members end within a late-fee of whole.
    for i in 1..3 {
        assert!(h.net(i).abs() <= (C / 10) as i128, "member {}: {}", i, h.net(i));
    }
}

// ---------------------------------------------------------------------------
// T1 — EndCycle never confiscates (PLAN_V5 3.8, C1)
// ---------------------------------------------------------------------------

#[test]
fn end_cycle_after_a_payout_finishes_the_cycle_for_everyone() {
    let mut h = H::new(Cfg {
        members: 4,
        cycles: 2,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.everyone_pays();
    h.next_round(); // the creator (slot 0) received 4C
    assert!(!h.view(None).end_now_allowed);
    h.exec(0, &ExecuteMsg::EndCycle { circle_id: 1 }, &[]).unwrap();
    let v = h.view(None);
    assert_eq!(v.status, CircleStatus::Running, "no confiscation: the cycle goes on");
    assert_eq!(v.ending_after_cycle, Some(1));
    assert_eq!(h.events_of("safrimba_end_requested").len(), 1);
    for _ in 0..3 {
        h.everyone_pays();
        h.next_round();
    }
    let v = h.view(None);
    assert_eq!(v.status, CircleStatus::Completed);
    assert_eq!(v.rounds_settled, 4, "stopped after cycle 1 of 2");
    assert_eq!(h.contract_bank(), 0);
    for i in 0..4 {
        assert_eq!(h.net(i), 0, "member {} received their turn", i);
    }
    let closed = h.events_of("safrimba_closed");
    assert_eq!(attr(closed[0], "reason"), "admin_end");
}

#[test]
fn end_cycle_request_can_be_withdrawn() {
    let mut h = H::new(Cfg {
        members: 4,
        cycles: 2,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.everyone_pays();
    h.next_round();
    h.exec(0, &ExecuteMsg::EndCycle { circle_id: 1 }, &[]).unwrap();
    h.exec(0, &ExecuteMsg::EndCycle { circle_id: 1 }, &[]).unwrap();
    assert_eq!(h.view(None).ending_after_cycle, None);
    assert_eq!(h.events_of("safrimba_end_cancelled").len(), 1);
    for _ in 0..3 {
        h.everyone_pays();
        h.next_round();
    }
    let v = h.view(None);
    assert_eq!(v.status, CircleStatus::Running);
    assert_eq!(v.rounds_settled, 4);
    assert_eq!(v.current_cycle, 2);
}

#[test]
fn end_cycle_before_the_first_distribution_ends_now_and_refunds() {
    let mut h = H::new(Cfg {
        members: 4,
        threshold: Some(DistributionThreshold::Total {}),
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    for _ in 0..2 {
        h.everyone_pays();
        h.next_round();
    }
    h.everyone_pays(); // round 3 paid, not settled
    assert!(h.view(None).end_now_allowed);
    h.exec(0, &ExecuteMsg::EndCycle { circle_id: 1 }, &[]).unwrap();
    assert_eq!(status(&h), CircleStatus::Ended);
    h.check();
    for i in 0..4 {
        assert_eq!(h.net(i), 0, "member {} got every contribution back", i);
    }
}

#[test]
fn end_cycle_at_a_cycle_boundary_ends_now() {
    let mut h = H::new(Cfg {
        cycles: 2,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    for _ in 0..3 {
        h.everyone_pays();
        h.next_round();
    }
    assert!(h.view(None).end_now_allowed);
    h.exec(0, &ExecuteMsg::EndCycle { circle_id: 1 }, &[]).unwrap();
    assert_eq!(status(&h), CircleStatus::Ended);
    h.check();
    for i in 0..3 {
        assert_eq!(h.net(i), 0);
    }
}

// ---------------------------------------------------------------------------
// T2 — Bounded pause (PLAN_V5 3.9, C4)
// ---------------------------------------------------------------------------

#[test]
fn pause_freezes_and_shifts_every_deadline() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    let r = h.view(None).round.unwrap();
    h.advance(1_000);
    h.pause(0, "assemblée").unwrap();
    h.advance(3_000);
    assert!(h.tick().is_err(), "nothing settles while paused");
    // Deposits are still accepted while paused.
    h.pay(1, C).unwrap();
    h.exec(0, &ExecuteMsg::UnpauseCircle { circle_id: 1 }, &[]).unwrap();
    let r2 = h.view(None).round.unwrap();
    assert_eq!(r2.settles_at.seconds(), r.settles_at.seconds() + 3_000);
    h.check();
}

#[test]
fn pause_ends_by_itself_after_the_maximum() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    let t0 = h.now().seconds();
    let r = h.view(None).round.unwrap();
    h.advance(1_000);
    h.pause(0, "Réunion du groupe").unwrap();
    let v = h.view(None);
    assert_eq!(v.status, CircleStatus::Paused);
    assert_eq!(v.max_pause_secs, ROUND);
    assert_eq!(v.paused_until.unwrap().seconds(), t0 + 1_000 + ROUND);
    assert_eq!(v.pause_reason.as_deref(), Some("Réunion du groupe"));
    // Deadlines are shown after the automatic resume.
    assert_eq!(v.round.unwrap().settles_at.seconds(), r.settles_at.seconds() + ROUND);
    assert!(v.next_action_at.is_some(), "the keeper knows when to come back");
    h.everyone_pays();
    // The admin disappears: the circle resumes by itself.
    h.advance(4_000);
    let v = h.view(None);
    assert_eq!(v.status, CircleStatus::Running);
    assert_eq!(v.paused_until, None);
    assert_eq!(v.pause_secs_left, ROUND);
    let settles = r.settles_at.seconds() + ROUND;
    h.advance_to(Timestamp::from_seconds(settles - 1));
    h.check();
    assert_eq!(h.view(None).rounds_settled, 0);
    h.advance(1);
    h.check();
    assert_eq!(h.view(None).rounds_settled, 1, "shifted by exactly the maximal pause");
    assert!(h
        .events_of("safrimba_status")
        .iter()
        .any(|e| attr(e, "reason") == "auto_resume"));
}

#[test]
fn pause_budget_is_two_maximal_pauses() {
    let mut h = H::new(Cfg {
        cycles: 2,
        policy: EjectionPolicy::AdminOnly,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.pause(0, "one").unwrap();
    h.advance(100);
    h.exec(0, &ExecuteMsg::UnpauseCircle { circle_id: 1 }, &[]).unwrap();
    assert_eq!(h.view(None).pause_secs_left, 2 * ROUND - 100);
    h.pause(0, "two").unwrap();
    h.advance(ROUND);
    // "two" ended by itself after a full pause: 100 + 3600 s used, 3500 s left.
    h.pause(0, "three").unwrap();
    assert_eq!(
        h.view(None).paused_until.unwrap().seconds(),
        h.now().seconds() + ROUND - 100
    );
    h.advance(ROUND - 100);
    let e = err_text(h.pause(0, "four"));
    assert!(e.contains("Pause budget exhausted"), "{}", e);
    assert_eq!(h.view(None).pause_secs_left, 0);
    h.check();
}

#[test]
fn pause_reason_is_bounded_and_defaults() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    let long: String = "é".repeat(141);
    assert!(h.pause(0, &long).is_err());
    let ok: String = "é".repeat(140);
    h.pause(0, &ok).unwrap();
    h.exec(0, &ExecuteMsg::UnpauseCircle { circle_id: 1 }, &[]).unwrap();
    h.exec(
        0,
        &ExecuteMsg::PauseCircle {
            circle_id: 1,
            reason: None,
        },
        &[],
    )
    .unwrap();
    assert_eq!(h.view(None).pause_reason.as_deref(), Some("unspecified"));
    // Members cannot pause.
    h.exec(0, &ExecuteMsg::UnpauseCircle { circle_id: 1 }, &[]).unwrap();
    assert!(h.pause(1, "no").is_err());
}

// ---------------------------------------------------------------------------
// T3 — Admin powers (PLAN_V5 3.4, 3.10, C2, C12)
// ---------------------------------------------------------------------------

#[test]
fn admin_cannot_eject_a_member_in_good_standing() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    h.everyone_pays();
    h.next_round();
    let e = err_text(h.eject(0, 1));
    assert!(e.contains("eject for cause"), "{}", e);
    assert!(h.eject(0, 0).is_err(), "self-ejection refused");
    // Member 2 misses round 2 (lock covers it): now there is a cause.
    h.everyone_pays_but(&[2]);
    h.next_round();
    let v = h.view(None);
    let flags: Vec<bool> = v.members.iter().map(|m| m.can_be_ejected).collect();
    assert_eq!(flags, vec![false, false, true]);
    h.eject(0, 2).unwrap();
    assert_eq!(h.view(Some(2)).me.unwrap().status, MemberStatus::Ejected);
    h.everyone_pays();
    h.next_round();
    assert_eq!(status(&h), CircleStatus::Completed);
    for i in 0..3 {
        assert_eq!(h.net(i), 0, "member {} whole", i);
    }
}

#[test]
fn co_admin_cannot_cancel_but_can_organise() {
    let mut h = H::new(Cfg {
        co_admin: Some(1),
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    assert_eq!(h.view(None).admins, vec![h.who[0].clone(), h.who[1].clone()]);
    let e = err_text(h.exec(1, &ExecuteMsg::CancelCircle { circle_id: 1 }, &[]));
    assert!(e.contains("Only the creator"), "{}", e);
    h.pause(1, "co-admin pause").unwrap();
    h.exec(1, &ExecuteMsg::UnpauseCircle { circle_id: 1 }, &[]).unwrap();
    h.exec(0, &ExecuteMsg::CancelCircle { circle_id: 1 }, &[]).unwrap();
    assert_eq!(status(&h), CircleStatus::Cancelled);
    let penalty = (2 * C / 5) as i128;
    assert_eq!(h.net(0), -penalty);
    assert_eq!(h.net(1) + h.net(2), penalty);
}

#[test]
fn admin_role_moves_when_the_creator_leaves() {
    // No co-admin: the active member with the lowest slot.
    let mut h = H::new(Cfg {
        members: 4,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.exit(0).unwrap();
    assert_eq!(h.view(None).admins, vec![h.who[1].clone()]);
    assert!(h.pause(0, "gone").is_err());
    h.pause(1, "new admin").unwrap();
    // With a co-admin: the co-admin.
    let mut h = H::new(Cfg {
        members: 4,
        co_admin: Some(2),
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.exit(0).unwrap();
    assert_eq!(h.view(None).admins, vec![h.who[2].clone()]);
    h.check();
}

// ---------------------------------------------------------------------------
// T4 — Departures, claims, settle-up (PLAN_V5 3.3, C3, C13, C15)
// ---------------------------------------------------------------------------

#[test]
fn exit_before_turn_returns_lock_and_prepaid_rounds() {
    let mut h = H::new(Cfg {
        members: 4,
        exit_bps: 2_000,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.everyone_pays(); // round 1 paid, not settled
    let q = h.quote(3);
    assert!(q.allowed);
    assert!(!q.served_this_cycle);
    assert_eq!(q.prepaid_refund.u128(), C);
    assert_eq!(q.penalty.u128(), C / 5);
    assert_eq!(q.lock_refund.u128(), C - C / 5);
    assert_eq!(q.claim.u128(), 0);
    h.exit(3).unwrap();
    let me = h.view(Some(3)).me.unwrap();
    assert_eq!(me.status, MemberStatus::Exited);
    assert_eq!(me.lock.u128(), 0, "no liability left: the lock is back");
    assert_eq!(h.net(3), -((C / 5) as i128), "only the exit penalty is lost");
    for _ in 0..4 {
        h.everyone_pays();
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    // The others shared the leaver's orphaned turn and the penalty.
    let others: i128 = (0..3).map(|i| h.net(i)).sum();
    assert_eq!(others, (C / 5) as i128);
    for i in 0..3 {
        assert!(h.net(i) >= 0, "member {} lost money: {}", i, h.net(i));
    }
}

#[test]
fn leaver_before_turn_in_rotation_is_repaid_and_nobody_gains() {
    let mut h = H::new(Cfg {
        members: 4,
        exit_bps: 0,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.everyone_pays();
    h.next_round(); // member 0 served
    let q = h.quote(3);
    assert_eq!(q.claim.u128(), C);
    assert_eq!(q.claim_paid_now.u128(), 0, "rotation: no pooled carry");
    assert_eq!(q.lock_refund.u128(), C);
    assert_eq!(q.to_attach.u128(), 0);
    h.exit(3).unwrap();
    assert_eq!(h.view(Some(3)).me.unwrap().claim.u128(), C);
    for _ in 0..3 {
        h.everyone_pays();
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    for i in 0..4 {
        assert_eq!(h.net(i), 0, "member {} whole", i);
    }
    let paid = h.events_of("safrimba_claim_paid");
    assert_eq!(paid.len(), 1);
    assert_eq!(attr(paid[0], "amount"), C.to_string());
}

#[test]
fn leaver_in_total_mode_gets_contributions_back_immediately() {
    let mut h = H::new(Cfg {
        members: 4,
        threshold: Some(DistributionThreshold::Total {}),
        exit_bps: 0,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    for _ in 0..2 {
        h.everyone_pays();
        h.next_round();
    }
    let q = h.quote(3);
    assert_eq!(q.claim.u128(), 2 * C);
    assert_eq!(q.claim_paid_now.u128(), 2 * C);
    h.exit(3).unwrap();
    assert_eq!(h.net(3), 0, "contributions and lock back at once");
    for _ in 0..2 {
        h.everyone_pays();
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    for i in 0..4 {
        assert_eq!(h.net(i), 0, "member {} whole", i);
    }
}

#[test]
fn leaver_in_hybrid_mode_is_exactly_fair() {
    // The leaver holds a group slot.
    for leaver in [3usize, 1usize] {
        let mut h = H::new(Cfg {
            members: 4,
            min_members: 4,
            threshold: Some(DistributionThreshold::MinMembers { count: 2 }),
            exit_bps: 0,
            ..Cfg::default()
        });
        h.join_all();
        h.start();
        for _ in 0..2 {
            h.everyone_pays();
            h.next_round();
        }
        h.exit(leaver).unwrap();
        for _ in 0..2 {
            h.everyone_pays();
            h.next_round();
        }
        assert_eq!(status(&h), CircleStatus::Completed);
        for i in 0..4 {
            assert_eq!(h.net(i), 0, "leaver {}: member {} not whole", leaver, i);
        }
    }
}

#[test]
fn exit_after_being_served_requires_settling_the_cycle() {
    let mut h = H::new(Cfg {
        members: 4,
        exit_bps: 2_000,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.everyone_pays();
    h.next_round(); // the creator received 4C
    let q = h.quote(0);
    assert!(q.served_this_cycle);
    assert_eq!(q.penalty.u128(), 2 * C / 5);
    assert_eq!(q.owed.u128(), 3 * C);
    assert_eq!(q.from_lock.u128(), 2 * C - 2 * C / 5);
    assert_eq!(q.to_attach.u128(), 3 * C - (2 * C - 2 * C / 5));
    assert!(h.exec(0, &ExecuteMsg::ExitCircle { circle_id: 1 }, &[]).is_err());
    assert!(h
        .exec(0, &ExecuteMsg::ExitCircle { circle_id: 1 }, &[coin(C)])
        .is_err());
    h.exit(0).unwrap();
    let me = h.view(Some(0)).me.unwrap();
    assert_eq!(me.status, MemberStatus::Exited);
    assert_eq!(me.paid_through, 4, "the rest of the cycle is prepaid");
    for _ in 0..3 {
        h.everyone_pays();
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    assert_eq!(h.net(0), -((2 * C / 5) as i128), "only the exit penalty");
    for i in 1..4 {
        assert!(h.net(i) >= (2 * C / 15) as i128, "member {}", i);
    }
}

#[test]
fn ejection_after_being_served_records_a_repayable_debt() {
    let mut h = H::new(Cfg {
        members: 4,
        cycles: 2,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    let supply = h.total_supply();
    for _ in 0..2 {
        h.everyone_pays();
        h.next_round(); // members 0 then 1 served
    }
    for _ in 0..2 {
        h.everyone_pays_but(&[1]);
        h.next_round();
    }
    let me = h.view(Some(1)).me.unwrap();
    assert_eq!(me.status, MemberStatus::Ejected);
    assert_eq!(me.arrears.u128(), C, "the uncovered round stays as a debt");
    // A departed member can still repay the debt (it goes to the common pool).
    let repay = ExecuteMsg::RepayArrears { circle_id: 1 };
    assert!(h.exec(1, &repay, &[coin(C + 1)]).is_err());
    h.exec(1, &repay, &[coin(C)]).unwrap();
    assert_eq!(h.view(Some(1)).me.unwrap().arrears.u128(), 0);
    assert_eq!(h.events_of("safrimba_arrears_repaid").len(), 1);
    for _ in 0..4 {
        h.everyone_pays();
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    assert_eq!(h.total_supply(), supply);
    assert_eq!(h.net(1), 0, "after repaying, the defaulter is even");
}

#[test]
fn admin_ejection_before_turn_repays_net_contributions() {
    let mut h = H::new(Cfg {
        members: 4,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.everyone_pays_but(&[3]);
    h.next_round(); // member 3 covered by the lock: one strike
    h.eject(0, 3).unwrap();
    let me = h.view(Some(3)).me.unwrap();
    assert_eq!(me.status, MemberStatus::Ejected);
    assert_eq!(me.claim.u128(), C, "the lock-covered round is theirs");
    for _ in 0..3 {
        h.everyone_pays();
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    for i in 0..4 {
        assert_eq!(h.net(i), 0, "member {} whole", i);
    }
}

#[test]
fn early_close_pays_unserved_members_first() {
    let mut h = H::new(Cfg {
        members: 3,
        threshold: Some(DistributionThreshold::Total {}),
        exit_bps: 2_000,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.everyone_pays();
    h.next_round();
    h.exit(1).unwrap();
    h.exit(2).unwrap(); // one active member left → the circle ends
    assert_eq!(status(&h), CircleStatus::Ended);
    h.check();
    assert_eq!(h.net(1), -((C / 5) as i128));
    assert_eq!(h.net(2), -((C / 5) as i128));
    assert_eq!(h.net(0), (2 * C / 5) as i128);
}

#[test]
fn strict_mode_allows_exit_only_at_a_cycle_boundary() {
    let mut h = H::new(Cfg {
        strict: true,
        cycles: 2,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    assert_eq!(h.quote(1).blocked_reason.as_deref(), Some("strict_mode"));
    assert!(h.exec(1, &ExecuteMsg::ExitCircle { circle_id: 1 }, &[]).is_err());
    for _ in 0..3 {
        h.everyone_pays();
        h.next_round();
    }
    // Cycle 1 is over: leaving now is clean.
    let q = h.quote(1);
    assert!(q.allowed, "{:?}", q);
    h.exit(1).unwrap();
    h.check();
}

// ---------------------------------------------------------------------------
// T5 — Catching up (C8)
// ---------------------------------------------------------------------------

#[test]
fn catching_up_blocks_roster_changes_until_ticked() {
    let mut h = H::new(Cfg {
        members: 3,
        cycles: 12,
        policy: EjectionPolicy::AdminOnly,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.advance(30 * ROUND + GRACE + 1);
    let v = h.view(Some(2));
    assert_eq!(v.rounds_behind, 30);
    assert!(!v.my_due.unwrap().can_pay_now);
    assert_eq!(
        v.me.unwrap().exit_quote.unwrap().blocked_reason.as_deref(),
        Some("catching_up")
    );
    assert!(err_text(h.exec(2, &ExecuteMsg::ExitCircle { circle_id: 1 }, &[])).contains("catching up"));
    assert!(err_text(h.pause(0, "x")).contains("catching up"));
    assert!(err_text(h.exec(0, &ExecuteMsg::EndCycle { circle_id: 1 }, &[])).contains("catching up"));
    h.tick().unwrap(); // 24 rounds
    assert_eq!(h.view(None).rounds_behind, 6);
    h.exit(2).unwrap(); // its own sync settles the last 6
    h.check();
}

// ---------------------------------------------------------------------------
// T6 — A failing send never blocks the circle (C5)
// ---------------------------------------------------------------------------

#[test]
fn failed_payout_is_deferred_and_withdrawable() {
    let mut h = H::new(Cfg::default());
    h.join_all();
    h.start();
    h.blocked.borrow_mut().insert(h.who[0].to_string());
    let before = h.bank(&h.who[0]);
    h.everyone_pays();
    h.next_round(); // the creator's payout fails, settlement goes through
    let v = h.view(Some(0));
    assert_eq!(v.rounds_settled, 1);
    assert_eq!(v.pending_payouts_total.u128(), 3 * C);
    assert_eq!(v.me.unwrap().pending_payout.u128(), 3 * C);
    assert_eq!(h.bank(&h.who[0]), before - C);
    let deferred = h.events_of("safrimba_payout_deferred");
    assert_eq!(deferred.len(), 1);
    assert_eq!(attr(deferred[0], "amount"), (3 * C).to_string());
    // Still blocked: the withdrawal is deferred again, nothing is lost.
    h.exec(0, &ExecuteMsg::Withdraw { circle_id: 1 }, &[]).unwrap();
    assert_eq!(h.view(None).pending_payouts_total.u128(), 3 * C);
    h.check();
    h.blocked.borrow_mut().clear();
    h.exec(0, &ExecuteMsg::Withdraw { circle_id: 1 }, &[]).unwrap();
    assert_eq!(h.view(None).pending_payouts_total.u128(), 0);
    for _ in 0..2 {
        h.everyone_pays();
        h.next_round();
    }
    assert_eq!(status(&h), CircleStatus::Completed);
    assert_eq!(h.contract_bank(), 0);
    for i in 0..3 {
        assert_eq!(h.net(i), 0);
    }
}

#[test]
fn failed_platform_fee_is_deferred_too() {
    let mut h = H::new(Cfg {
        platform_bps: 100,
        ..Cfg::default()
    });
    h.join_all();
    h.start();
    h.blocked.borrow_mut().insert(h.platform.to_string());
    for _ in 0..3 {
        h.everyone_pays();
        h.next_round();
    }
    // Closed, but a withdrawal is still owed: Finalizing until it is pulled.
    assert_eq!(status(&h), CircleStatus::Finalizing);
    let fees = 3 * (3 * C) / 100;
    assert_eq!(h.view(None).pending_payouts_total.u128(), fees);
    assert_eq!(h.contract_bank(), fees);
    h.blocked.borrow_mut().clear();
    let platform = h.platform.clone();
    h.exec_as(&platform, &ExecuteMsg::Withdraw { circle_id: 1 }, &[])
        .unwrap();
    assert_eq!(h.bank(&h.platform), fees);
    assert_eq!(h.contract_bank(), 0);
    assert_eq!(status(&h), CircleStatus::Completed);
}

#[test]
fn add_private_member_rejects_contract_addresses() {
    let mut h = H::new(Cfg {
        members: 4,
        ..Cfg::default()
    });
    let contract = h.contract.clone();
    let e = err_text(h.exec(
        0,
        &ExecuteMsg::AddPrivateMember {
            circle_id: 1,
            member_address: contract,
            pseudonym: None,
        },
        &[coin(C)],
    ));
    assert!(e.contains("smart contract"), "{}", e);
}

// ---------------------------------------------------------------------------
// T7 — Randomized adversarial simulation
// ---------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

#[derive(Default)]
struct SimOut {
    completed: bool,
    departures: u32,
    claims: usize,
    pauses: usize,
}

fn dump(h: &H, s: u32) {
    for e in &h.events {
        if e.ty.starts_with("wasm-safrimba_") {
            let attrs: Vec<String> = e
                .attributes
                .iter()
                .filter(|a| a.key != "_contract_address" && a.key != "circle_id")
                .map(|a| format!("{}={}", a.key, a.value))
                .collect();
            eprintln!("[S={}] {} {}", s, &e.ty[5..], attrs.join(" "));
        }
    }
}

fn cfg_summary(c: &Cfg) -> String {
    format!(
        "members {} cycles {} threshold {:?} policy {:?} exit {} late {} fee {} strict {} order {:?} co_admin {:?}",
        c.members, c.cycles, c.threshold, c.policy, c.exit_bps, c.late_bps, c.platform_bps, c.strict, c.order, c.co_admin
    )
}

/// Replay one seed with a full event dump: `cargo test -- --ignored replay_seed --nocapture`
/// with `SEED=<n>`.
#[test]
#[ignore]
fn replay_seed() {
    let arg = std::env::var("SEED").unwrap_or_default();
    if arg == "scan" {
        let mut failing = vec![];
        for seed in 1..=400u64 {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| simulate(seed, false)));
            if r.is_err() {
                failing.push(seed);
            }
        }
        eprintln!("failing seeds: {:?}", failing);
        return;
    }
    let seed: u64 = arg.parse().unwrap_or(1);
    simulate(seed, true);
}

/// One randomized circle (see the test below).
fn simulate(seed: u64, verbose: bool) -> SimOut {
    let mut out = SimOut::default();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let big = rng.chance(8);
    let members = if big {
        12 + rng.below(19) as usize
    } else {
        2 + rng.below(7) as usize
    };
    let cycles = if big {
        1 + rng.below(2) as u32
    } else {
        1 + rng.below(6) as u32
    };
    let threshold = match rng.below(3) {
        0 => Some(DistributionThreshold::Total {}),
        1 => Some(DistributionThreshold::MinMembers { count: 1 }),
        _ => Some(DistributionThreshold::MinMembers {
            count: 1 + rng.below(members as u64) as u32,
        }),
    };
    let min_members = match &threshold {
        Some(DistributionThreshold::MinMembers { count }) => (*count).max(2).min(members as u32),
        _ => 2,
    };
    let policy = match rng.below(3) {
        0 => EjectionPolicy::AdminOnly,
        1 => EjectionPolicy::AutoOnly,
        _ => EjectionPolicy::AutoAndAdmin,
    };
    let co_admin = if members >= 3 && rng.chance(30) {
        Some(1 + rng.below(members as u64 - 1) as usize)
    } else {
        None
    };
    let cfg = Cfg {
        members,
        cycles,
        threshold,
        min_members: min_members.max(1),
        policy: policy.clone(),
        exit_bps: rng.below(5_000),
        late_bps: 1 + rng.below(3_000),
        platform_bps: rng.below(300),
        strict: rng.chance(25),
        order: if rng.chance(50) {
            PayoutOrderType::RandomOrder
        } else {
            PayoutOrderType::PredefinedOrder
        },
        co_admin,
        ..Cfg::default()
    };
    let mut h = H::new(cfg.clone());
    h.join_all();
    h.start();
    let supply = h.total_supply();
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 3_000, "seed {}: did not finish", seed);
        let v = h.view(None);
        if terminal(&v.status) {
            break;
        }
        // Members act.
        for i in 0..members {
            let me = h.view(Some(i));
            let Some(due) = me.my_due.clone() else { continue };
            if !due.can_pay_now {
                continue;
            }
            match rng.below(10) {
                0..=5 => {
                    h.pay(i, C).unwrap();
                }
                6 => {
                    h.pay(i, due.max_payment.u128()).unwrap();
                }
                7 => {
                    let extra = rng.below((due.max_payment.u128() - C) as u64 + 1) as u128;
                    h.pay(i, C + extra).unwrap();
                }
                _ => {} // miss
            }
        }
        // Departures, always through the quote: it must execute as shown.
        if rng.chance(4) {
            h.settle_all();
            let i = rng.below(members as u64) as usize;
            if let Some(q) = h.view(Some(i)).me.and_then(|m| m.exit_quote) {
                if q.allowed {
                    let before = h.bank(&h.who[i]);
                    h.exit(i).unwrap_or_else(|e| panic!("seed {}: quoted exit failed: {}", seed, e));
                    out.departures += 1;
                    let after = h.bank(&h.who[i]);
                    let promised = q.lock_refund.u128() + q.prepaid_refund.u128() + q.claim_paid_now.u128();
                    if terminal(&h.view(None).status) {
                        assert!(after + q.to_attach.u128() >= before + promised, "seed {}", seed);
                    } else {
                        assert_eq!(after + q.to_attach.u128(), before + promised, "seed {}: exit quote mismatch", seed);
                    }
                }
            }
        }
        // Debt repayment by anyone who owes something.
        if rng.chance(5) {
            let i = rng.below(members as u64) as usize;
            let v = h.view(Some(i));
            if let (Some(me), false) = (v.me, terminal(&v.status)) {
                let refill = if me.status == MemberStatus::Active {
                    me.lock_original.u128() - me.lock.u128()
                } else {
                    0
                };
                let max = me.arrears.u128() + refill;
                if max > 0 && v.rounds_behind == 0 && !v.tick_due {
                    let amount = 1 + rng.below(max as u64) as u128;
                    h.exec(i, &ExecuteMsg::RepayArrears { circle_id: 1 }, &[coin(amount)])
                        .unwrap_or_else(|e| panic!("seed {}: repay failed: {}", seed, e));
                }
            }
        }
        // Adversarial admin.
        h.settle_all();
        let v = h.view(None);
        if !terminal(&v.status) {
            if let Some(admin) = v.admins.first().cloned() {
                let ai = h.idx(&admin);
                if rng.chance(5) {
                    let target = rng.below(members as u64) as usize;
                    let tv = v.members.iter().find(|m| m.address == h.who[target]).cloned();
                    let res = h.eject(ai, target);
                    let expected = tv.map(|m| m.can_be_ejected).unwrap_or(false) && target != ai;
                    assert_eq!(res.is_ok(), expected, "seed {}: eject result {:?}", seed, res.err());
                }
                if rng.chance(5) && v.status == CircleStatus::Running {
                    let res = h.pause(ai, "random");
                    assert_eq!(res.is_ok(), v.pause_secs_left > 0, "seed {}: pause {:?}", seed, res.err());
                    if res.is_ok() {
                        out.pauses += 1;
                    }
                }
                if rng.chance(25) && v.status == CircleStatus::Paused {
                    h.exec(ai, &ExecuteMsg::UnpauseCircle { circle_id: 1 }, &[]).unwrap();
                }
                if rng.chance(2) && !terminal(&h.view(None).status) {
                    h.exec(ai, &ExecuteMsg::EndCycle { circle_id: 1 }, &[])
                        .unwrap_or_else(|e| panic!("seed {}: end_cycle failed: {}", seed, e));
                }
            }
            if rng.chance(1) {
                let _ = h.exec(0, &ExecuteMsg::CancelCircle { circle_id: 1 }, &[]);
            }
        }
        // Time passes: within the round, a round, several, or a long keeper outage.
        let jump = match rng.below(7) {
            0 => rng.below(ROUND),
            1 => ROUND + GRACE,
            2 => rng.below(4 * ROUND),
            3 => GRACE / 2,
            4 if rng.chance(30) => 26 * ROUND,
            _ => rng.below(2 * ROUND),
        };
        h.advance(jump.max(1));
        if rng.chance(60) {
            h.check();
        }
        assert_eq!(h.total_supply(), supply, "seed {}: supply changed", seed);
    }
    h.check();
    assert_eq!(h.contract_bank(), 0, "seed {}: funds left behind", seed);
    assert_eq!(h.total_supply(), supply, "seed {}: supply changed", seed);
    let v = h.view(None);
    for m in &v.members {
        assert!(m.claim.is_zero(), "seed {}: open claim for {}", seed, m.address);
    }
    if verbose {
        eprintln!("seed {}: {:?}", seed, cfg_summary(&h.cfg));
        dump(&h, members as u32);
    }
    // Per member and cycle: served at most once, never more than S × C gross.
    let s = members as u32;
    let mut served: BTreeMap<(String, u32), u32> = BTreeMap::new();
    let mut gross: BTreeMap<(String, u32), u128> = BTreeMap::new();
    for e in h.events_of("safrimba_payout") {
        let kind = attr(e, "kind");
        let round: u32 = attr(e, "round").parse().unwrap();
        let cycle = (round.max(1) - 1) / s + 1;
        let who = attr(e, "recipient");
        let amount: u128 = attr(e, "gross").parse().unwrap();
        if kind == "turn" || kind == "group" {
            *served.entry((who.clone(), cycle)).or_default() += 1;
        }
        if kind != "close_share" {
            *gross.entry((who, cycle)).or_default() += amount;
        }
    }
    for (k, n) in served {
        assert!(n <= 1, "seed {}: {:?} served {} times in a cycle", seed, k, n);
    }
    for (k, g) in gross {
        assert!(g <= s as u128 * C, "seed {}: {:?} received {} > S×C", seed, k, g);
    }
    out.claims = h.events_of("safrimba_claim_paid").len();
    out.completed = v.status == CircleStatus::Completed;
    out
}

/// Up to 30 members and 6 cycles, members who pay, skip, overpay, repay and
/// leave (always through the exit quote), an adversarial admin (ejections,
/// pauses, early ends, cancels), keeper outages of more than 24 rounds.
/// Invariants: money is conserved at every step, every circle finishes, the
/// contract ends empty, quotes are exact, nobody is served twice in a cycle,
/// nobody receives more than `S × C` gross in a cycle, no claim is left open.
#[test]
fn randomized_adversarial_lifecycles_conserve_money_and_always_finish() {
    let (mut completed, mut ended, mut departures, mut claims, mut pauses) =
        (0u32, 0u32, 0u32, 0usize, 0usize);
    for seed in 1..=150u64 {
        let out = simulate(seed, false);
        if out.completed {
            completed += 1;
        } else {
            ended += 1;
        }
        departures += out.departures;
        claims += out.claims;
        pauses += out.pauses;
    }
    eprintln!(
        "randomized: {} completed, {} ended early, {} quoted exits, {} claim payments, {} pauses",
        completed, ended, departures, claims, pauses
    );
    assert!(completed >= 20, "too few full lifecycles exercised");
    assert!(ended >= 5, "too few early endings exercised");
    assert!(departures >= 20, "too few departures exercised");
    assert!(claims >= 10, "too few claims exercised");
    assert!(pauses >= 10, "too few pauses exercised");
}
