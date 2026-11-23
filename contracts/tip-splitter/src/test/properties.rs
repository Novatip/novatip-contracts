//! Property tests.

use super::*;
use soroban_sdk::vec;

/// Config for every property test: a fixed seed, so the snapshots their envs
/// write are identical on every run and `cargo test` leaves the checkout clean
/// (#264). Set `PROPTEST_RNG_SEED` to run a different set of cases.
pub(super) fn snapshot_stable(cases: u32) -> ProptestConfig {
    let mut config = ProptestConfig::with_cases(cases);
    if std::env::var_os("PROPTEST_RNG_SEED").is_none() {
        config.rng_seed = proptest::test_runner::RngSeed::Fixed(0x6e6f_7661_7469_7073);
    }
    config
}

/// A random valid bps distribution over `n` recipients: every share is at
/// least 1 and the shares sum to exactly `BPS_DENOM`, mirroring what
/// `validate_splits` requires.
fn valid_bps_distribution(n: u32) -> impl Strategy<Value = std::vec::Vec<u32>> {
    proptest::collection::vec(1u64..=1_000_000u64, n as usize).prop_map(move |weights| {
        let remaining = (BPS_DENOM - n) as u64;
        let sum_w: u64 = weights.iter().sum();
        let mut bps = std::vec::Vec::with_capacity(n as usize);
        let mut used = 0u64;
        for w in weights.iter().take(n as usize - 1) {
            let extra = if sum_w == 0 { 0 } else { w * remaining / sum_w };
            used += extra;
            bps.push(1 + extra as u32);
        }
        bps.push(1 + (remaining - used) as u32);
        bps
    })
}

fn splits_strategy() -> impl Strategy<Value = std::vec::Vec<u32>> {
    (1u32..=MAX_RECIPIENTS).prop_flat_map(valid_bps_distribution)
}

/// Random splits plus, about half the time, the index of the one share the
/// tipper themselves holds (#78). `None` means every recipient is a fresh
/// address, as the property test originally assumed.
fn splits_with_optional_tipper() -> impl Strategy<Value = (std::vec::Vec<u32>, Option<usize>)> {
    splits_strategy().prop_flat_map(|bps| {
        let n = bps.len();
        (Just(bps), proptest::option::of(0..n))
    })
}

proptest! {
    #![proptest_config(snapshot_stable(64))]

    /// The core invariant, stated so it holds whether or not the tipper is
    /// one of the recipients (#78):
    ///
    /// - every other recipient receives exactly their `preview_split` share,
    ///   dust included;
    /// - what leaves the tipper equals what those other recipients receive;
    /// - the tipper keeps their own share, so the two amounts above plus that
    ///   share add back to exactly the tipped amount.
    ///
    /// With no share for the tipper this is the original "payouts sum to the
    /// tip and the tipper's balance drops by all of it". With a share, a
    /// split that rolled the tipper's share into the last recipient would
    /// overpay that recipient and fail here.
    ///
    /// `amount` starts at 10 000: a share of 1 bps needs at least that much to
    /// avoid truncating to zero, which `tip` rejects with
    /// `SplitAmountTooSmall` before any transfer.
    #[test]
    fn tip_distributes_full_amount_across_random_splits(
        (bps, tipper_slot) in splits_with_optional_tipper(),
        amount in 10_000i128..=1_000_000_000i128,
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let token = token::Client::new(env, &s.token);
        let token_admin = token::StellarAssetClient::new(env, &s.token);

        let owner = Address::generate(env);
        let tipper = Address::generate(env);
        token_admin.mint(&tipper, &amount);

        let recipients: std::vec::Vec<Address> = (0..bps.len())
            .map(|i| {
                if tipper_slot == Some(i) {
                    tipper.clone()
                } else {
                    Address::generate(env)
                }
            })
            .collect();
        let mut splits = vec![env];
        for (addr, b) in recipients.iter().zip(bps.iter()) {
            splits.push_back(Split {
                to: addr.clone(),
                bps: *b,
            });
        }

        let jar_id = String::from_str(env, "@prop");
        client.create_jar(&owner, &jar_id, &splits);
        let shares = i128_vec_to_std(&client.preview_split(&jar_id, &amount));
        let message = String::from_str(env, "prop");
        if bps.len() == 1 && tipper_slot.is_some() {
            // The tipper is the only recipient, so nothing would move (#93).
            prop_assert_eq!(
                client.try_tip(&tipper, &jar_id, &amount, &message),
                Err(Ok(Error::NoFundsMoved.into()))
            );
            prop_assert_eq!(token.balance(&tipper), amount);
            return Ok(());
        }
        client.tip(&tipper, &jar_id, &amount, &message);

        let mut others_received: i128 = 0;
        let mut tipper_share: i128 = 0;
        for (i, r) in recipients.iter().enumerate() {
            if tipper_slot == Some(i) {
                tipper_share = shares[i];
            } else {
                prop_assert_eq!(token.balance(r), shares[i]);
                others_received += token.balance(r);
            }
        }
        let left_tipper = amount - token.balance(&tipper);

        prop_assert_eq!(left_tipper, others_received);
        prop_assert_eq!(others_received + tipper_share, amount);
        prop_assert_eq!(token.balance(&tipper), tipper_share);
    }
}

