#![cfg(test)]
use super::*;
use proptest::prelude::*;
use soroban_sdk::testutils::{Address as _, Events as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{symbol_short, token, vec, Address, Env, IntoVal, String, TryFromVal};

/// Shared test fixture: a fresh env with a USDC-like token and a deployed
/// TipSplitter pointed at it. All auths are mocked.
struct Setup {
    env: Env,
    contract: Address,
    token: Address,
    admin: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let token = sac.address();

    let contract = env.register(TipSplitter, (admin.clone(), token.clone()));
    Setup {
        env,
        contract,
        token,
        admin,
    }
}

#[test]
fn tip_splits_70_30() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@band");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 7000,
        },
        Split {
            to: bob.clone(),
            bps: 3000,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "great show"));

    assert_eq!(token.balance(&alice), 70);
    assert_eq!(token.balance(&bob), 30);
    assert_eq!(token.balance(&tipper), 900);
}

#[test]
fn tip_sends_rounding_dust_to_last_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let a = Address::generate(env);
    let b = Address::generate(env);
    let c = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &10);

    let jar_id = String::from_str(env, "@trio");
    let splits = vec![
        env,
        Split {
            to: a.clone(),
            bps: 3333,
        },
        Split {
            to: b.clone(),
            bps: 3333,
        },
        Split {
            to: c.clone(),
            bps: 3334,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);

    client.tip(&tipper, &jar_id, &10, &String::from_str(env, "hi"));

    // 10 * 3333 / 10000 = 3 (truncated) for a and b; c absorbs the remainder.
    assert_eq!(token.balance(&a), 3);
    assert_eq!(token.balance(&b), 3);
    assert_eq!(token.balance(&c), 4);
    assert_eq!(token.balance(&tipper), 0);
}

#[test]
fn tip_emits_tip_event_with_expected_topics_and_data() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);

    let jar_id = String::from_str(env, "@ev");
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

    let message = String::from_str(env, "nice set");
    client.tip(&tipper, &jar_id, &100, &message);

    // decodeTipEvent in novatip-sdk reads topic 0 as the "tip" symbol, topic 1
    // as the jar id, and the data as the (from, amount, message) tuple.
    let tip_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract)
        .collect();
    assert_eq!(tip_events.len(), 1);
    let (_, topics, data) = tip_events.first().unwrap();
    assert_eq!(
        topics,
        &vec![
            env,
            symbol_short!("tip").into_val(env),
            jar_id.into_val(env)
        ]
    );
    let decoded: (Address, i128, String) =
        TryFromVal::try_from_val(env, data).expect("tip event data shape");
    assert_eq!(decoded, (tipper, 100i128, message));
}

#[test]
fn create_jar_rejects_bad_bps_sum() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    // Distinct recipients, so the only defect is the sum: 6000 + 3000 = 9000.
    let bad = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 6000,
        },
        Split {
            to: bob.clone(),
            bps: 3000,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, "@x"), &bad);
    assert_eq!(res, Err(Ok(Error::SplitSumNot100Pct.into())));
}

/// No single share may exceed 100%. Such an entry can never belong to a set
/// summing to `BPS_DENOM`, and rejecting it per entry is what bounds the
/// running total well below `u32::MAX`.
#[test]
fn create_jar_rejects_bps_above_one_hundred_percent() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bad = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10_001,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, "@overshare"), &bad);
    assert_eq!(res, Err(Ok(Error::SplitOutOfRange.into())));
}

/// Overflow regression: `u32::MAX + 10_001` is `2^32 + 10_000`, so a wrapping
/// `+=` would land on exactly `BPS_DENOM` and wave this jar through as if the
/// shares summed to 100%. It must be rejected with the typed `InvalidSplits`,
/// and it must be rejected by the validator itself — not by `overflow-checks`
/// trapping in the release profile, which would surface as an opaque wasm
/// error instead of error code 4.
#[test]
fn create_jar_rejects_bps_sum_that_wraps_u32() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let bad = vec![
        env,
        Split {
            to: alice.clone(),
            bps: u32::MAX,
        },
        Split {
            to: bob.clone(),
            bps: 10_001,
        },
    ];

    let jar_id = String::from_str(env, "@wrap");
    let res = client.try_create_jar(&owner, &jar_id, &bad);
    assert_eq!(res, Err(Ok(Error::SplitOutOfRange.into())));

    // The jar must not have been stored.
    let jar = client.try_get_jar(&jar_id);
    assert!(jar.is_err(), "rejected jar must not be persisted");
}

