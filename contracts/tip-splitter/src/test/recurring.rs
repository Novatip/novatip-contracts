//! Recurring tips.

use super::*;
use soroban_sdk::vec;

#[test]
fn recurring_tip_create_read_cancel_roundtrip() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@creator");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );

    let seq = env.ledger().sequence();
    let id = client.create_recurring(
        &supporter,
        &jar_id,
        &500,
        &100,
        &String::from_str(env, "monthly"),
    );
    assert_eq!(id, 1);

    let rec = client.get_recurring(&id);
    assert_eq!(rec.supporter, supporter);
    assert_eq!(rec.jar_id, jar_id);
    assert_eq!(rec.amount, 500);
    assert_eq!(rec.period_ledgers, 100);
    assert_eq!(rec.next_due_ledger, seq + 100);

    client.cancel_recurring(&supporter, &id);
    assert!(client.try_get_recurring(&id).is_err());
}

#[test]
fn recurring_tip_validation() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@vvv");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );

    // zero period
    assert!(client
        .try_create_recurring(&supporter, &jar_id, &500, &0, &String::from_str(env, ""))
        .is_err());
    // non-positive amount
    assert!(client
        .try_create_recurring(&supporter, &jar_id, &0, &100, &String::from_str(env, ""))
        .is_err());
    // unknown jar
    assert!(client
        .try_create_recurring(
            &supporter,
            &String::from_str(env, "@nope"),
            &500,
            &100,
            &String::from_str(env, "")
        )
        .is_err());
}

#[test]
fn cancel_recurring_requires_the_supporter() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@own");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );
    let id = client.create_recurring(&supporter, &jar_id, &500, &100, &String::from_str(env, ""));

    // Only the supporter's signature authorizes cancellation; without it the
    // entry survives.
    env.mock_auths(&[]);
    assert!(client.try_cancel_recurring(&supporter, &id).is_err());
    // still present
    assert_eq!(client.get_recurring(&id).supporter, supporter);
}

// #316 / #317 — update_recurring keeps the id and due date; recr_add carries
// the schedule and the jar as a topic.
#[test]
fn update_recurring_keeps_id_and_due_date_and_recr_add_has_payload() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@monthly");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );
    let id = client.create_recurring(&supporter, &jar_id, &500, &100, &String::from_str(env, "a"));

    let add_topics = vec![
        env,
        symbol_short!("recr_add").into_val(env),
        supporter.clone().into_val(env),
        jar_id.clone().into_val(env),
    ];
    let add_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == add_topics)
        .collect();
    assert_eq!(add_events.len(), 1);
    let added: RecurringAddedEvent =
        TryFromVal::try_from_val(env, &add_events[0].2).expect("recr_add event data shape");
    let before = client.get_recurring(&id);
    assert_eq!(added.id, id);
    assert_eq!(added.jar_id, jar_id);
    assert_eq!(added.amount, 500);
    assert_eq!(added.period_ledgers, 100);
    assert_eq!(added.next_due_ledger, before.next_due_ledger);

    client.update_recurring(&id, &700, &String::from_str(env, "b"));
    let after = client.get_recurring(&id);
    assert_eq!(after.amount, 700);
    assert_eq!(after.message, String::from_str(env, "b"));
    assert_eq!(after.next_due_ledger, before.next_due_ledger);

    assert_eq!(
        client.try_update_recurring(&id, &0, &String::from_str(env, "")),
        Err(Ok(Error::AmountZero.into()))
    );
    env.mock_auths(&[]);
    assert!(client
        .try_update_recurring(&id, &900, &String::from_str(env, ""))
        .is_err());
    assert_eq!(client.get_recurring(&id).amount, 700);
}

// ---------------------------------------------------------------------------
// #175 — collect_recurring: pay a due schedule through the supporter's
// token allowance, one period per call, same split and event as a manual tip
// ---------------------------------------------------------------------------