// ── Issue #136: small amounts are rejected, not silently mis-split ──────────

// Asserts that amounts below the minimum accepted value fail with
// `InvalidAmount` and leave every balance unchanged. The minimum is
// `ceil(BPS_DENOM / min_bps)` for the generated splits — below that,
// some recipient's share would truncate to zero.
proptest! {
    #![proptest_config(snapshot_stable(64))]

    #[test]
    fn tip_rejects_small_amounts(
        bps in splits_strategy(),
        small in 1i128..=20i128,
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let token = token::Client::new(env, &s.token);
        let token_admin = token::StellarAssetClient::new(env, &s.token);

        let owner = Address::generate(env);
        let tipper = Address::generate(env);
        token_admin.mint(&tipper, &small);

        let recipients: std::vec::Vec<Address> =
            bps.iter().map(|_| Address::generate(env)).collect();
        let mut splits = vec![env];
        for (addr, b) in recipients.iter().zip(bps.iter()) {
            splits.push_back(Split { to: addr.clone(), bps: *b });
        }

        let jar_id = String::from_str(env, "@small");
        client.create_jar(&owner, &jar_id, &splits);

        // Record balances before
        let tipper_before = token.balance(&tipper);
        let mut balances_before = std::vec::Vec::new();
        for r in recipients.iter() {
            balances_before.push(token.balance(r));
        }

        let result = client.try_tip(&tipper, &jar_id, &small, &String::from_str(env, "x"));

        // A share truncates to zero exactly when amount * bps < BPS_DENOM for
        // the smallest share. Only that case must be rejected.
        let min_bps = *bps.iter().min().unwrap() as i128;
        if small * min_bps < 10_000 {
            prop_assert!(result.is_err(), "a zero-truncated share must be rejected");

            // No balance changed
            prop_assert_eq!(token.balance(&tipper), tipper_before);
            for (r, before) in recipients.iter().zip(balances_before.iter()) {
                prop_assert_eq!(token.balance(r), *before);
            }
        } else {
            prop_assert!(result.is_ok());
        }
    }
}

fn model_validate_splits(splits: &std::vec::Vec<(u8, u32)>) -> Result<(), ModelError> {
    const MAX_RECIPIENTS: usize = 10;
    const BPS_DENOM: u32 = 10_000;
    if splits.is_empty() {
        return Err(ModelError::SplitsEmpty);
    }
    if splits.len() > MAX_RECIPIENTS {
        return Err(ModelError::TooManyRecipients);
    }
    for (_, bps) in splits {
        if *bps == 0 || *bps > BPS_DENOM {
            return Err(ModelError::SplitOutOfRange);
        }
    }
    let mut seen = std::collections::HashSet::new();
    for (idx, _) in splits {
        // The Soroban vector maps indices into a pool of four addresses, so
        // duplicates are on `idx % 4`, not the raw index.
        if !seen.insert(*idx % 4) {
            return Err(ModelError::DuplicateRecipient);
        }
    }
    let sum: u32 = splits.iter().map(|(_, b)| *b).sum();
    if sum != BPS_DENOM {
        return Err(ModelError::SplitSumNot100Pct);
    }
    Ok(())
}

