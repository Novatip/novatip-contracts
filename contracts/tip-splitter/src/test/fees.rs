//! The protocol fee.

use super::*;
use soroban_sdk::vec;

// ===========================================================================
// #79 protocol fee
// ===========================================================================

/// A 70/30 jar between two fresh recipients, with `tipper` funded by `mint`.
fn fee_jar(s: &Setup, jar_id: &str, mint: i128) -> (String, Address, Address, Address) {
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &mint);

    let jar_id = String::from_str(env, jar_id);
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
    (jar_id, alice, bob, tipper)
}

/// No deployment starts charging on upgrade: the fee is 0 until the admin
/// sets one, and a tip then pays the recipients the whole amount.
#[test]
fn fee_defaults_to_zero_and_takes_nothing() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let (jar_id, alice, bob, tipper) = fee_jar(&s, "@fee_default", 10_000);

    assert_eq!(client.get_fee_bps(), 0);
    client.tip(&tipper, &jar_id, &10_000, &String::from_str(env, "free"));

    assert_eq!(token.balance(&alice), 7_000);
    assert_eq!(token.balance(&bob), 3_000);
    assert_eq!(token.balance(&s.admin), 0);
}

/// `set_fee_bps` stores the value `get_fee_bps` reports, accepts the 500 bps
/// cap itself, and rejects anything above it with `FeeTooHigh`, leaving the
/// stored fee unchanged.
#[test]
fn set_fee_bps_is_capped_at_five_percent() {
    let s = setup();
    let client = TipSplitterClient::new(&s.env, &s.contract);

    client.set_fee_bps(&100);
    assert_eq!(client.get_fee_bps(), 100);

    client.set_fee_bps(&MAX_FEE_BPS);
    assert_eq!(client.get_fee_bps(), 500);

    assert_eq!(
        client.try_set_fee_bps(&501),
        Err(Ok(Error::FeeTooHigh.into()))
    );
    assert_eq!(
        client.get_fee_bps(),
        500,
        "a rejected fee must not be stored"
    );

    client.set_fee_bps(&0);
    assert_eq!(client.get_fee_bps(), 0, "0 turns the fee off again");
}

/// Only the admin may set the fee. Paired with a positive control using the
/// same builder and the admin as signer.
#[test]
fn set_fee_bps_requires_admin_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let stranger = Address::generate(env);

    let mock_signer = |signer: &Address| {
        env.mock_auths(&[MockAuth {
            address: signer,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "set_fee_bps",
                args: (100u32,).into_val(env),
                sub_invokes: &[],
            },
        }]);
    };

    mock_signer(&stranger);
    assert!(
        client.try_set_fee_bps(&100).is_err(),
        "a non-admin must not be able to set the protocol fee"
    );
    assert_eq!(client.get_fee_bps(), 0);

    mock_signer(&s.admin);
    client.set_fee_bps(&100);
    assert_eq!(client.get_fee_bps(), 100);
}

/// `set_fee_bps` publishes `fee_set` in the shared admin shape, so a client
/// can track the fee from the event log.
#[test]
fn set_fee_bps_emits_fee_set_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    client.set_fee_bps(&100);

    let expected_topics = vec![env, symbol_short!("fee_set").into_val(env)];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1);
    let fee: FeeEvent = TryFromVal::try_from_val(env, &events[0].2).unwrap();
    assert_eq!(fee.version, 1);
    assert_eq!(fee.admin, s.admin);
    assert_eq!(fee.fee_bps, 100);
}