/// `update_splits` runs the same validator, so overflowing shares can't be
/// swapped into a jar that already exists.
#[test]
fn update_splits_rejects_bps_sum_that_wraps_u32() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);

    let jar_id = String::from_str(env, "@upd-wrap");
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

    let res = client.try_update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: u32::MAX,
            },
            Split {
                to: bob.clone(),
                bps: 10_001,
            },
        ],
    );
    assert_eq!(res, Err(Ok(Error::SplitOutOfRange.into())));

    // The original split must be untouched.
    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.splits.len(), 1);
    assert_eq!(jar.splits.get(0).unwrap().bps, 10000);
}

/// The same address twice is rejected even though the shares still total 100%.
#[test]
fn create_jar_rejects_duplicate_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let dup = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 6000,
        },
        Split {
            to: alice.clone(),
            bps: 4000,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, "@dupaddr"), &dup);
    assert_eq!(res, Err(Ok(Error::DuplicateRecipient.into())));

    // The jar must not have been stored.
    let jar = client.try_get_jar(&String::from_str(env, "@dupaddr"));
    assert!(jar.is_err(), "rejected jar must not be persisted");
}

/// The duplicate need not be adjacent — the check is pairwise across the whole
/// vector, not just neighbours.
#[test]
fn create_jar_rejects_non_adjacent_duplicate_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let dup = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 4000,
        },
        Split {
            to: bob.clone(),
            bps: 3000,
        },
        Split {
            to: alice.clone(),
            bps: 3000,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, "@spread"), &dup);
    assert_eq!(res, Err(Ok(Error::DuplicateRecipient.into())));
}

/// A vector of distinct addresses must still be accepted and pay out normally —
/// the new check must not reject valid jars.
#[test]
fn create_jar_accepts_distinct_recipients() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);

    let jar_id = String::from_str(env, "@distinct");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 5000,
        },
        Split {
            to: bob.clone(),
            bps: 3000,
        },
        Split {
            to: carol.clone(),
            bps: 2000,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);

    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.splits.len(), 3);

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "nice"));
    assert_eq!(token.balance(&alice), 50);
    assert_eq!(token.balance(&bob), 30);
    assert_eq!(token.balance(&carol), 20);
}

/// The O(n^2) scan must still accept a full-size jar at the MAX_RECIPIENTS bound.
#[test]
fn create_jar_accepts_max_distinct_recipients() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);

    // 20 distinct recipients at 500 bps each = 10000.
    let mut splits = soroban_sdk::Vec::new(env);
    for _ in 0..20 {
        splits.push_back(Split {
            to: Address::generate(env),
            bps: 500,
        });
    }

    let jar_id = String::from_str(env, "@full");
    client.create_jar(&owner, &jar_id, &splits);
    assert_eq!(client.get_jar(&jar_id).splits.len(), 20);
}

/// `update_splits` runs the same validator, so a duplicate can't be introduced
/// into an existing jar.
#[test]
fn update_splits_rejects_duplicate_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);

    let jar_id = String::from_str(env, "@upd-dup");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 5000,
            },
            Split {
                to: bob.clone(),
                bps: 5000,
            },
        ],
    );

    let res = client.try_update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 5000,
            },
            Split {
                to: alice.clone(),
                bps: 5000,
            },
        ],
    );
    assert_eq!(res, Err(Ok(Error::DuplicateRecipient.into())));

    // The original two-way split must be untouched.
    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.splits.len(), 2);
    assert_eq!(jar.splits.get(1).unwrap().to, bob);
}

/// A recipient with `bps: 0` can never be paid, so it must be rejected at
/// validation time rather than silently skipped inside `tip`.
#[test]
fn create_jar_rejects_zero_bps_entry() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let ghost = Address::generate(env);
    // Sums to exactly 10000, but `ghost` would never receive anything.
    let bad = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
        Split {
            to: ghost.clone(),
            bps: 0,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, "@zero"), &bad);
    assert_eq!(res, Err(Ok(Error::SplitOutOfRange.into())));

    // The jar must not have been stored.
    let jar = client.try_get_jar(&String::from_str(env, "@zero"));
    assert!(jar.is_err(), "rejected jar must not be persisted");
}