proptest! {
    #![proptest_config(snapshot_stable(64))]

    /// Compares \`try_create_jar\` against the reference model across
    /// generated vectors that combine several defects (duplicates, out-of-range
    /// bps, wrong sums). Also verifies that a rejected create leaves no jar.
    #[test]
    fn validate_splits_matches_reference_model(
        splits in proptest::collection::vec(
            (0u8..5u8, proptest::sample::select(std::vec![0u32, 1, 5000, 9999, 10_000, 10_001])),
            0..12usize
        ),
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);

        let owner = Address::generate(env);
        let jar_id = String::from_str(env, "@model");
        let pool: std::vec::Vec<Address> = (0..4).map(|_| Address::generate(env)).collect();

        // Build the Soroban Vec<Split> from the generated data
        let mut sb_splits = vec![env];
        for (idx, bps) in splits.iter() {
            // Use a fixed pool of addresses so duplicates are common
            let addr = pool[*idx as usize % 4].clone();
            sb_splits.push_back(Split { to: addr, bps: *bps });
        }

        let expected = model_validate_splits(&splits);
        let result = client.try_create_jar(&owner, &jar_id, &sb_splits);

        match expected {
            Ok(()) => {
                prop_assert!(result.is_ok());
                prop_assert!(client.jar_exists(&jar_id));
            }
            Err(_) => {
                prop_assert!(result.is_err());
                prop_assert!(!client.jar_exists(&jar_id));
            }
        }
    }
}

// ── Issue #138: preview_split matches tip across random jars ────────────────

proptest! {
    #![proptest_config(snapshot_stable(64))]

    /// Asserts that preview_split, the tip event breakdown, and balance
    /// changes all agree recipient-by-recipient for random jars and amounts.
    /// Also asserts preview and tip succeed or fail together with the same error.
    #[test]
    fn preview_split_matches_tip_across_random_jars(
        bps in splits_strategy(),
        amount in 1i128..=10_000_000i128,
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let token = token::Client::new(env, &s.token);
        let token_admin = token::StellarAssetClient::new(env, &s.token);

        let owner = Address::generate(env);
        let tipper = Address::generate(env);
        token_admin.mint(&tipper, &10_000_000i128);

        let recipients: std::vec::Vec<Address> =
            bps.iter().map(|_| Address::generate(env)).collect();
        let mut splits = vec![env];
        for (addr, b) in recipients.iter().zip(bps.iter()) {
            splits.push_back(Split { to: addr.clone(), bps: *b });
        }

        let jar_id = String::from_str(env, "@preview");
        client.create_jar(&owner, &jar_id, &splits);

        // Record balances before
        let mut balances_before = std::vec::Vec::new();
        for r in recipients.iter() {
            balances_before.push(token.balance(r));
        }

        // Try preview and tip — they must agree on success/failure
        let preview_result = client.try_preview_split(&jar_id, &amount);
        let tip_result = client.try_tip(&tipper, &jar_id, &amount, &String::from_str(env, "x"));

        prop_assert_eq!(preview_result.is_ok(), tip_result.is_ok());

        if preview_result.is_ok() && tip_result.is_ok() {
            let preview_shares = preview_result.unwrap().unwrap();
            // Check that each recipient's balance increased by the previewed share
            for (i, r) in recipients.iter().enumerate() {
                let expected_share = preview_shares.get(i as u32).unwrap();
                let actual_balance = token.balance(r);
                prop_assert_eq!(actual_balance - balances_before[i], expected_share);
            }
        }
    }
}