/// The configured 1%: the fee comes off the top and goes to the admin, the
/// recipients split what is left, and the recipients' total plus the fee is
/// the tip exactly. The `tip` event reports the fee next to the breakdown.
#[test]
fn tip_takes_the_fee_before_the_split() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let (jar_id, alice, bob, tipper) = fee_jar(&s, "@fee_one_pct", 10_000);

    client.set_fee_bps(&100);
    // Previews already report post-fee shares, so the fee a client shows is
    // the tip minus their sum.
    let preview = i128_vec_to_std(&client.preview_split(&jar_id, &10_000));
    assert_eq!(preview, std::vec![6_930, 2_970]);
    assert_eq!(10_000 - preview.iter().sum::<i128>(), 100);

    client.tip(&tipper, &jar_id, &10_000, &String::from_str(env, "1%"));
    let decoded = last_tip_event(env, &s.contract, &jar_id);

    // 1% of 10 000 is 100; 9 900 splits 70/30.
    assert_eq!(token.balance(&s.admin), 100);
    assert_eq!(token.balance(&alice), 6_930);
    assert_eq!(token.balance(&bob), 2_970);
    assert_eq!(token.balance(&tipper), 0);
    assert_eq!(
        token.balance(&alice) + token.balance(&bob) + token.balance(&s.admin),
        10_000,
        "recipients plus fee must equal the tip exactly"
    );

    assert_eq!(decoded.amount, 10_000);
    assert_eq!(decoded.fee, 100);
    assert_eq!(
        decoded.breakdown,
        vec![env, (alice, 6_930i128, false), (bob, 2_970i128, false)]
    );
}

/// The fee truncates and the last recipient absorbs the split's own dust, so
/// an awkward amount still balances to the unit.
#[test]
fn fee_and_dust_rounding_still_account_for_every_unit() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let (jar_id, alice, bob, tipper) = fee_jar(&s, "@fee_dust", 10_099);

    client.set_fee_bps(&100);
    client.tip(&tipper, &jar_id, &10_099, &String::from_str(env, "odd"));

    // fee = floor(10 099 * 100 / 10 000) = 100; net = 9 999;
    // alice = floor(9 999 * 0.7) = 6 999; bob takes the remaining 3 000.
    assert_eq!(token.balance(&s.admin), 100);
    assert_eq!(token.balance(&alice), 6_999);
    assert_eq!(token.balance(&bob), 3_000);
    assert_eq!(token.balance(&tipper), 0);
}

/// The too-small-to-split guard applies to the post-fee amount: a tip whose
/// gross clears it but whose net does not is rejected, and
/// `get_min_accepted_tip` reports the first gross amount that works.
#[test]
fn split_guard_and_min_accepted_tip_apply_after_the_fee() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);
    let jar_id = String::from_str(env, "@fee_guard");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 100,
            },
            Split {
                to: Address::generate(env),
                bps: 9_900,
            },
        ],
    );
    // A 1% share needs a net of at least 100.
    assert_eq!(client.get_min_accepted_tip(&jar_id), 100);

    client.set_fee_bps(&MAX_FEE_BPS);
    // 104 - floor(104 * 5%) = 99, which truncates the 1% share to zero;
    // 105 - floor(105 * 5%) = 100, the first gross amount that pays everyone.
    assert_eq!(client.get_min_accepted_tip(&jar_id), 105);
    let msg = String::from_str(env, "edge");
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &104, &msg),
        Err(Ok(Error::SplitAmountTooSmall.into()))
    );
    assert_eq!(
        client.try_preview_split(&jar_id, &104),
        Err(Ok(Error::SplitAmountTooSmall.into()))
    );
    assert!(client.try_tip(&tipper, &jar_id, &105, &msg).is_ok());
}

/// The fee rule follows the self-transfer rule: an admin tipping keeps the
/// fee rather than paying themselves, and the event reports a fee of 0
/// because nothing moved.
#[test]
fn admin_tipping_keeps_the_fee() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let (jar_id, alice, bob, _) = fee_jar(&s, "@fee_admin", 0);
    token_admin.mint(&s.admin, &10_000);

    client.set_fee_bps(&100);
    client.tip(&s.admin, &jar_id, &10_000, &String::from_str(env, "mine"));
    let decoded = last_tip_event(env, &s.contract, &jar_id);

    assert_eq!(token.balance(&alice), 6_930);
    assert_eq!(token.balance(&bob), 2_970);
    assert_eq!(token.balance(&s.admin), 100);
    assert_eq!(decoded.fee, 0);
}