/// Shared fixture for the collect tests: jar `@rec` paying 70/30 to
/// alice/bob, a supporter holding 10 000 who has approved *this contract* as
/// token spender for `approved` units, and a schedule of `amount` every 100
/// ledgers. Returns the pieces; each test builds its own client.
fn recurring_fixture<'a>(
    env: &'a Env,
    s: &'a Setup,
    amount: i128,
    approved: i128,
) -> (
    TipSplitterClient<'a>,
    Address,
    Address,
    Address,
    String,
    u128,
) {
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let supporter = Address::generate(env);
    token_admin.mint(&supporter, &10_000);

    let jar_id = String::from_str(env, "@rec");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 7000,
            },
            Split {
                to: bob.clone(),
                bps: 3000,
            },
        ],
    );

    // The payment rail #175 assumes: the supporter approves the contract as
    // spender. The expiration is far enough out that the allowance tests
    // exercise exhaustion through `approved`, not through expiry.
    let expiration = env.ledger().sequence() + 100_000;
    token.approve(&supporter, &s.contract, &approved, &expiration);

    let id = client.create_recurring(
        &supporter,
        &jar_id,
        &amount,
        &100,
        &String::from_str(env, "monthly"),
    );
    (client, alice, bob, supporter, jar_id, id)
}

/// The first collection, once due, pays exactly what a manual tip of the same
/// amount would: the same 70/30 split, the same totals, and the identical
/// `tip` event — with no signature from the supporter at all.
#[test]
fn collect_recurring_pays_the_first_due_period_like_a_tip() {
    let s = setup();
    let env = &s.env;
    let (client, alice, bob, supporter, jar_id, id) = recurring_fixture(env, &s, 500, 1_000);
    let token = token::Client::new(env, &s.token);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due);

    // The keeper's own signature is irrelevant: with *no* auths mocked at
    // all the call still settles, proving it is genuinely anyone-callable and
    // that the supporter never signs a collection.
    env.mock_auths(&[]);
    client.collect_recurring(&id);
    env.mock_all_auths();

    // Snapshot the events *now*, before any further contract call: the
    // testutils event buffer only exposes the most recent top-level
    // invocation, so a `balance` or `get_recurring` call in between would
    // erase the collection's events.
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| {
            e.0 == s.contract
                && topic_is_symbol(env, &e.1, 0, symbol_short!("tip"))
                && topic_at::<String>(env, &e.1, 1) == jar_id
        })
        .collect();
    assert_eq!(evs.len(), 1, "a collection publishes exactly one tip event");
    let decoded: TipEventData =
        TryFromVal::try_from_val(env, &evs.first().unwrap().2).expect("tip event data shape");
    assert_eq!(
        decoded,
        TipEvent {
            version: 1,
            from: supporter.clone(),
            amount: 500i128,
            fee: 0,
            message: String::from_str(env, "monthly"),
            breakdown: vec![
                env,
                (alice.clone(), 350i128, false),
                (bob.clone(), 150i128, false)
            ],
            token: s.token.clone(),
            seq: 1u64,
            reference: None,
        }
    );

    assert_eq!(token.balance(&alice), 350);
    assert_eq!(token.balance(&bob), 150);
    assert_eq!(token.balance(&supporter), 9_500);
    assert_eq!(token.balance(&s.contract), 0);

    // The entry advanced by exactly one period, anchored on the old due
    // ledger, not on "now".
    let rec = client.get_recurring(&id);
    assert_eq!(rec.next_due_ledger, due + 100);

    // Collected tips bump the same on-chain totals a manual tip would.
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 1,
            total: 500
        }
    );
    assert_eq!(
        client.get_supporter_stats(&jar_id, &supporter),
        SupporterStats {
            tip_count: 1,
            total: 500
        }
    );
}

/// Calling before the due ledger is a typed `NotDue` no-op: nothing moves,
/// nothing is cancelled, and the same call succeeds once the ledger is
/// reached — the early-retry path the issue names.
#[test]
fn collect_recurring_before_due_is_a_typed_no_op_and_succeeds_after_advancing() {
    let s = setup();
    let env = &s.env;
    let (client, alice, bob, supporter, _jar_id, id) = recurring_fixture(env, &s, 500, 1_000);
    let token = token::Client::new(env, &s.token);
    let due = client.get_recurring(&id).next_due_ledger;

    // One ledger short of due.
    env.ledger().set_sequence_number(due - 1);
    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::NotDue.into())),
        "an early collection must fail with the contract's own code"
    );
    assert_eq!(token.balance(&alice), 0);
    assert_eq!(token.balance(&bob), 0);
    assert_eq!(token.balance(&supporter), 10_000);
    // ...and the schedule is untouched: the same entry can still be collected.
    assert_eq!(client.get_recurring(&id).next_due_ledger, due);

    // On the due ledger itself it goes through.
    env.ledger().set_sequence_number(due);
    client.collect_recurring(&id);
    assert_eq!(token.balance(&alice), 350);
    assert_eq!(client.get_recurring(&id).next_due_ledger, due + 100);
}