/// The zero check must not depend on position — a leading zero-bps entry is
/// just as invalid as a trailing one.
#[test]
fn create_jar_rejects_zero_bps_first_entry() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let ghost = Address::generate(env);
    let alice = Address::generate(env);
    let bad = vec![
        env,
        Split {
            to: ghost.clone(),
            bps: 0,
        },
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, "@zerofirst"), &bad);
    assert_eq!(res, Err(Ok(Error::SplitOutOfRange.into())));
}

/// A single recipient holding the whole jar must still carry a real share.
#[test]
fn create_jar_rejects_all_zero_bps() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bad = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 0,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, "@allzero"), &bad);
    assert_eq!(res, Err(Ok(Error::SplitOutOfRange.into())));
}

/// `update_splits` runs the same validation, so a zero-bps entry can't be
/// smuggled into an already-valid jar.
#[test]
fn update_splits_rejects_zero_bps_entry() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let ghost = Address::generate(env);

    let jar_id = String::from_str(env, "@upd-zero");
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

    let res = client.try_update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
            Split {
                to: ghost.clone(),
                bps: 0,
            },
        ],
    );
    assert_eq!(res, Err(Ok(Error::SplitOutOfRange.into())));

    // The original splits must be untouched.
    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.splits.len(), 1);
    assert_eq!(jar.splits.get(0).unwrap().bps, 10000);
}

#[test]
fn create_jar_rejects_duplicate_slug() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@dup");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    client.create_jar(&owner, &jar_id, &splits);
    let res = client.try_create_jar(&owner, &jar_id, &splits);
    assert_eq!(res, Err(Ok(Error::JarExists.into())));
}

/// `jar_exists` flips from false to true on registration, and matches the slug
/// exactly — a prefix or a different slug must not read as taken.
#[test]
fn jar_exists_tracks_registration() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@alice");

    assert!(
        !client.jar_exists(&jar_id),
        "slug must be free before registration"
    );

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

    assert!(
        client.jar_exists(&jar_id),
        "slug must be taken after registration"
    );
    assert!(
        !client.jar_exists(&String::from_str(env, "@bob")),
        "an unrelated slug must still be free"
    );
    // The onboarding check relies on exact matching: "@ali" is its own slug.
    assert!(
        !client.jar_exists(&String::from_str(env, "@ali")),
        "a prefix of a taken slug must still be free"
    );
}

/// A `create_jar` that fails validation must leave the slug free — otherwise
/// the availability check would report a name as taken that nobody owns.
#[test]
fn jar_exists_is_false_for_rejected_jar() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@rejected");

    // 6000 + 3000 = 9000, so the jar is never stored.
    let res = client.try_create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 6000,
            },
            Split {
                to: bob.clone(),
                bps: 3000,
            },
        ],
    );
    assert!(res.is_err());

    assert!(!client.jar_exists(&jar_id));
}

/// `update_splits` rewrites the same key, so the slug must stay registered.
#[test]
fn jar_exists_stays_true_after_update_splits() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@steady");

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
    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 5000,
            },
            Split {
                to: bob.clone(),
                bps: 5000,
            },
        ],
    );

    assert!(client.jar_exists(&jar_id));
}

#[test]
fn tip_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let tipper = Address::generate(env);
    let res = client.try_tip(
        &tipper,
        &String::from_str(env, "@ghost"),
        &100,
        &String::from_str(env, "?"),
    );
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// `update_splits` runs the same lookup-then-panic path as `tip`, so an
/// unregistered slug must be rejected rather than quietly creating a jar.
#[test]
fn update_splits_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let alice = Address::generate(env);
    // Valid splits, so a missing jar is the only thing that can fail this.
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    let jar_id = String::from_str(env, "@never-registered");
    let res = client.try_update_splits(&jar_id, &splits);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));

    // The failed update must not have brought the jar into existence.
    assert!(
        client.try_get_jar(&jar_id).is_err(),
        "update_splits must not create a jar as a side effect"
    );
}

#[test]
fn tip_rejects_nonpositive_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = String::from_str(env, "@a");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);

    let res = client.try_tip(&tipper, &jar_id, &0, &String::from_str(env, ""));
    assert_eq!(res, Err(Ok(Error::InvalidAmount.into())));
}

