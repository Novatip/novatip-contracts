//! Split validation and changes: bps rules, recipients, leaving and locking.

use super::*;
use soroban_sdk::vec;

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

    let res = client.try_create_jar(&owner, &String::from_str(env, "@sum-bad"), &bad);
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

    // The splits event must carry the exact updated split vector, matching the
    // jar immediately after the mutation.
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
    let decoded: SplitsUpdatedEvent =
        TryFromVal::try_from_val(env, data).expect("splits event data shape");
    let jar = client.get_jar(&jar_id);
    assert_eq!(decoded.count, jar.splits.len() as u32);
    assert_eq!(decoded.splits, jar.splits);
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

/// Creates a single-recipient jar owned by `owner` and transfers it to
/// `new_owner` under `mock_all_auths`, so the authority tests below start
/// from a completed handoff and only the call under test runs with strict
/// `mock_auths`.
fn handed_off_jar(
    env: &Env,
    client: &TipSplitterClient,
    jar_id: &String,
    owner: &Address,
    new_owner: &Address,
    recipient: &Address,
) {
    client.create_jar(
        owner,
        jar_id,
        &vec![
            env,
            Split {
                to: recipient.clone(),
                bps: 10000,
            },
        ],
    );
    client.transfer_jar_ownership(jar_id, new_owner);
}

/// Mocks exactly one signature: `signer` authorizing
/// `update_splits(jar_id, splits)` on the contract, and nothing else.
fn mock_update_splits_auth(
    env: &Env,
    contract: &Address,
    signer: &Address,
    jar_id: &String,
    splits: &Vec<Split>,
) {
    env.mock_auths(&[MockAuth {
        address: signer,
        invoke: &MockAuthInvoke {
            contract,
            fn_name: "update_splits",
            args: (jar_id.clone(), splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
}

/// Authority moves with the stored owner (#75): after a transfer, the new
/// owner's signature alone is enough to call `update_splits`. A transfer that
/// rewrote the field without moving authority would fail here.
#[test]
fn new_owner_can_update_splits_after_transfer() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@handoff_new");
    handed_off_jar(env, &client, &jar_id, &owner, &new_owner, &alice);

    let new_splits = vec![
        env,
        Split {
            to: bob.clone(),
            bps: 10000,
        },
    ];
    mock_update_splits_auth(env, &s.contract, &new_owner, &jar_id, &new_splits);
    client.update_splits(&jar_id, &new_splits);

    let splits = client.get_jar(&jar_id).splits;
    assert_eq!(splits.len(), 1);
    assert_eq!(splits.get(0).unwrap().to, bob);
}

/// The other half of #75: once the jar has moved, the previous owner's
/// signature no longer authorizes `update_splits`, and the splits are left as
/// they were. `new_owner_can_update_splits_after_transfer` is the positive
/// control: the same builder with the right signer succeeds, so this failure
/// is about who signed, not the shape of the mocked invocation.
#[test]
fn previous_owner_cannot_update_splits_after_transfer() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let mallory = Address::generate(env);
    let jar_id = String::from_str(env, "@handoff_old");
    handed_off_jar(env, &client, &jar_id, &owner, &new_owner, &alice);

    let hijack = vec![
        env,
        Split {
            to: mallory.clone(),
            bps: 10000,
        },
    ];
    mock_update_splits_auth(env, &s.contract, &owner, &jar_id, &hijack);
    let res = client.try_update_splits(&jar_id, &hijack);
    assert!(
        res.is_err(),
        "the previous owner must not be able to update splits after transferring ownership"
    );

    let splits = client.get_jar(&jar_id).splits;
    assert_eq!(splits.len(), 1);
    assert_eq!(
        splits.get(0).unwrap().to,
        alice,
        "a rejected update must leave the splits untouched"
    );
}

#[test]
fn leave_jar_reassigns_departing_share_to_last_entry() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);

    let jar_id = String::from_str(env, "@trio");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
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
                to: carol.clone(),
                bps: 3000,
            },
        ],
    );

    client.leave_jar(&jar_id, &bob);

    // A `splits` event must publish the post-update split vector, which matches
    // the jar contents read back immediately afterward.
    let expected_topics = vec![
        env,
        symbol_short!("splits").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(evs.len(), 1);
    let decoded: SplitsUpdatedEvent =
        TryFromVal::try_from_val(env, &evs[0].2).expect("splits event data shape");
    let jar = client.get_jar(&jar_id);
    assert_eq!(decoded.count, jar.splits.len() as u32);
    assert_eq!(decoded.splits, jar.splits);
    assert_eq!(jar.splits.len(), 2);
    // middle entry removed; the last remaining entry (carol) absorbs bob's 3000.
    assert_eq!(jar.splits.get(0).unwrap().to, alice);
    assert_eq!(jar.splits.get(0).unwrap().bps, 4000);
    assert_eq!(jar.splits.get(1).unwrap().to, carol);
    assert_eq!(jar.splits.get(1).unwrap().bps, 6000);
    assert!(!client.is_recipient(&jar_id, &bob));
}