fn op_strategy() -> impl Strategy<Value = Op> {
    let slugs = prop::sample::select(std::vec![
        std::string::String::from("@aaa"),
        std::string::String::from("@bbb"),
        std::string::String::from("@ccc"),
    ]);
    let addr_idx = 0..4usize;
    prop_oneof![
        (
            slugs.clone(),
            addr_idx.clone(),
            prop::collection::vec((addr_idx.clone(), 1..10000u32), 1..3)
        )
            .prop_map(|(slug, owner_idx, recipients)| Op::CreateJar {
                slug,
                owner_idx,
                recipients
            }),
        (slugs.clone(), addr_idx.clone(), 1..1000i128).prop_map(|(slug, tipper_idx, amount)| {
            Op::Tip {
                slug,
                tipper_idx,
                amount,
            }
        },),
    ]
}

proptest! {
    #![proptest_config(snapshot_stable(ProptestConfig::default().cases))]

    #[test]
    fn model_based_property_test(ops in proptest::collection::vec(op_strategy(), 20..50)) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let token = token::Client::new(env, &s.token);
        let token_admin = token::StellarAssetClient::new(env, &s.token);

        let addresses: std::vec::Vec<Address> = (0..4).map(|_| Address::generate(env)).collect();
        for address in &addresses {
            token_admin.mint(address, &1_000_000);
        }

        let mut model_jars: std::collections::HashMap<
            std::string::String,
            std::vec::Vec<(usize, u32)>,
        > = std::collections::HashMap::new();

        for op in &ops {
            match op {
                Op::CreateJar {
                    slug,
                    owner_idx,
                    recipients,
                } => {
                    let jar_id = String::from_str(env, slug.as_str());
                    let mut splits: Vec<Split> = vec![env];
                    for (i, bps) in recipients.iter() {
                        splits.push_back(Split {
                            to: addresses[*i].clone(),
                            bps: *bps,
                        });
                    }
                    let res = client.try_create_jar(&addresses[*owner_idx], &jar_id, &splits);
                    let mut seen = std::collections::HashSet::new();
                    let unique = recipients.iter().all(|(i, _)| seen.insert(*i));
                    let sum: u32 = recipients.iter().map(|(_, b)| *b).sum();
                    if sum != 10000 || !unique || model_jars.contains_key(slug) {
                        prop_assert!(res.is_err());
                    } else {
                        prop_assert!(res.is_ok());
                        model_jars.insert(slug.clone(), recipients.clone());
                    }
                }
                Op::Tip {
                    slug,
                    tipper_idx,
                    amount,
                } => {
                    let jar_id = String::from_str(env, slug.as_str());
                    if !model_jars.contains_key(slug) {
                        prop_assert!(client
                            .try_tip(
                                &addresses[*tipper_idx],
                                &jar_id,
                                amount,
                                &String::from_str(env, "tip")
                            )
                            .is_err());
                        continue;
                    }
                    let before = token.balance(&s.contract);
                    let res = client.try_tip(
                        &addresses[*tipper_idx],
                        &jar_id,
                        amount,
                        &String::from_str(env, "tip"),
                    );
                    if res.is_ok() {
                        // The router never keeps a tip: a successful call pays
                        // recipients directly.
                        prop_assert_eq!(token.balance(&s.contract), before);
                    }
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(snapshot_stable(48))]

    /// For any fee up to the cap and any valid split, the recipients' total
    /// plus the fee equals the tip exactly, and the fee is the truncated
    /// `amount * fee_bps / 10 000` (#79).
    #[test]
    fn fee_plus_payouts_equal_the_tip(
        bps in splits_strategy(),
        fee_bps in 0u32..=MAX_FEE_BPS,
        amount in 20_000i128..=1_000_000_000i128,
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let token = token::Client::new(env, &s.token);
        let token_admin = token::StellarAssetClient::new(env, &s.token);

        let owner = Address::generate(env);
        let tipper = Address::generate(env);
        token_admin.mint(&tipper, &amount);
        let recipients: std::vec::Vec<Address> =
            bps.iter().map(|_| Address::generate(env)).collect();
        let mut splits = vec![env];
        for (addr, b) in recipients.iter().zip(bps.iter()) {
            splits.push_back(Split { to: addr.clone(), bps: *b });
        }
        let jar_id = String::from_str(env, "@prop_fee");
        client.create_jar(&owner, &jar_id, &splits);
        client.set_fee_bps(&fee_bps);
        client.tip(&tipper, &jar_id, &amount, &String::from_str(env, "prop"));

        let paid: i128 = recipients.iter().map(|r| token.balance(r)).sum();
        let fee = token.balance(&s.admin);
        prop_assert_eq!(fee, amount * fee_bps as i128 / 10_000);
        prop_assert_eq!(paid + fee, amount);
        prop_assert_eq!(token.balance(&tipper), 0);
    }
}

// ---------------------------------------------------------------------------
// #266 — with escrow (#171) the contract keeps exactly the held balances
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum EscrowOp {
    Tip {
        tipper: usize,
        jar: usize,
        amount: i128,
    },
    Block {
        recipient: usize,
    },
    Unblock,
    Withdraw {
        recipient: usize,
    },
}

fn escrow_op_strategy() -> impl Strategy<Value = EscrowOp> {
    prop_oneof![
        4 => (0..2usize, 0..2usize, 1i128..=50_000).prop_map(|(tipper, jar, amount)| EscrowOp::Tip { tipper, jar, amount }),
        2 => (0..4usize).prop_map(|recipient| EscrowOp::Block { recipient }),
        1 => Just(EscrowOp::Unblock),
        2 => (0..4usize).prop_map(|recipient| EscrowOp::Withdraw { recipient }),
    ]
}

proptest! {
    #![proptest_config(snapshot_stable(32))]

    /// After every step of a random mix of tips, recipient deauthorisations
    /// (the token refuses transfers to one recipient), recoveries and
    /// withdrawals, the contract's token balance equals the sum of every
    /// recipient's `get_held`. Rejected calls must leave both sides as they
    /// were. One token only: escrow is not tracked per token yet.
    #[test]
    fn contract_holds_exactly_the_escrowed_total(ops in proptest::collection::vec(escrow_op_strategy(), 1..40)) {
        let s = setup_hostile();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let token = TestTokenClient::new(env, &s.token);

        let recipients: std::vec::Vec<Address> = (0..4).map(|_| Address::generate(env)).collect();
        let tippers: std::vec::Vec<Address> = (0..2).map(|_| Address::generate(env)).collect();
        for t in &tippers {
            token.mint(t, &10_000_000);
        }
        let jars = [String::from_str(env, "@trio"), String::from_str(env, "@duo")];
        client.create_jar(
            &Address::generate(env),
            &jars[0],
            &vec![
                env,
                Split { to: recipients[0].clone(), bps: 5_000 },
                Split { to: recipients[1].clone(), bps: 3_000 },
                Split { to: recipients[2].clone(), bps: 2_000 },
            ],
        );
        client.create_jar(
            &Address::generate(env),
            &jars[1],
            &vec![
                env,
                Split { to: recipients[2].clone(), bps: 6_000 },
                Split { to: recipients[3].clone(), bps: 4_000 },
            ],
        );
        let msg = String::from_str(env, "");

        for op in ops {
            match op {
                EscrowOp::Tip { tipper, jar, amount } => {
                    let _ = client.try_tip(&tippers[tipper], &jars[jar], &amount, &msg);
                }
                EscrowOp::Block { recipient } => token.block(&recipients[recipient]),
                EscrowOp::Unblock => token.unblock(),
                EscrowOp::Withdraw { recipient } => {
                    let _ = client.try_withdraw_held(&recipients[recipient]);
                }
            }
            let held: i128 = recipients.iter().map(|r| client.get_held(r)).sum();
            prop_assert_eq!(token.balance(&s.contract), held);
        }
    }
}