/// An over-long message is rejected before any funds move — the guard sits
/// above the transfer loop, so balances must be untouched.
#[test]
fn tip_rejects_over_long_message() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &500);

    let jar_id = String::from_str(env, "@wordy");
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

    // One byte over MAX_MESSAGE_LEN (280).
    let too_long = String::from_bytes(env, &[b'a'; 281]);
    let res = client.try_tip(&tipper, &jar_id, &100, &too_long);
    assert_eq!(res, Err(Ok(Error::MessageTooLong.into())));

    // No funds may have moved.
    assert_eq!(token.balance(&alice), 0, "alice must not have been paid");
    assert_eq!(
        token.balance(&tipper),
        500,
        "tipper balance must be unchanged"
    );
}

/// A message of exactly MAX_MESSAGE_LEN bytes is still valid — the bound is
/// inclusive, so an off-by-one here would reject legitimate tips.
#[test]
fn tip_accepts_message_at_exact_limit() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &500);

    let jar_id = String::from_str(env, "@atlimit");
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

    let exact = String::from_bytes(env, &[b'a'; 280]);
    assert_eq!(exact.len(), 280);
    client.tip(&tipper, &jar_id, &100, &exact);

    assert_eq!(token.balance(&alice), 100);
    assert_eq!(token.balance(&tipper), 400);
}

#[test]
fn update_splits_changes_distribution() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &200);

    let jar_id = String::from_str(env, "@band");
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

    // Add bob; now split 50/50.
    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 5000,
            },
            Split {
                to: bob.clone(),
                bps: 5000,
            },
        ],
    );

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "gig"));

    assert_eq!(token.balance(&alice), 50);
    assert_eq!(token.balance(&bob), 50);

    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.splits.len(), 2);
}

#[test]
fn tip_single_recipient_receives_full_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &500);

    let jar_id = String::from_str(env, "@solo");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);
    client.tip(
        &tipper,
        &jar_id,
        &500,
        &String::from_str(env, "all for you"),
    );

    // Single recipient must receive the exact amount with no dust loss
    assert_eq!(token.balance(&alice), 500);
    assert_eq!(token.balance(&tipper), 0);
}

#[test]
fn create_jar_rejects_too_many_recipients() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let addr = Address::generate(env);

    // 21 recipients exceeds MAX_RECIPIENTS (20)
    // bps values don't matter — TooManyRecipients is checked first
    let mut splits_vec = soroban_sdk::Vec::new(env);
    for _ in 0..21 {
        splits_vec.push_back(Split {
            to: addr.clone(),
            bps: 476,
        });
    }

    let res = client.try_create_jar(&owner, &String::from_str(env, "@toobig"), &splits_vec);
    assert_eq!(res, Err(Ok(Error::TooManyRecipients.into())));
}

#[test]
fn create_jar_emits_jar_created_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];
    let jar_id = String::from_str(env, "@one");

    client.create_jar(&owner, &jar_id, &splits);

    // The jar_crtd event must be published with the correct topics and data.
    let jar_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract)
        .collect();
    assert_eq!(jar_events.len(), 1);
    let (_, topics, data) = jar_events.first().unwrap();
    assert_eq!(
        topics,
        &vec![
            env,
            symbol_short!("jar_crtd").into_val(env),
            jar_id.into_val(env)
        ]
    );
    let decoded: Address = TryFromVal::try_from_val(env, data).expect("jar_crtd event data shape");
    assert_eq!(decoded, owner);
}

#[test]
fn update_splits_emits_splits_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@two");

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

    let new_splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 5000,
        },
        Split {
            to: bob.clone(),
            bps: 5000,
        },
    ];
    client.update_splits(&jar_id, &new_splits);

    // The splits event must be published with the jar id and new split count.
    let expected_topics = vec![
        env,
        symbol_short!("splits").into_val(env),
        jar_id.into_val(env),
    ];
    let splits_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(splits_events.len(), 1);
    let (_, _, data) = splits_events.first().unwrap();
    let decoded: u32 = TryFromVal::try_from_val(env, data).expect("splits event data shape");
    assert_eq!(decoded, new_splits.len());
}