#[test]
fn leave_jar_when_the_last_entry_leaves() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@pair");
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

    client.leave_jar(&jar_id, &bob); // last entry leaves
    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.splits.len(), 1);
    assert_eq!(jar.splits.get(0).unwrap().to, alice);
    assert_eq!(jar.splits.get(0).unwrap().bps, 10000);
}

#[test]
fn leave_jar_rejects_sole_and_non_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let only = Address::generate(env);
    let stranger = Address::generate(env);
    let jar_id = String::from_str(env, "@only");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: only.clone(),
                bps: 10000,
            },
        ],
    );

    // sole recipient cannot leave (jar would have no splits)
    assert!(client.try_leave_jar(&jar_id, &only).is_err());
    // an address not in the jar cannot leave
    assert!(client.try_leave_jar(&jar_id, &stranger).is_err());
}

#[test]
fn lock_splits_sets_flag_and_publishes_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@lock");
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
    assert!(!client.is_splits_locked(&jar_id));

    client.lock_splits(&jar_id);

    // read the `lock` event immediately: the test env only keeps the last
    // invocation's events, and `is_splits_locked` is a separate call.
    let expected_topics = vec![
        env,
        symbol_short!("lock").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(evs.len(), 1);
    let decoded: JarLockEvent =
        TryFromVal::try_from_val(env, &evs[0].2).expect("lock event data shape");
    assert_eq!(decoded.version, 1);
    assert!(decoded.locked);

    assert!(client.is_splits_locked(&jar_id));
}

#[test]
fn locked_jar_needs_consent_only_from_decreased_recipients() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@shared");
    client.create_jar(
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
                bps: 4000,
            },
        ],
    );
    client.lock_splits(&jar_id);

    // bob 4000 -> 6000 (increase, no consent needed); alice 6000 -> 4000 (decrease, needs alice)
    let new_splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 4000,
        },
        Split {
            to: bob.clone(),
            bps: 6000,
        },
    ];

    // owner signs but the decreased recipient (alice) does not -> rejected
    env.mock_auths(&[MockAuth {
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "update_splits",
            args: (jar_id.clone(), new_splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_update_splits(&jar_id, &new_splits).is_err());
    assert_eq!(client.get_jar(&jar_id).splits.get(0).unwrap().bps, 6000);

    // owner + alice (the one who lost share) -> accepted; bob never had to sign
    env.mock_auths(&[
        MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "update_splits",
                args: (jar_id.clone(), new_splits.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &alice,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "update_splits",
                args: (jar_id.clone(), new_splits.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);
    client.update_splits(&jar_id, &new_splits);
    assert_eq!(client.get_jar(&jar_id).splits.get(1).unwrap().bps, 6000);
}

#[test]
fn unlock_requires_all_current_recipients() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@shared");
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
    client.lock_splits(&jar_id);

    // owner alone cannot disarm the lock
    env.mock_auths(&[MockAuth {
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "unlock_splits",
            args: (jar_id.clone(),).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_unlock_splits(&jar_id).is_err());
    assert!(client.is_splits_locked(&jar_id));

    // owner + every current recipient can unlock
    let args: Vec<soroban_sdk::Val> = (jar_id.clone(),).into_val(env);
    let inv_owner = MockAuthInvoke {
        contract: &s.contract,
        fn_name: "unlock_splits",
        args: args.clone(),
        sub_invokes: &[],
    };
    let inv_alice = MockAuthInvoke {
        contract: &s.contract,
        fn_name: "unlock_splits",
        args: args.clone(),
        sub_invokes: &[],
    };
    let inv_bob = MockAuthInvoke {
        contract: &s.contract,
        fn_name: "unlock_splits",
        args,
        sub_invokes: &[],
    };
    env.mock_auths(&[
        MockAuth {
            address: &owner,
            invoke: &inv_owner,
        },
        MockAuth {
            address: &alice,
            invoke: &inv_alice,
        },
        MockAuth {
            address: &bob,
            invoke: &inv_bob,
        },
    ]);
    client.unlock_splits(&jar_id);
    assert!(!client.is_splits_locked(&jar_id));
}

#[test]
fn lock_survives_ownership_transfer() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@keeplock");
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
    client.lock_splits(&jar_id);
    client.transfer_jar_ownership(&jar_id, &new_owner);
    assert!(client.is_splits_locked(&jar_id));
    assert_eq!(client.get_jar_owner(&jar_id), new_owner);
}

/// #77: `update_splits` rebuilds the `Jar` by hand, so the owner must be
/// re-read afterwards and must still hold the right to update.
#[test]
fn update_splits_leaves_the_owner_unchanged() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = single_recipient_jar(env, &client, &owner, "@keepowner");

    let first = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 6000,
        },
        Split {
            to: bob.clone(),
            bps: 4000,
        },
    ];
    client.update_splits(&jar_id, &first);
    assert_eq!(client.get_jar(&jar_id).owner, owner);
    assert_eq!(client.get_jar_owner(&jar_id), owner);

    // Only the original owner's auth is mocked, so this proves the stored
    // owner is still the address `update_splits` checks.
    let second = vec![
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
            args: (jar_id.clone(), second.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
    client.update_splits(&jar_id, &second);

    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.owner, owner);
    assert!(jar.splits == second);
}

// ---------------------------------------------------------------------------
// #239 — recovering a locked jar from an unreachable recipient
// ---------------------------------------------------------------------------

use soroban_sdk::testutils::Ledger as _;

/// A locked jar: alice 5000, bob 3000, carol 2000. Returns the pieces.
fn locked_three(s: &Setup, id: &str) -> (String, Address, Address, Address, Address) {
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let (alice, bob, carol) = (
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    );
    let jar_id = String::from_str(env, id);
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
                bps: 3000,
            },
            Split {
                to: carol.clone(),
                bps: 2000,
            },
        ],
    );
    client.lock_splits(&jar_id);
    (jar_id, owner, alice, bob, carol)
}