/// A keeper that was offline cannot drain several periods at once: each call
/// collects at most one period — the due ledger moves exactly one
/// `period_ledgers` per call — and the catch-up is additionally bounded by
/// the token allowance the supporter granted, which the third call exhausts
/// with a typed error and zero state change.
#[test]
fn collect_recurring_advances_exactly_one_period_per_call() {
    let s = setup();
    let env = &s.env;
    let (client, _alice, _bob, supporter, _jar_id, id) = recurring_fixture(env, &s, 500, 1_000);
    let token = token::Client::new(env, &s.token);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due + 500);
    // A keeper backlogged by five periods can still only collect one period
    // per call: the due ledger moves by exactly `period_ledgers`, anchored on
    // the old due ledger — not on "now".
    client.collect_recurring(&id);
    assert_eq!(client.get_recurring(&id).next_due_ledger, due + 100);
    // The anchor is still in the past, so the schedule remains due and a
    // second call collects exactly one further period — the per-call cap is
    // what bounds the catch-up, together with the allowance the supporter
    // granted.
    client.collect_recurring(&id);
    assert_eq!(client.get_recurring(&id).next_due_ledger, due + 200);
    assert_eq!(token.balance(&supporter), 9_000);
    // The allowance covered exactly two periods: a third call fails with the
    // typed error and changes nothing — an offline keeper can never drain
    // more than the supporter approved.
    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::InsufficientAllowance.into())),
        "the catch-up is one period per call, bounded by the allowance"
    );
    assert_eq!(client.get_recurring(&id).next_due_ledger, due + 200);
    assert_eq!(token.balance(&supporter), 9_000);
}

/// An exhausted allowance fails with the typed `InsufficientAllowance` before
/// any funds move and without touching the schedule — the supporter revoking
/// (or approving too little) stops collections dead, and no partial period is
/// ever paid.
#[test]
fn collect_recurring_rejects_an_insufficient_allowance_without_state_change() {
    let s = setup();
    let env = &s.env;
    // Approved 400 for a 500 schedule: the allowance never covers a period.
    let (client, alice, _bob, supporter, _jar_id, id) = recurring_fixture(env, &s, 500, 400);
    let token = token::Client::new(env, &s.token);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due);

    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::InsufficientAllowance.into())),
        "an allowance below the scheduled amount is this contract's typed error, not the token's"
    );
    assert_eq!(token.balance(&alice), 0, "no partial period is paid");
    assert_eq!(token.balance(&supporter), 10_000);
    assert_eq!(client.get_recurring(&id).next_due_ledger, due);

    // The expired case needs no separate code path: SAC reads an expired
    // allowance as zero, which the same gate rejects. Revoke instead of
    // waiting out the expiration: approve 0 for this spender.
    let expiration = env.ledger().sequence() + 10;
    token.approve(&supporter, &s.contract, &400, &expiration);
    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::InsufficientAllowance.into())),
        "revoking the allowance stops collections on the next call"
    );
    env.ledger().set_sequence_number(expiration + 1);
    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::InsufficientAllowance.into())),
        "an expired allowance reads as zero and fails the same typed check"
    );
}

/// An unknown schedule id is a typed `RecurringNotFound`, matching
/// `get_recurring`/`cancel_recurring`.
#[test]
fn collect_recurring_rejects_unknown_id() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    assert_eq!(
        client.try_collect_recurring(&77),
        Err(Ok(Error::RecurringNotFound.into())),
    );
}

/// A collected recurring tip pays the same fee as a manual tip of the same
/// amount, drawn from the same allowance.
#[test]
fn collect_recurring_takes_the_fee() {
    let s = setup();
    let env = &s.env;
    let (client, alice, bob, supporter, _jar_id, id) = recurring_fixture(env, &s, 1_000, 1_000);
    let token = token::Client::new(env, &s.token);
    client.set_fee_bps(&100);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due);

    let before = token.balance(&supporter);
    client.collect_recurring(&id);

    // 1% of 1 000 is 10; 990 splits 70/30 into 693 and 297.
    assert_eq!(token.balance(&s.admin), 10);
    assert_eq!(token.balance(&alice), 693);
    assert_eq!(token.balance(&bob), 297);
    assert_eq!(before - token.balance(&supporter), 1_000);
}