#[test]
fn tip_fails_when_sender_has_insufficient_balance() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let token = token::Client::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);

    // Mint less than the tip amount: tipper has 50, tip is 100.
    token_admin.mint(&tipper, &50);

    let jar_id = String::from_str(env, "@underfunded");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);

    let res = client.try_tip(&tipper, &jar_id, &100, &String::from_str(env, "oops"));
    assert!(res.is_err());

    // Balances must be unchanged — tipper keeps their 50, alice gets nothing.
    assert_eq!(token.balance(&tipper), 50);
    assert_eq!(token.balance(&alice), 0);
}

/// Atomicity test: if the sender can't cover the full tip on a multi-recipient
/// jar, the transaction must revert entirely — no recipient receives anything
/// and the tipper's balance is unchanged. This is the core "all-or-nothing"
/// guarantee stated in the module docs and CONTRACT.md.
#[test]
fn tip_multi_recipient_no_partial_distribution_on_insufficient_balance() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let token = token::Client::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    let tipper = Address::generate(env);

    // Tip amount is 300; tipper only has 100 — not enough to cover all splits.
    let tip_amount: i128 = 300;
    token_admin.mint(&tipper, &100);

    let jar_id = String::from_str(env, "@trio-atomic");
    // Three-way even split: 40% / 35% / 25%
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 4000,
        },
        Split {
            to: bob.clone(),
            bps: 3500,
        },
        Split {
            to: carol.clone(),
            bps: 2500,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);

    let res = client.try_tip(
        &tipper,
        &jar_id,
        &tip_amount,
        &String::from_str(env, "not enough"),
    );

    // The call must fail.
    assert!(res.is_err());

    // Atomicity: every recipient balance must still be 0 — no partial payment.
    assert_eq!(
        token.balance(&alice),
        0,
        "alice must not have received anything"
    );
    assert_eq!(
        token.balance(&bob),
        0,
        "bob must not have received anything"
    );
    assert_eq!(
        token.balance(&carol),
        0,
        "carol must not have received anything"
    );

    // The tipper's balance must be completely unchanged.
    assert_eq!(
        token.balance(&tipper),
        100,
        "tipper balance must be unchanged"
    );
}

// ---------------------------------------------------------------------------
// Authorization
//
// Every test above runs under `setup()`, which calls `env.mock_all_auths()` —
// that makes every `require_auth()` succeed unconditionally, so none of them
// can tell a wired-up auth check from a missing one. The tests below switch the
// env to `mock_auths(&[..])`, which authorizes *only* the listed invocations
// and rejects everything else, so a deleted `require_auth()` line shows up as a
// call that unexpectedly succeeds.
//
// Each negative test is paired with a positive control using the same builder
// and the correct signer. Without the control, a negative test would still pass
// if the call failed for some unrelated reason (wrong arg encoding, say), which
// would make it worthless as a guard.
// ---------------------------------------------------------------------------

/// The declared `owner` must sign `create_jar` — a third party cannot register
/// a jar in someone else's name.
#[test]
fn create_jar_requires_declared_owner_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let attacker = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@unauthorized");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    // The attacker signs, but the call declares `owner` as the jar owner.
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "create_jar",
            args: (owner.clone(), jar_id.clone(), splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_create_jar(&owner, &jar_id, &splits);
    assert!(
        res.is_err(),
        "create_jar must reject a caller who is not the declared owner"
    );

    // Nothing may have been written.
    assert!(
        client.try_get_jar(&jar_id).is_err(),
        "no jar may be created without the owner's authorization"
    );
}

/// Positive control for the test above: the same call with the owner signing
/// must succeed, proving the rejection is about *who* signed and not about the
/// shape of the mocked invocation.
#[test]
fn create_jar_succeeds_with_declared_owner_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@authorized");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    env.mock_auths(&[MockAuth {
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "create_jar",
            args: (owner.clone(), jar_id.clone(), splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);

    client.create_jar(&owner, &jar_id, &splits);
    assert_eq!(client.get_jar(&jar_id).owner, owner);
}

