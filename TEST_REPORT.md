# Safrimba Contract Test Report

Generated: 2026-05-12T10:20:37Z
Network: testnet | Chain: safro-testnet-1 | Code ID: 122
Contract: `addr_safro1le7ef2vc67ugruurn0zyusxvwsd8kjqws5vct57kpfrxye0krleqk6ysqp`
Creator: `addr_safro1hezduhmk52kacax8f9l076sdgtejf8eshtszla` | Member: `addr_safro1f8a9m8r5dq046qvmm9h5eryk0fn0u7tqu6v6sn`
Cycle: 20s | Grace: 5s | Round wait: 22s

## Results: 125 passed, 0 failed out of 125 tests

```
════════════════════════════════════════════════════════════════
  SAFRIMBA COMPREHENSIVE CONTRACT TEST (cron-driven)
════════════════════════════════════════════════════════════════
Network: testnet | Code ID: 122 | cycle=20s grace=5s
Creator: mycontractadmin (addr_safro1hezduhmk52kacax8f9l076sdgtejf8eshtszla)
Member:  mywallet (addr_safro1f8a9m8r5dq046qvmm9h5eryk0fn0u7tqu6v6sn)

[1] INSTANTIATE CONTRACT
  Contract: addr_safro1le7ef2vc67ugruurn0zyusxvwsd8kjqws5vct57kpfrxye0krleqk6ysqp

[2] PUBLIC CIRCLE CREATION — expected to be rejected
  ✓ PASS: Public visibility rejected (temporarily disabled at contract level)

[3] CREATE 7 PRIVATE CIRCLES (varied configs)
  ✓ PASS: Circle 1: Private + Total, 3c × 2m (long-running)
  ✓ PASS: Circle 2: Private + None,  1c × 2m
  ✓ PASS: Circle 3: Private + MinMembers(2), 3-cap 2-actual
  ✓ PASS: Circle 4: Private + Total, manual_trigger
  ✓ PASS: Circle 5: Private + None, ejection params
  ✓ PASS: Circle 6: Private + Total (cancel test)
  ✓ PASS: Circle 7: Private + None (exit test)
  ✓ PASS: Circle 8: Private + Total, 2c × 2m (block target)

[4] UPDATE CIRCLE (before start)
  ✓ PASS: UpdateCircle on circle 6

[5] JoinCircle MUST FAIL on Private circles
  ✓ PASS: JoinCircle on circle 1 (Private) rejected

[6] INVITE + ACCEPT (Private flow on all 7 circles)
  ✓ PASS: Invite member to circle 1
  ✓ PASS: Member accepts circle 1
  ✓ PASS: Invite member to circle 2
  ✓ PASS: Member accepts circle 2
  ✓ PASS: Invite member to circle 3
  ✓ PASS: Member accepts circle 3
  ✓ PASS: Invite member to circle 4
  ✓ PASS: Member accepts circle 4
  ✓ PASS: Invite member to circle 5
  ✓ PASS: Member accepts circle 5
  ✓ PASS: Invite member to circle 6
  ✓ PASS: Member accepts circle 6
  ✓ PASS: Invite member to circle 7
  ✓ PASS: Member accepts circle 7
  ✓ PASS: Invite member to circle 8
  ✓ PASS: Member accepts circle 8

[7] AddPrivateMember — direct creator add (no funds attached)
  ✓ PASS: AddPrivateMember on circle 3 (third addr w/ pseudonym)

[8] UpdateMemberPseudonym (creator updates a member's pseudonym)
  ✓ PASS: UpdateMemberPseudonym on circle 3

[9] CANCEL CIRCLE BEFORE START
  ✓ PASS: Cancel circle 6 (before start, full refund)
  ✓ PASS: Verify circle 6 status = Cancelled

[10] EXIT CIRCLE BEFORE START
  ✓ PASS: Exit circle 7 as member (full refund)

[11] START CIRCLES
  ✓ PASS: Start circle 1 manually (status was Full)
  ✓ PASS: Start circle 2 manually (status was Full)
  ✓ PASS: Start circle 3 manually (status was Full)
  ✓ PASS: Start circle 4 manually (status was Full)
  ✓ PASS: Start circle 5 manually (status was Full)
  ✓ PASS: Start circle 8 manually (status was Full)

[12] QUERY CIRCLE STATES (initial snapshot)
  Circle 1: status=Running, members=2, threshold={"total":{}}
  Circle 2: status=Running, members=2, threshold=none
  Circle 3: status=Running, members=3, threshold={"min_members":{"count":2}}
  Circle 4: status=Running, members=2, threshold={"total":{}}
  Circle 5: status=Running, members=2, threshold=none
  Circle 6: status=Cancelled, members=2, threshold={"total":{}}
  Circle 7: status=Cancelled, members=1, threshold=none
  Circle 8: status=Running, members=2, threshold={"total":{}}

[13] DEPOSIT CONTRIBUTION (Round 1)
  ✓ PASS: Circle 1: creator deposit R1
  ✓ PASS: Circle 1: member  deposit R1
  ✓ PASS: Circle 2: creator deposit R1
  ✓ PASS: Circle 2: member  deposit R1
  ✓ PASS: Circle 3: creator deposit R1
  ✓ PASS: Circle 3: member  deposit R1
  ✓ PASS: Circle 4: creator deposit R1
  ✓ PASS: Circle 4: member  deposit R1
  ✓ PASS: Circle 8: creator deposit R1
  ✓ PASS: Circle 8: member  deposit R1
  ✓ PASS: Circle 5: creator deposit R1 (member skips, will accumulate late fees)

  … sleeping 22s for R1 of all circles (cron tick) (cycle+grace gate)
[14] R1→R2 TRANSITION (cron-style: advance or payout depending on threshold)
  ✓ PASS: Circle 1: AdvanceRound R1→R2 (Total, not yet distribution round)
  ✓ PASS: Circle 2: AdvanceRound R1→R2 (None=Total semantics, awaiting last round)
  ✓ PASS: Circle 3: AdvanceRound R1→R2 (MinMembers(2))
  ✓ PASS: Circle 4: AdvanceRound R1→R2 (Total, manual_trigger)
  ✓ PASS: Circle 5: AdvanceRound R1→R2 (None=Total, missing member triggers late fee)
  ✓ PASS: Circle 8: AdvanceRound R1→R2 (block-target setup)

[15] PAUSE / UNPAUSE
  ✓ PASS: Pause circle 3
  ✓ PASS: Verify circle 3 status = Paused
  ✓ PASS: Unpause circle 3
  ✓ PASS: Verify circle 3 status = Running

[16] DEPOSIT R2 for all running circles
  ✓ PASS: Circle 1: creator deposit R2
  ✓ PASS: Circle 1: member  deposit R2
  ✓ PASS: Circle 2: creator deposit R2
  ✓ PASS: Circle 2: member  deposit R2
  ✓ PASS: Circle 3: creator deposit R2
  ✓ PASS: Circle 3: member  deposit R2
  ✓ PASS: Circle 4: creator deposit R2
  ✓ PASS: Circle 4: member  deposit R2
  ✓ PASS: Circle 5: creator deposit R2 (member still missing)

  … sleeping 22s for R2 of all circles (cron tick, distribution round) (cycle+grace gate)
[17] PROCESS_PAYOUT R2 (distribution round, Total semantics)
  ✓ PASS: Circle 1: ProcessPayout R2 (cycle completes)
  ✓ PASS: Circle 2: ProcessPayout R2 (cycle completes)
  ✓ PASS: Circle 3: ProcessPayout R2 (cycle completes)
  ✓ PASS: Circle 4: ProcessPayout R2 (cycle completes)
  ✓ PASS: Circle 5: ProcessPayout R2 (cycle completes)

[18] CIRCLE 1: SECOND CYCLE (R3→R4) — leaves circle 1 Running at cycle 3 R1
  Circle 1 status after R2: Running
  ✓ PASS: Circle 1: creator deposit R3
  ✓ PASS: Circle 1: member  deposit R3
  … sleeping 22s for Circle 1 R3 (cycle+grace gate)
  ✓ PASS: Circle 1: AdvanceRound R3→R4
  ✓ PASS: Circle 1: creator deposit R4
  ✓ PASS: Circle 1: member  deposit R4
  … sleeping 22s for Circle 1 R4 (cycle 2 distribution) (cycle+grace gate)
  ✓ PASS: Circle 1: ProcessPayout R4 (cycle 2 distribution)

[19] WITHDRAW pending payouts
  ✓ PASS: Withdraw circle 1 (creator, pending=400000)
  ✓ PASS: Withdraw circle 1 (member, pending=400000)
  ✓ PASS: Withdraw circle 2 (creator, pending=400000)
  ✓ PASS: Withdraw circle 2 (member, pending=300000)
  ↳ skip Withdraw circle 3 (creator): no pending payout
  ✓ PASS: Withdraw circle 3 (member, pending=200000)
  ✓ PASS: Withdraw circle 4 (creator, pending=400000)
  ✓ PASS: Withdraw circle 4 (member, pending=300000)
  ✓ PASS: Withdraw circle 5 (creator, pending=390000)
  ↳ skip Withdraw circle 5 (member): no pending payout

[20] CHECK EJECTION (circle 5)
  Circle 5 status: Completed, members=1
  ✓ Member auto-ejected during deposits/payout

[21] BLOCK MEMBER + DISTRIBUTE BLOCKED FUNDS (target: circle 8)
  Circle 8 status: Running, members=2
  ✓ PASS: Block member in circle 8
  ✓ PASS: GetBlockedMembers(8) shows the blocked member
  ✓ PASS: DistributeBlockedFunds(8, cycle=2) rejected (no active depositors at cycle)

[22] CANCEL AFTER START — expect fail
  Targeting circle 1 (status=Running cycles_completed=2)
  ✓ PASS: Cancel circle 1 after distribution rejected

[23] EXIT AFTER START (apply penalty)
  ✓ PASS: Exit circle 1 as member (after start, penalty applied)

[24] EMERGENCY STOP
  ✓ PASS: EmergencyStop circle 8 (was Running)

[25] COMPREHENSIVE QUERIES
  ✓ PASS: GetCircle(1)
  ✓ PASS: GetCircles(limit=10)
  ✓ PASS: GetCircles(status=Running)
  ✓ PASS: GetCircles(by creator)
  ✓ PASS: GetCircleMembers(1)
  ✓ PASS: GetCircleStatus(1)
  ✓ PASS: GetCurrentCycle(1)
  ✓ PASS: GetCycleDeposits(1,1)
  ✓ PASS: GetMemberDeposits
  ✓ PASS: GetPayouts(1)
  ✓ PASS: GetPayoutHistory(1)
  ✓ PASS: GetPayoutHistory(1,cycle=1)
  ✓ PASS: GetCircleBalance(1)
  ✓ PASS: GetMemberBalance
  ✓ PASS: GetPenalties(1)
  ✓ PASS: GetPenalties(1, member)
  ✓ PASS: GetRefunds(1)
  ✓ PASS: GetPendingPayout
  ✓ PASS: GetMemberAccumLateFees
  ✓ PASS: GetDepositRequirement
  ✓ PASS: GetEvents(1)
  ✓ PASS: GetCircleStats(1)
  ✓ PASS: GetMemberStats
  ✓ PASS: GetMemberLockedAmount
  ✓ PASS: GetBlockedMembers(1)
  ✓ PASS: GetBlockedMembers(3)
  ✓ PASS: GetMemberPseudonym(3)
  ✓ PASS: GetPrivateMembers(3)
  ✓ PASS: GetDistributionCalendar(1)
  ✓ PASS: GetArchivedDate(1)
  ✓ PASS: GetContractVersion

[26] FINAL STATUS SNAPSHOT
  Circle 1: status=Running, members=1, round=5, cycles_completed=2, pending=0
  Circle 2: status=Completed, members=2, round=2, cycles_completed=1, pending=0
  Circle 3: status=Running, members=2, round=3, cycles_completed=1, pending=0
  Circle 4: status=Completed, members=2, round=2, cycles_completed=1, pending=0
  Circle 5: status=Completed, members=1, round=2, cycles_completed=1, pending=0
  Circle 6: status=Cancelled, members=2, round=0, cycles_completed=0, pending=0
  Circle 7: status=Cancelled, members=1, round=0, cycles_completed=0, pending=0
  Circle 8: status=Paused, members=2, round=2, cycles_completed=0, pending=0

════════════════════════════════════════════════════════════════
  ALL 125 TESTS PASSED ✓
════════════════════════════════════════════════════════════════
Contract: addr_safro1le7ef2vc67ugruurn0zyusxvwsd8kjqws5vct57kpfrxye0krleqk6ysqp


```