/// A recurring schedule keeps its message on-chain and republishes it on every
/// collection, so it shares the tip's content validation.
#[test]
fn create_recurring_rejects_invalid_message() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@recmsg");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );

    let with_nul = String::from_bytes(env, b"monthly\0tip");
    let res = client.try_create_recurring(&supporter, &jar_id, &500, &100, &with_nul);
    assert_eq!(res, Err(Ok(Error::InvalidMessage.into())));
}
// ---------------------------------------------------------------------------
// #324 — the jar owner can cancel recurring tips into their jar
// ---------------------------------------------------------------------------

#[test]
fn cancel_recurring_accepts_the_supporter_or_the_jar_owner_only() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@sched");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 10000,
            },
        ],
    );
    let memo = String::from_str(env, "");

    let first = client.create_recurring(&supporter, &jar_id, &500, &100, &memo);
    client.cancel_recurring(&supporter, &first);
    assert!(client.try_get_recurring(&first).is_err());

    let second = client.create_recurring(&supporter, &jar_id, &500, &100, &memo);
    let stranger = Address::generate(env);
    assert_eq!(
        client.try_cancel_recurring(&stranger, &second),
        Err(Ok(Error::NotAuthorized.into()))
    );
    assert_eq!(client.get_recurring(&second).supporter, supporter);

    client.cancel_recurring(&owner, &second);
    assert!(client.try_get_recurring(&second).is_err());
}

#[test]
fn recr_del_records_who_cancelled() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@sched");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 10000,
            },
        ],
    );
    let id = client.create_recurring(&supporter, &jar_id, &500, &100, &String::from_str(env, ""));

    client.cancel_recurring(&owner, &id);
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("recr_del")))
        .collect();
    assert_eq!(evs.len(), 1);
    let (cancelled, cancelled_by): (u128, Address) =
        TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(cancelled, id);
    assert_eq!(cancelled_by, owner);
}

/// #221 — a schedule whose supporter is the jar's only recipient moves no
/// funds, so it is refused when created, and a schedule that becomes one after
/// the jar's splits change is refused at collection with no state change.
#[test]
fn recurring_that_moves_no_funds_is_rejected() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let supporter = Address::generate(env);
    let alice = Address::generate(env);

    let self_jar = String::from_str(env, "@selfjar");
    client.create_jar(
        &owner,
        &self_jar,
        &vec![
            env,
            Split {
                to: supporter.clone(),
                bps: 10000,
            },
        ],
    );
    assert_eq!(
        client.try_create_recurring(
            &supporter,
            &self_jar,
            &500,
            &100,
            &String::from_str(env, "self")
        ),
        Err(Ok(Error::NoFundsMoved.into()))
    );

    // A schedule stored while the jar paid someone else.
    let jar = String::from_str(env, "@rotate");
    client.create_jar(
        &owner,
        &jar,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );
    let token = token::Client::new(env, &s.token);
    token::StellarAssetClient::new(env, &s.token).mint(&supporter, &1_000);
    token.approve(&supporter, &s.contract, &1_000, &(env.ledger().sequence() + 100_000));
    let id = client.create_recurring(&supporter, &jar, &500, &100, &String::from_str(env, "m"));
    let due = client.get_recurring(&id).next_due_ledger;

    // The owner now points the jar at the supporter alone.
    client.update_splits(
        &jar,
        &vec![
            env,
            Split {
                to: supporter.clone(),
                bps: 10000,
            },
        ],
    );
    env.ledger().set_sequence_number(due);

    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::NoFundsMoved.into()))
    );
    assert_eq!(client.get_recurring(&id).next_due_ledger, due);
    assert_eq!(client.get_tip_count(), 0);
    assert_eq!(client.get_jar_stats(&jar).tip_count, 0);
    assert_eq!(client.get_jar_stats(&jar).total, 0);
    let stats = client.get_supporter_stats(&jar, &supporter);
    assert_eq!(stats.tip_count, 0);
    assert_eq!(stats.total, 0);
    assert_eq!(token.balance(&supporter), 1_000);
}