/// Only the jar owner may rewrite the splits. This is the check that matters
/// most: without it anyone on the network could redirect a creator's tips to
/// their own address.
#[test]
fn update_splits_requires_jar_owner_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let attacker = Address::generate(env);

    let jar_id = String::from_str(env, "@victim");
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

    // The attacker tries to point the whole jar at themselves.
    let hijacked = vec![
        env,
        Split {
            to: attacker.clone(),
            bps: 10000,
        },
    ];
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "update_splits",
            args: (jar_id.clone(), hijacked.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_update_splits(&jar_id, &hijacked);
    assert!(
        res.is_err(),
        "update_splits must reject a caller who does not own the jar"
    );

    // The stored splits must still pay alice, not the attacker.
    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.splits.len(), 1);
    assert_eq!(
        jar.splits.get(0).unwrap().to,
        alice,
        "an unauthorized update must not change the recipient"
    );
}

/// Positive control: the owner's own signature is accepted by the same builder.
#[test]
fn update_splits_succeeds_with_jar_owner_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);

    let jar_id = String::from_str(env, "@ownerupd");
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

    let new_splits = vec![
        env,
        Split {
            to: bob.clone(),
            bps: 10000,
        },
    ];
    env.mock_auths(&[MockAuth {
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "update_splits",
            args: (jar_id.clone(), new_splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);

    client.update_splits(&jar_id, &new_splits);
    assert_eq!(client.get_jar(&jar_id).splits.get(0).unwrap().to, bob);
}

#[test]
fn transfer_jar_ownership_requires_current_owner_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let attacker = Address::generate(env);

    let jar_id = String::from_str(env, "@stolen");
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

    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "transfer_jar_ownership",
            args: (jar_id.clone(), attacker.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_transfer_jar_ownership(&jar_id, &attacker);
    assert!(
        res.is_err(),
        "transfer_jar_ownership must reject a caller who does not own the jar"
    );
    assert_eq!(client.get_jar(&jar_id).owner, owner);
}

#[test]
fn transfer_jar_ownership_moves_control_to_new_owner() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);

    let jar_id = String::from_str(env, "@handoff");
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

    client.transfer_jar_ownership(&jar_id, &new_owner);
    assert_eq!(client.get_jar(&jar_id).owner, new_owner);
    // Splits must survive the transfer untouched.
    assert_eq!(client.get_jar(&jar_id).splits.get(0).unwrap().to, alice);

    // The new owner can now update splits.
    let new_splits = vec![
        env,
        Split {
            to: bob.clone(),
            bps: 10000,
        },
    ];
    env.mock_auths(&[MockAuth {
        address: &new_owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "update_splits",
            args: (jar_id.clone(), new_splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
    client.update_splits(&jar_id, &new_splits);
    assert_eq!(client.get_jar(&jar_id).splits.get(0).unwrap().to, bob);

    // The old owner can no longer update splits.
    env.mock_auths(&[MockAuth {
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "update_splits",
            args: (jar_id.clone(), new_splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
    let res = client.try_update_splits(&jar_id, &new_splits);
    assert!(
        res.is_err(),
        "the old owner must not be able to update splits after transferring ownership"
    );
}

/// `tip` moves the sender's tokens, so it must carry the sender's signature.
///
/// The mock here authorizes *only* the token `transfer` the contract makes on
/// the sender's behalf — deliberately not the `tip` call itself. Withholding
/// every auth would not prove anything: the SAC's own `transfer` requires the
/// sender too, so the call would fail even with `from.require_auth()` deleted.
/// Pre-authorizing the transfer strips that second line of defence away, so the
/// only thing left standing between this call and a spent balance is `tip`'s
/// own check.
#[test]
fn tip_requires_sender_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@nosig");
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

    env.mock_auths(&[MockAuth {
        address: &tipper,
        invoke: &MockAuthInvoke {
            contract: &s.token,
            fn_name: "transfer",
            args: (tipper.clone(), alice.clone(), 100i128).into_val(env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_tip(&tipper, &jar_id, &100, &String::from_str(env, "sneaky"));
    assert!(
        res.is_err(),
        "tip must reject a call the sender has not authorized"
    );

    // No tokens may have moved.
    assert_eq!(token.balance(&tipper), 1_000, "sender must not be debited");
    assert_eq!(token.balance(&alice), 0, "recipient must not be credited");
}

/// A signature from someone other than `from` is not enough either — the auth
/// must belong to the address whose balance is being spent.
#[test]
fn tip_rejects_auth_from_wrong_address() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    let bystander = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@wrongsig");
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

    // As above, the sender's transfer is pre-authorized so that only `tip`'s own
    // check can reject this. The `tip` call carries a bystander's signature.
    let message = String::from_str(env, "not mine to send");
    env.mock_auths(&[
        MockAuth {
            address: &bystander,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "tip",
                args: (tipper.clone(), jar_id.clone(), 100i128, message.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &tipper,
            invoke: &MockAuthInvoke {
                contract: &s.token,
                fn_name: "transfer",
                args: (tipper.clone(), alice.clone(), 100i128).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);

    let res = client.try_tip(&tipper, &jar_id, &100, &message);
    assert!(
        res.is_err(),
        "tip must reject a signature from an address other than the sender"
    );
    assert_eq!(token.balance(&tipper), 1_000, "sender must not be debited");
    assert_eq!(token.balance(&alice), 0, "recipient must not be credited");
}

/// Positive control: the sender's signature, covering both the `tip` call and
/// the token `transfer` it makes on their behalf, is accepted.
#[test]
fn tip_succeeds_with_sender_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@goodsig");
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

    let message = String::from_str(env, "thanks");
    env.mock_auths(&[MockAuth {
        address: &tipper,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "tip",
            args: (tipper.clone(), jar_id.clone(), 100i128, message.clone()).into_val(env),
            sub_invokes: &[MockAuthInvoke {
                contract: &s.token,
                fn_name: "transfer",
                args: (tipper.clone(), alice.clone(), 100i128).into_val(env),
                sub_invokes: &[],
            }],
        },
    }]);

    client.tip(&tipper, &jar_id, &100, &message);
    assert_eq!(token.balance(&alice), 100);
    assert_eq!(token.balance(&tipper), 900);
}

/// An empty `jar_id` is rejected before any storage write — it would still be
/// usable as a storage key, but no frontend could address it as a URL slug.
#[test]
fn create_jar_rejects_empty_jar_id() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, ""), &splits);
    assert_eq!(res, Err(Ok(Error::InvalidJarId.into())));
}

/// A `jar_id` longer than `MAX_JAR_ID_LEN` is rejected — it is used as a
/// storage key, an event topic, and a public URL slug, so unbounded input
/// costs unnecessary rent.
#[test]
fn create_jar_rejects_over_long_jar_id() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    // One byte over MAX_JAR_ID_LEN (64).
    let too_long = String::from_bytes(env, &[b'a'; 65]);
    let res = client.try_create_jar(&owner, &too_long, &splits);
    assert_eq!(res, Err(Ok(Error::InvalidJarId.into())));

    // The jar must not have been stored.
    assert!(
        client.try_get_jar(&too_long).is_err(),
        "rejected jar must not be persisted"
    );
}

/// A `jar_id` of exactly `MAX_JAR_ID_LEN` bytes is still valid — the bound is
/// inclusive, so an off-by-one here would reject legitimate jar ids.
#[test]
fn create_jar_accepts_jar_id_at_exact_limit() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    let exact = String::from_bytes(env, &[b'a'; 64]);
    assert_eq!(exact.len(), 64);
    client.create_jar(&owner, &exact, &splits);

    assert_eq!(client.get_jar(&exact).splits.len(), 1);
}

#[test]
fn get_admin_returns_constructor_admin() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert_eq!(client.get_admin(), s.admin);
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// The core invariant: however a tip is split, every recipient's payout
    /// sums back to exactly the tipped amount, with no dust lost or created,
    /// and the tipper's balance drops by exactly that amount. Covers amounts
    /// smaller than the recipient count, where truncation bites hardest.
    #[test]
    fn tip_distributes_full_amount_across_random_splits(
        bps in splits_strategy(),
        amount in 1i128..=1_000_000_000i128,
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
            splits.push_back(Split {
                to: addr.clone(),
                bps: *b,
            });
        }

        let jar_id = String::from_str(env, "@prop");
        client.create_jar(&owner, &jar_id, &splits);
        client.tip(&tipper, &jar_id, &amount, &String::from_str(env, "prop"));

        let total: i128 = recipients.iter().map(|r| token.balance(r)).sum();
        prop_assert_eq!(total, amount);
        prop_assert_eq!(token.balance(&tipper), 0);
    }
}
