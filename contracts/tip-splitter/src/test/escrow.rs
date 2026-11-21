//! Escrowed shares of recipients who cannot receive the token (#171).

use super::*;
use soroban_sdk::vec;

// ===========================================================================
// #171 hold undeliverable shares · #172 leave_jar · #173 lock_splits · #174 recurring
// ===========================================================================

/// Reads the raw per-recipient `tip` breakdown including the `held` flag.
fn raw_tip_breakdown(
    env: &Env,
    contract: &Address,
    jar_id: &String,
) -> std::vec::Vec<(Address, i128, bool)> {
    let expected_topics = vec![
        env,
        symbol_short!("tip").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == *contract && e.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1, "expected exactly one tip event");
    let (_, _, data) = events.first().unwrap();
    let decoded: TipEventData = TryFromVal::try_from_val(env, data).expect("tip event data shape");
    let breakdown = decoded.breakdown;
    (0..breakdown.len())
        .map(|i| breakdown.get(i).unwrap())
        .collect()
}

#[test]
fn tip_holds_share_of_deauthorised_recipient_and_withdraw_pays_it() {
    let s = setup_hostile();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = TestTokenClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@band");
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

    // Bob can no longer receive the token.
    token.block(&bob);
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "hi"));

    // Read the tip event immediately: the test env only keeps the last
    // invocation's events, and the balance/get_held checks below are new calls.
    let bd = raw_tip_breakdown(env, &s.contract, &jar_id);
    assert_eq!(bd[0], (alice.clone(), 70i128, false));
    assert_eq!(bd[1], (bob.clone(), 30i128, true));

    // Alice is paid; Bob is not, but the tip did NOT revert.
    assert_eq!(token.balance(&alice), 70);
    assert_eq!(token.balance(&bob), 0);
    // Bob's share is escrowed in the contract and reported as held.
    assert_eq!(token.balance(&s.contract), 30);
    assert_eq!(client.get_held(&bob), 30);
    // The tipper is debited the full amount regardless.
    assert_eq!(token.balance(&tipper), 900);

    // Bob recovers; he withdraws his held share.
    token.unblock();
    client.withdraw_held(&bob);
    let expected_topics = vec![
        env,
        symbol_short!("withdraw").into_val(env),
        bob.clone().into_val(env),
    ];
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(evs.len(), 1);
    let decoded: HeldWithdrawnEvent =
        TryFromVal::try_from_val(env, &evs[0].2).expect("withdraw event data shape");
    assert_eq!(decoded.version, 1);
    assert_eq!(decoded.amount, 30);
    assert_eq!(token.balance(&bob), 30);
    assert_eq!(token.balance(&s.contract), 0);
    assert_eq!(client.get_held(&bob), 0);

    // Nothing left to withdraw.
    assert!(client.try_withdraw_held(&bob).is_err());
}

#[test]
fn withdraw_held_requires_the_recipients_own_authorisation() {
    let s = setup_hostile();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = TestTokenClient::new(env, &s.token);

    let owner = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@solo");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );
    token.block(&bob);
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, ""));
    assert_eq!(client.get_held(&bob), 100);

    // `withdraw_held` is gated on the recipient's own signature: with no auth
    // granted, the call fails and the escrow is untouched.
    env.mock_auths(&[]);
    assert!(client.try_withdraw_held(&bob).is_err());
    assert_eq!(client.get_held(&bob), 100);
}

// ---------------------------------------------------------------------------
// #261 — withdraw_held edge cases
// ---------------------------------------------------------------------------

#[test]
fn withdraw_held_edge_cases_accumulate_retry_pause_and_double_withdraw() {
    let s = setup_hostile();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = TestTokenClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token.mint(&tipper, &1_000);

    let jar_one = String::from_str(env, "@held_one");
    let jar_two = String::from_str(env, "@held_two");

    client.create_jar(
        &owner,
        &jar_one,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 5_000,
            },
            Split {
                to: bob.clone(),
                bps: 5_000,
            },
        ],
    );
    client.create_jar(
        &owner,
        &jar_two,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10_000,
            },
        ],
    );

    // Block Bob and accumulate held credit across multiple tips and jars.
    token.block(&bob);
    client.tip(&tipper, &jar_one, &100, &String::from_str(env, "t1")); // +50 held
    client.tip(&tipper, &jar_one, &200, &String::from_str(env, "t2")); // +100 held
    client.tip(&tipper, &jar_two, &75, &String::from_str(env, "t3")); // +75 held

    assert_eq!(token.balance(&alice), 150);
    assert_eq!(client.get_held(&bob), 225);
    assert_eq!(token.balance(&s.contract), 225);

    // Withdrawing while Bob still cannot receive fails and leaves `get_held` intact.
    assert!(client.try_withdraw_held(&bob).is_err());
    assert_eq!(client.get_held(&bob), 225);
    assert_eq!(token.balance(&bob), 0);

    // Pause the contract: per docs/CONTRACT.md, admin cannot stop `withdraw_held` by pausing.
    client.pause();
    assert!(client.is_paused());

    // Unblock Bob and withdraw while paused; assert the `withdraw` event.
    token.unblock();
    client.withdraw_held(&bob);
    let expected_topics = vec![
        env,
        symbol_short!("withdraw").into_val(env),
        bob.clone().into_val(env),
    ];
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(evs.len(), 1);
    let decoded: HeldWithdrawnEvent =
        TryFromVal::try_from_val(env, &evs[0].2).expect("withdraw event data");
    assert_eq!(decoded.version, 1);
    assert_eq!(decoded.amount, 225);

    assert_eq!(token.balance(&bob), 225);
    assert_eq!(token.balance(&s.contract), 0);
    assert_eq!(client.get_held(&bob), 0);

    // Withdrawing a second time fails with `NothingHeld`.
    assert_eq!(
        client.try_withdraw_held(&bob),
        Err(Ok(Error::NothingHeld.into()))
    );
}