fn advance(env: &Env, ledgers: u32) {
    env.ledger().with_mut(|l| l.sequence_number += ledgers);
}

#[test]
fn locked_jar_recovers_from_an_unreachable_recipient_after_the_delay() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (jar_id, _, alice, bob, carol) = locked_three(&s, "@recover");

    client.request_recipient_removal(&jar_id, &bob);
    let ready = env.ledger().sequence() + 120_960;
    let evs = env.events().all();
    let last = evs.last().unwrap();
    let data: RemovalRequestedEvent = TryFromVal::try_from_val(env, &last.2).unwrap();
    assert_eq!(data.recipient, bob);
    assert_eq!(data.ready_ledger, ready);

    // Not yet: the delay has not elapsed.
    advance(env, 120_959);
    assert_eq!(
        client.try_execute_recipient_removal(&jar_id),
        Err(Ok(Error::RemovalNotReady.into()))
    );
    advance(env, 1);
    client.execute_recipient_removal(&jar_id);

    // Bob is gone, his share went to the last entry, and the lock is intact.
    let jar = client.get_jar(&jar_id);
    assert_eq!(
        jar.splits,
        vec![
            env,
            Split {
                to: alice,
                bps: 5000
            },
            Split {
                to: carol,
                bps: 5000
            },
        ]
    );
    assert!(client.is_splits_locked(&jar_id));
    // The request is consumed.
    assert_eq!(
        client.try_execute_recipient_removal(&jar_id),
        Err(Ok(Error::RemovalNotRequested.into()))
    );
}

#[test]
fn recipient_can_veto_a_removal_and_owner_cannot_force_it() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (jar_id, owner, _, bob, _) = locked_three(&s, "@veto");

    client.request_recipient_removal(&jar_id, &bob);
    advance(env, 200_000);
    client.cancel_recipient_removal(&jar_id, &bob);
    assert_eq!(
        client.try_execute_recipient_removal(&jar_id),
        Err(Ok(Error::RemovalNotRequested.into()))
    );
    assert_eq!(client.get_jar(&jar_id).splits.len(), 3);

    // The owner can withdraw their own request too; a stranger cannot.
    client.request_recipient_removal(&jar_id, &bob);
    let stranger = Address::generate(env);
    assert_eq!(
        client.try_cancel_recipient_removal(&jar_id, &stranger),
        Err(Ok(Error::NotAuthorized.into()))
    );
    client.cancel_recipient_removal(&jar_id, &owner);
}

#[test]
fn removal_bypass_attempts_are_blocked() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (jar_id, _, alice, bob, _) = locked_three(&s, "@bypass");
    let stranger = Address::generate(env);

    // Not a recipient / executing with no request.
    assert_eq!(
        client.try_request_recipient_removal(&jar_id, &stranger),
        Err(Ok(Error::NotRecipient.into()))
    );
    assert_eq!(
        client.try_execute_recipient_removal(&jar_id),
        Err(Ok(Error::RemovalNotRequested.into()))
    );

    // Requests need the owner's signature.
    env.mock_auths(&[]);
    assert!(client.try_request_recipient_removal(&jar_id, &bob).is_err());
    env.mock_all_auths();

    // A request is void once the recipients change underneath it.
    client.request_recipient_removal(&jar_id, &bob);
    client.leave_jar(&jar_id, &alice);
    advance(env, 120_960);
    assert_eq!(
        client.try_execute_recipient_removal(&jar_id),
        Err(Ok(Error::RemovalNotRequested.into()))
    );

    // Execution needs the owner's signature.
    client.request_recipient_removal(&jar_id, &bob);
    advance(env, 120_960);
    env.mock_auths(&[]);
    assert!(client.try_execute_recipient_removal(&jar_id).is_err());
    env.mock_all_auths();
}

#[test]
fn sole_recipient_cannot_be_removed() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@solo-rm");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10_000,
            },
        ],
    );
    client.lock_splits(&jar_id);
    assert_eq!(
        client.try_request_recipient_removal(&jar_id, &alice),
        Err(Ok(Error::SoleRecipient.into()))
    );
}
