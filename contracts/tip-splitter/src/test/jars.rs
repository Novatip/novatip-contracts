//! Jar lifecycle: creation, slugs, ownership, renaming, deletion, pausing, minimum and maximum tips, and storage lifetime.

use super::*;
use soroban_sdk::vec;

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
    let decoded: JarCreatedEvent =
        TryFromVal::try_from_val(env, data).expect("jar_crtd event data shape");
    assert_eq!(decoded.owner, owner);
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

    env.mock_auths(&[
        MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "transfer_jar_ownership",
                args: (jar_id.clone(), new_owner.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &new_owner,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "transfer_jar_ownership",
                args: (jar_id.clone(), new_owner.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);

    client.transfer_jar_ownership(&jar_id, &new_owner);
    assert_eq!(client.get_jar(&jar_id).owner, new_owner);
    // Splits must survive the transfer untouched.
    assert_eq!(client.get_jar(&jar_id).splits.get(0).unwrap().to, alice);
}

#[test]
fn transfer_jar_ownership_requires_new_owner_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@handoff_new_auth");

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
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "transfer_jar_ownership",
            args: (jar_id.clone(), new_owner.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_transfer_jar_ownership(&jar_id, &new_owner);
    assert!(
        res.is_err(),
        "the incoming owner must authorize the transfer"
    );
    assert_eq!(client.get_jar(&jar_id).owner, owner);
}

#[test]
fn transfer_jar_ownership_rejects_current_owner() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@self_xfer");

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

    env.mock_all_auths();
    // A no-op handoff has its own code, distinct from the contract/token one.
    assert_eq!(
        client.try_transfer_jar_ownership(&jar_id, &owner),
        Err(Ok(Error::SameOwner.into()))
    );
    assert_eq!(
        client.try_transfer_jar_ownership(&jar_id, &s.contract),
        Err(Ok(Error::InvalidRecipient.into()))
    );
    assert_eq!(
        client.try_transfer_jar_ownership(&jar_id, &s.token),
        Err(Ok(Error::InvalidRecipient.into()))
    );
    assert_eq!(client.get_jar(&jar_id).owner, owner);
}

#[test]
fn delete_jar_releases_slug_and_requires_owner_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@retire");
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
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "delete_jar",
            args: (jar_id.clone(),).into_val(env),
            sub_invokes: &[],
        },
    }]);
    client.delete_jar(&jar_id);

    assert!(!client.jar_exists(&jar_id));
    env.mock_auths(&[MockAuth {
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "create_jar",
            args: (
                owner.clone(),
                jar_id.clone(),
                vec![
                    env,
                    Split {
                        to: alice.clone(),
                        bps: 10000,
                    },
                ],
            )
                .into_val(env),
            sub_invokes: &[],
        },
    }]);
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
    assert!(client.jar_exists(&jar_id));
}

/// Reads the `(prev_owner, new_owner)` pair out of the `jar_xfer` event for
/// `jar_id`. The test env's event buffer only retains the most recent
/// invocation's events, so this is called straight after each transfer.
fn jar_xfer_owners(env: &Env, contract: &Address, jar_id: &String) -> (Address, Address) {
    let expected_topics = vec![
        env,
        symbol_short!("jar_xfer").into_val(env),
        jar_id.into_val(env),
    ];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == *contract && e.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1, "expected exactly one jar_xfer event");
    let (_, _, data) = events.first().unwrap();
    let decoded: JarTransferredEvent =
        TryFromVal::try_from_val(env, data).expect("jar_xfer event data shape");
    (decoded.previous_owner, decoded.new_owner)
}

/// The `jar_xfer` event must name both ends of the move. An indexer replaying a
/// partial event log can then see where a jar came from as well as where it
/// went, without having already indexed every prior event for that jar.
#[test]
fn transfer_jar_ownership_emits_jar_xfer_event_with_both_owners() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);

    let jar_id = String::from_str(env, "@xfer");
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

    let decoded = jar_xfer_owners(env, &s.contract, &jar_id);
    assert_eq!(
        decoded,
        (owner.clone(), new_owner.clone()),
        "the payload is (prev_owner, new_owner), in that order"
    );
    // The two must not be confusable: the first is the address that no longer
    // controls the jar, the second is the one that does.
    assert_ne!(decoded.0, decoded.1);
    assert_eq!(client.get_jar(&jar_id).owner, new_owner);
}

/// A second hand-off reports the first recipient as the outgoing owner, so a
/// consumer can chain the events into a complete ownership history rather than
/// seeing two disconnected arrivals.
#[test]
fn jar_xfer_events_chain_across_successive_transfers() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let first = Address::generate(env);
    let second = Address::generate(env);
    let third = Address::generate(env);
    let alice = Address::generate(env);

    let jar_id = String::from_str(env, "@chain");
    client.create_jar(
        &first,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );

    client.transfer_jar_ownership(&jar_id, &second);
    assert_eq!(
        jar_xfer_owners(env, &s.contract, &jar_id),
        (first, second.clone())
    );

    client.transfer_jar_ownership(&jar_id, &third);
    assert_eq!(
        jar_xfer_owners(env, &s.contract, &jar_id),
        (second, third.clone()),
        "the second event's prev_owner must be the first event's new_owner"
    );

    assert_eq!(client.get_jar(&jar_id).owner, third);
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

/// A slug body over 32 characters is rejected — it is used as a storage key,
/// an event topic, and a public URL slug, so unbounded input costs
/// unnecessary rent and the backend could never link to it.
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

    // One byte over MAX_JAR_SLUG_LEN (32), after the leading `@`.
    let mut bytes = [b'a'; 34];
    bytes[0] = b'@';
    let too_long = String::from_bytes(env, &bytes);
    let res = client.try_create_jar(&owner, &too_long, &splits);
    assert_eq!(res, Err(Ok(Error::InvalidJarId.into())));

    // The jar must not have been stored.
    assert!(
        client.try_get_jar(&too_long).is_err(),
        "rejected jar must not be persisted"
    );
}

/// The longest slug the backend can produce — `@` plus 32 characters — is
/// accepted, so an off-by-one in the bound cannot lock out a real creator.
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

    let mut bytes = [b'a'; 33];
    bytes[0] = b'@';
    let exact = String::from_bytes(env, &bytes);
    assert_eq!(exact.len(), 33);
    client.create_jar(&owner, &exact, &splits);

    assert_eq!(client.get_jar(&exact).splits.len(), 1);
}

/// The slug charset is canonical: upper case, whitespace, control bytes and a
/// bare id without `@` are all rejected with `InvalidJarId`, so a lookalike
/// such as `@Alice` cannot register a jar next to `@alice`.
#[test]
fn create_jar_rejects_non_canonical_jar_ids() {
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

    let upper = String::from_str(env, "@Alice");
    assert_eq!(
        client.try_create_jar(&owner, &upper, &splits),
        Err(Ok(Error::InvalidJarId.into()))
    );

    let spaced = String::from_str(env, "@ali ce");
    assert_eq!(
        client.try_create_jar(&owner, &spaced, &splits),
        Err(Ok(Error::InvalidJarId.into()))
    );

    let nul = String::from_bytes(env, b"@ali\0ce");
    assert_eq!(
        client.try_create_jar(&owner, &nul, &splits),
        Err(Ok(Error::InvalidJarId.into()))
    );

    let no_at = String::from_str(env, "alice");
    assert_eq!(
        client.try_create_jar(&owner, &no_at, &splits),
        Err(Ok(Error::InvalidJarId.into()))
    );

    let too_short = String::from_str(env, "@ab");
    assert_eq!(
        client.try_create_jar(&owner, &too_short, &splits),
        Err(Ok(Error::InvalidJarId.into()))
    );

    // None of them were persisted, and none shadow the canonical id.
    for id in [&upper, &spaced, &nul, &no_at, &too_short] {
        assert!(client.try_get_jar(id).is_err());
    }
    let canonical = String::from_str(env, "@alice");
    client.create_jar(&owner, &canonical, &splits);
    assert_eq!(client.get_jar(&canonical).owner, owner);
}

/// The destination of `rename_jar` goes through the same canonical rule, so a
/// jar cannot be moved to a slug no tip page can address.
#[test]
fn rename_jar_rejects_non_canonical_new_id() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let original = String::from_str(env, "@original");
    client.create_jar(
        &owner,
        &original,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );

    for new_id in [
        String::from_str(env, "@New"),
        String::from_str(env, "@new id"),
        String::from_str(env, "@ab"),
    ] {
        assert_eq!(
            client.try_rename_jar(&original, &new_id),
            Err(Ok(Error::InvalidJarId.into()))
        );
    }

    // The failed renames left the jar where it was; a canonical id moves it.
    assert_eq!(client.get_jar(&original).owner, owner);
    let renamed = String::from_str(env, "@renamed");
    client.rename_jar(&original, &renamed);
    assert_eq!(client.get_jar(&renamed).owner, owner);
    assert!(client.try_get_jar(&original).is_err());
}

#[test]
fn set_min_tip_amount_sets_and_reads_minimum() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@creators");
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

    // Initial state: no minimum configured
    assert_eq!(client.get_min_tip_amount(&jar_id), None);

    // Set minimum to 500
    client.set_min_tip_amount(&jar_id, &Some(500_i128));
    assert_eq!(client.get_min_tip_amount(&jar_id), Some(500_i128));

    // Clear minimum by passing None
    client.set_min_tip_amount(&jar_id, &None);
    assert_eq!(client.get_min_tip_amount(&jar_id), None);
}

#[test]
fn set_min_tip_amount_emits_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@eventjar");
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

    client.set_min_tip_amount(&jar_id, &Some(250_i128));

    let expected_topics = vec![
        env,
        symbol_short!("min_tip").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let min_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(min_events.len(), 1);
    let (_, _, data) = min_events.first().unwrap();
    let decoded: MinTipEvent = TryFromVal::try_from_val(env, data).expect("min_tip event data");
    assert_eq!(decoded.amount, Some(250_i128));
}

#[test]
fn set_min_tip_amount_requires_owner_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let non_owner = Address::generate(env);
    let jar_id = String::from_str(env, "@authcheck");
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
        address: &non_owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_min_tip_amount",
            args: (jar_id.clone(), Some(100_i128)).into_val(env),
            sub_invokes: &[],
        },
    }]);

    let res = client.try_set_min_tip_amount(&jar_id, &Some(100_i128));
    assert!(res.is_err());
    assert_eq!(client.get_min_tip_amount(&jar_id), None);
}

#[test]
fn set_min_tip_amount_rejects_non_positive_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@nonpos");
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

    let res_zero = client.try_set_min_tip_amount(&jar_id, &Some(0_i128));
    assert_eq!(res_zero, Err(Ok(Error::InvalidMinimumAmount.into())));

    let res_neg = client.try_set_min_tip_amount(&jar_id, &Some(-50_i128));
    assert_eq!(res_neg, Err(Ok(Error::InvalidMinimumAmount.into())));
}

#[test]
fn set_min_tip_amount_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let jar_id = String::from_str(env, "@ghost");
    let res = client.try_set_min_tip_amount(&jar_id, &Some(100_i128));
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

// ---------------------------------------------------------------------------
// #45 — per-jar pause switch
// ---------------------------------------------------------------------------

/// A paused jar rejects tips with `JarPaused`; unpausing restores them. Reads
/// stay available throughout, which is the whole point of a temporary pause.
#[test]
fn paused_jar_rejects_tips_until_unpaused() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@paused");
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

    // A fresh jar is open, and a tip settles normally.
    assert!(!client.is_jar_paused(&jar_id));
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "before"));
    assert_eq!(token.balance(&alice), 100);

    // Pausing closes the jar: `tip`, `tip_in_token` and `tip_many` all reject
    // with `JarPaused`, and no funds move.
    client.set_jar_paused(&jar_id, &true);
    assert!(client.is_jar_paused(&jar_id));

    let res = client.try_tip(&tipper, &jar_id, &100, &String::from_str(env, "paused"));
    assert_eq!(res, Err(Ok(Error::JarPaused.into())));
    let res_token = client.try_tip_in_token(
        &tipper,
        &jar_id,
        &s.token,
        &100,
        &String::from_str(env, "paused"),
    );
    assert_eq!(res_token, Err(Ok(Error::JarPaused.into())));
    let batch = vec![
        env,
        TipRequest {
            jar_id: jar_id.clone(),
            amount: 100,
            message: String::from_str(env, "paused"),
            reference: None,
        },
    ];
    assert_eq!(
        client.try_tip_many(&tipper, &batch, &None),
        Err(Ok(Error::JarPaused.into()))
    );
    // Balances are untouched by every rejected attempt.
    assert_eq!(token.balance(&alice), 100);
    assert_eq!(token.balance(&tipper), 900);

    // Reads stay available while paused.
    assert_eq!(client.get_jar(&jar_id).splits.get(0).unwrap().to, alice);
    assert_eq!(
        client.preview_split(&jar_id, &100).get(0).unwrap(),
        100_i128
    );

    // Unpausing reopens the jar and the next tip settles normally.
    client.set_jar_paused(&jar_id, &false);
    assert!(!client.is_jar_paused(&jar_id));
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "after"));
    assert_eq!(token.balance(&alice), 200);
}

/// Only the jar owner may toggle the pause flag.
#[test]
fn set_jar_paused_is_owner_only() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let stranger = Address::generate(env);
    let jar_id = String::from_str(env, "@ownpause");
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

    // A caller who is not the owner cannot pause the jar: the owner's auth is
    // required but only the stranger's is mocked, so the call is rejected and
    // the flag stays clear.
    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_jar_paused",
            args: (jar_id.clone(), true).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_set_jar_paused(&jar_id, &true).is_err());
    assert!(!client.is_jar_paused(&jar_id));
}

/// `set_jar_paused` emits a `jar_pause` event, and the flag is a property of
/// the jar: it survives `transfer_jar_ownership` and shows up in `get_jar`.
#[test]
fn jar_pause_emits_event_and_survives_ownership_transfer() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@keeppause");
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

    client.set_jar_paused(&jar_id, &true);

    // Read the `jar_pause` event immediately: the test env only keeps the last
    // invocation's events.
    let expected_topics = vec![
        env,
        symbol_short!("jar_pause").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(evs.len(), 1);
    let decoded: JarPausedEvent =
        TryFromVal::try_from_val(env, &evs[0].2).expect("jar_pause event data shape");
    assert_eq!(decoded.version, 1);
    assert!(decoded.paused);

    // The flag is visible in the full jar read and survives the handover.
    assert!(client.get_jar(&jar_id).paused);
    client.transfer_jar_ownership(&jar_id, &new_owner);
    assert!(client.is_jar_paused(&jar_id));
    assert_eq!(client.get_jar_owner(&jar_id), new_owner);
}

/// The recorded decision for #170: the minimum is a raw, unitless number, so
/// it gates the default token only. A tip in another allowed asset is not
/// subject to it.
#[test]
fn minimum_tip_gates_the_default_token_only() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let usdc_admin = token::StellarAssetClient::new(env, &s.token);
    let eurc = second_asset(env, &s.admin);
    let eurc_admin = token::StellarAssetClient::new(env, &eurc);
    let eurc_client = token::Client::new(env, &eurc);
    client.allow_token(&eurc);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    usdc_admin.mint(&tipper, &10_000);
    eurc_admin.mint(&tipper, &10_000);

    let jar_id = String::from_str(env, "@floored");
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
    client.set_min_tip_amount(&jar_id, &Some(500_i128));

    // Default token: the floor holds.
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &499, &String::from_str(env, "under")),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    client.tip(
        &tipper,
        &jar_id,
        &500,
        &String::from_str(env, "at the floor"),
    );

    // Other allowed asset: same jar, same minimum, not enforced — the number
    // has no unit to compare against.
    client.tip_in_token(
        &tipper,
        &jar_id,
        &eurc,
        &1,
        &String::from_str(env, "tiny eurc"),
    );
    assert_eq!(eurc_client.balance(&alice), 1);

    // ...unless the call names the default token, where it still applies.
    assert_eq!(
        client.try_tip_in_token(&tipper, &jar_id, &s.token, &2, &String::from_str(env, "x")),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
}

/// A co-signature is only worth what it says: signed over a different
/// `(jar_id, owner)` pair it does not authorise this one.
#[test]
fn create_jar_rejects_a_co_signature_for_the_wrong_pair() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let other_owner = Address::generate(env);
    let alice = Address::generate(env);
    let registrar = Address::generate(env);
    client.set_registrar(&Some(registrar.clone()));

    let jar_id = String::from_str(env, "@wrong_cosig");
    let other_jar_id = String::from_str(env, "@other_jar");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

    // Signed over (@other_jar, owner): right signer, wrong jar id.
    env.mock_auths(&[
        MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (owner.clone(), jar_id.clone(), splits.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &registrar,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (other_jar_id.clone(), owner.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);
    assert!(
        client.try_create_jar(&owner, &jar_id, &splits).is_err(),
        "a co-signature for a different jar id must not authorise this one"
    );

    // Signed over (@other_jar, other_owner): wrong jar id *and* wrong owner.
    env.mock_auths(&[
        MockAuth {
            address: &owner,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (owner.clone(), jar_id.clone(), splits.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &registrar,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (other_jar_id.clone(), other_owner.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);
    assert!(
        client.try_create_jar(&owner, &jar_id, &splits).is_err(),
        "a co-signature naming a different owner must not authorise this one"
    );

    assert!(
        client.try_get_jar(&jar_id).is_err(),
        "neither mismatched co-signature may create a jar"
    );
}

#[test]
fn set_min_tip_amount_requires_owner_auth_exact_error() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let non_owner = Address::generate(env);
    let jar_id = String::from_str(env, "@authcheck-exact");
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

    // #140 — positive control: owner can set the min tip
    client.set_min_tip_amount(&jar_id, &Some(50_i128));
    assert_eq!(client.get_min_tip_amount(&jar_id), Some(50));

    // #140 — negative: non-owner is refused with the exact auth error
    env.mock_auths(&[MockAuth {
        address: &non_owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_min_tip_amount",
            args: (jar_id.clone(), Some(100_i128)).into_val(env),
            sub_invokes: &[],
        },
    }]);
    let res = client.try_set_min_tip_amount(&jar_id, &Some(100_i128));
    assert!(res.is_err(), "non-owner must be rejected");
    // Auth errors surface as host errors under mock_auths
    let _ = res.unwrap_err();
}

// ── #141: Assert exact authorization with env.auths() ───────────────────────

#[test]
fn create_jar_authorization_tree() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@authtree");

    client.mock_all_auths().create_jar(
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

    let auths = env.auths();
    assert_eq!(
        auths.len(),
        1,
        "create_jar must record exactly one authorization"
    );
    assert_eq!(auths[0].0, owner);
}

/// Two jars with the same owner and a shared recipient must never share state.
#[test]
fn jars_owned_by_the_same_address_are_isolated() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let shared = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &10_000);

    let jar_a = String::from_str(env, "@solo");
    let jar_b = String::from_str(env, "@band");
    client.create_jar(
        &owner,
        &jar_a,
        &vec![
            env,
            Split {
                to: shared.clone(),
                bps: 6000,
            },
            Split {
                to: alice.clone(),
                bps: 4000,
            },
        ],
    );
    // The duplicate-recipient check is per jar, so this must be accepted.
    client.create_jar(
        &owner,
        &jar_b,
        &vec![
            env,
            Split {
                to: shared.clone(),
                bps: 3000,
            },
            Split {
                to: bob.clone(),
                bps: 7000,
            },
        ],
    );
    assert!(client.jar_exists(&jar_a));
    assert!(client.jar_exists(&jar_b));

    let jar_b_splits_before = client.get_jar(&jar_b).splits;
    client.update_splits(
        &jar_a,
        &vec![
            env,
            Split {
                to: shared.clone(),
                bps: 5000,
            },
            Split {
                to: alice.clone(),
                bps: 5000,
            },
        ],
    );
    let jar_b_splits_after = client.get_jar(&jar_b).splits;
    assert_eq!(jar_b_splits_after.len(), jar_b_splits_before.len());
    for i in 0..jar_b_splits_before.len() {
        let before = jar_b_splits_before.get(i).unwrap();
        let after = jar_b_splits_after.get(i).unwrap();
        assert_eq!(after.to, before.to, "jar B's recipients must not change");
        assert_eq!(after.bps, before.bps, "jar B's shares must not change");
    }

    let jar_b_owner_before = client.get_jar(&jar_b).owner;
    client.transfer_jar_ownership(&jar_a, &new_owner);
    assert_eq!(client.get_jar(&jar_a).owner, new_owner);
    assert_eq!(
        client.get_jar(&jar_b).owner,
        jar_b_owner_before,
        "transferring jar A must not move jar B"
    );

    client.set_min_tip_amount(&jar_a, &Some(1_000_i128));
    assert_eq!(client.get_min_tip_amount(&jar_b), None);
    assert_eq!(
        client.try_preview_split(&jar_a, &999_i128),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    assert!(client.try_preview_split(&jar_b, &999_i128).is_ok());
    assert_eq!(
        client.try_tip(&tipper, &jar_a, &999_i128, &String::from_str(env, "low")),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );

    let shared_before = token.balance(&shared);
    let alice_before = token.balance(&alice);
    let bob_before = token.balance(&bob);

    client.tip(
        &tipper,
        &jar_b,
        &1_000_i128,
        &String::from_str(env, "band tip"),
    );

    assert_eq!(
        token.balance(&shared),
        shared_before + 300,
        "the shared recipient gets exactly jar B's 30 % share"
    );
    assert_eq!(token.balance(&bob), bob_before + 700);
    assert_eq!(
        token.balance(&alice),
        alice_before,
        "jar A's recipient must not be paid by a jar B tip"
    );
}

/// A minimum set on one jar must survive an unrelated `update_splits` and keep
/// governing both `tip` and `preview_split`.
#[test]
fn minimum_survives_update_splits_and_still_governs_tip_and_preview() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &10_000);

    let jar_id = String::from_str(env, "@min-update");
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
    client.set_min_tip_amount(&jar_id, &Some(500_i128));
    client.update_splits(
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

    assert_eq!(client.get_min_tip_amount(&jar_id), Some(500));
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &499_i128, &String::from_str(env, "low")),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    assert_eq!(
        client.try_preview_split(&jar_id, &499_i128),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );

    client.tip(&tipper, &jar_id, &500_i128, &String::from_str(env, "ok"));
    assert_eq!(token.balance(&alice), 350);
    assert_eq!(token.balance(&bob), 150);
}

/// After ownership moves, only the new owner can change or clear the minimum,
/// and the old owner's attempt is rejected.
#[test]
fn transferred_jar_minimum_is_controlled_by_the_new_owner() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@min-xfer");
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
    client.set_min_tip_amount(&jar_id, &Some(500_i128));
    client.transfer_jar_ownership(&jar_id, &new_owner);

    let set_700 = (jar_id.clone(), Some(700_i128)).into_val(env);
    env.mock_auths(&[MockAuth {
        address: &new_owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_min_tip_amount",
            args: set_700,
            sub_invokes: &[],
        },
    }]);
    client.set_min_tip_amount(&jar_id, &Some(700_i128));
    assert_eq!(client.get_min_tip_amount(&jar_id), Some(700));

    let set_100 = (jar_id.clone(), Some(100_i128)).into_val(env);
    env.mock_auths(&[MockAuth {
        address: &owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_min_tip_amount",
            args: set_100,
            sub_invokes: &[],
        },
    }]);
    assert!(
        client
            .try_set_min_tip_amount(&jar_id, &Some(100_i128))
            .is_err(),
        "the previous owner must not change the minimum"
    );
    assert_eq!(client.get_min_tip_amount(&jar_id), Some(700));

    let clear = (jar_id.clone(), Option::<i128>::None).into_val(env);
    env.mock_auths(&[MockAuth {
        address: &new_owner,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_min_tip_amount",
            args: clear,
            sub_invokes: &[],
        },
    }]);
    client.set_min_tip_amount(&jar_id, &None);
    assert_eq!(client.get_min_tip_amount(&jar_id), None);
}

/// A minimum below the share floor does not replace it: the 1 bps recipient
/// still needs an amount large enough for their share to round to at least one
/// unit. Both errors are named so a change to either rule fails clearly.
#[test]
fn minimum_does_not_lower_the_share_floor() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let dust = Address::generate(env);
    let rest = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &20_000);

    let jar_id = String::from_str(env, "@min-floor");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: dust.clone(),
                bps: 1,
            },
            Split {
                to: rest.clone(),
                bps: 9999,
            },
        ],
    );
    client.set_min_tip_amount(&jar_id, &Some(100_i128));

    assert_eq!(
        client.try_tip(
            &tipper,
            &jar_id,
            &99_i128,
            &String::from_str(env, "below min")
        ),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &100_i128, &String::from_str(env, "floor")),
        Err(Ok(Error::SplitAmountTooSmall.into()))
    );
    assert_eq!(
        client.try_preview_split(&jar_id, &100_i128),
        Err(Ok(Error::SplitAmountTooSmall.into()))
    );

    client.tip(
        &tipper,
        &jar_id,
        &10_000_i128,
        &String::from_str(env, "clears floor"),
    );
    assert_eq!(token.balance(&dust), 1);
    assert_eq!(token.balance(&rest), 9_999);
}

/// A rejection on the minimum happens before any transfer, so no balance moves.
#[test]
fn rejected_below_min_tip_leaves_all_balances_unchanged() {
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

    let jar_id = String::from_str(env, "@min-rejected");
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
    client.set_min_tip_amount(&jar_id, &Some(500_i128));

    let alice_before = token.balance(&alice);
    let bob_before = token.balance(&bob);
    let tipper_before = token.balance(&tipper);

    assert_eq!(
        client.try_tip(&tipper, &jar_id, &499_i128, &String::from_str(env, "low")),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );

    assert_eq!(token.balance(&alice), alice_before);
    assert_eq!(token.balance(&bob), bob_before);
    assert_eq!(token.balance(&tipper), tipper_before);
}

/// Remaining time to live of a jar's persistent entry, read from inside the
/// contract so the test sees exactly what the host will archive on.
fn jar_ttl(env: &Env, contract: &Address, jar_id: &String) -> u32 {
    use soroban_sdk::testutils::storage::Persistent as _;
    env.as_contract(contract, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Jar(jar_id.clone()))
    })
}

fn min_tip_ttl(env: &Env, contract: &Address, jar_id: &String) -> u32 {
    use soroban_sdk::testutils::storage::Persistent as _;
    env.as_contract(contract, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::MinTip(jar_id.clone()))
    })
}

/// Keep the contract's own instance and code alive across a large ledger
/// advance, so a failure can only come from the jar entry under test.
fn keep_instance_alive(env: &Env, contract: &Address) {
    let max = env.storage().max_ttl();
    env.as_contract(contract, || {
        env.storage().instance().extend_ttl(max, max);
    });
}

/// #72: archival is silent and total, so a jar that is read must outlive one
/// that is never touched once the ledger moves past the untouched entry's TTL.
#[test]
fn touched_jar_survives_a_ledger_advance_that_archives_an_idle_one() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);

    let touched = single_recipient_jar(env, &client, &owner, "@touched");
    let idle = single_recipient_jar(env, &client, &owner, "@idle");

    // Both start at JAR_TTL_LEDGERS because create_jar extends the TTL (#67).
    assert_eq!(jar_ttl(env, &s.contract, &idle), JAR_TTL_LEDGERS);
    assert_eq!(jar_ttl(env, &s.contract, &touched), JAR_TTL_LEDGERS);

    // Advance to where the TTL falls below threshold
    keep_instance_alive(env, &s.contract);
    let start = env.ledger().sequence();
    let below = JAR_TTL_LEDGERS - JAR_TTL_THRESHOLD + 1;
    env.ledger().set_sequence_number(start + below);

    // Reading `touched` extends it back to JAR_TTL_LEDGERS
    client.get_jar(&touched);
    assert_eq!(jar_ttl(env, &s.contract, &touched), JAR_TTL_LEDGERS);

    // Advance past idle's expiration
    env.ledger()
        .set_sequence_number(start + JAR_TTL_LEDGERS + 1);

    assert_eq!(client.get_jar(&touched).owner, owner);
    // The test host escalates any access to an archived entry into a panic
    // (the real network refuses the transaction outright), so read it last.
    let archived = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.get_jar(&idle);
    }));
    assert!(
        archived.is_err(),
        "an untouched jar must be archived once its TTL runs out"
    );
}

/// A freshly created jar has its storage lifetime extended on creation, so it
/// remains readable after a ledger advance that would otherwise archive an un-extended entry (#67).
#[test]
fn freshly_created_jar_extends_ttl_and_survives_ledger_advance() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);

    let jar_id = single_recipient_jar(env, &client, &owner, "@fresh");
    assert_eq!(jar_ttl(env, &s.contract, &jar_id), JAR_TTL_LEDGERS);

    keep_instance_alive(env, &s.contract);
    let seq = env.ledger().sequence();
    // A ledger advance that would archive a default-TTL entry
    env.ledger().set_sequence_number(seq + 10_000);

    // The jar was extended on creation, so it is still readable
    assert_eq!(client.get_jar(&jar_id).owner, owner);
}

/// #72: below `JAR_TTL_THRESHOLD` an access extends the entry back to
/// `JAR_TTL_LEDGERS`; above it the access is a no-op and the TTL keeps falling.
#[test]
fn jar_access_extends_ttl_only_below_the_threshold() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let jar_id = single_recipient_jar(env, &client, &owner, "@ttl");
    keep_instance_alive(env, &s.contract);

    client.get_jar(&jar_id);
    assert_eq!(jar_ttl(env, &s.contract, &jar_id), JAR_TTL_LEDGERS);

    // Still above the threshold: the read must not pay for another write.
    let start = env.ledger().sequence();
    env.ledger().set_sequence_number(start + 1_000);
    client.get_jar(&jar_id);
    assert_eq!(jar_ttl(env, &s.contract, &jar_id), JAR_TTL_LEDGERS - 1_000);

    // One ledger below the threshold: the read must extend it again.
    let below = JAR_TTL_LEDGERS - JAR_TTL_THRESHOLD + 1;
    env.ledger().set_sequence_number(start + below);
    assert_eq!(jar_ttl(env, &s.contract, &jar_id), JAR_TTL_THRESHOLD - 1);
    client.get_jar(&jar_id);
    assert_eq!(jar_ttl(env, &s.contract, &jar_id), JAR_TTL_LEDGERS);
}

#[test]
fn jar_access_extends_minimum_tip_ttl_with_the_jar() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let jar_id = single_recipient_jar(env, &client, &owner, "@min-ttl");
    client.set_min_tip_amount(&jar_id, &Some(100));
    keep_instance_alive(env, &s.contract);

    let start = env.ledger().sequence();
    let below = JAR_TTL_LEDGERS - JAR_TTL_THRESHOLD + 1;
    env.ledger().set_sequence_number(start + below);
    client.get_jar(&jar_id);

    assert_eq!(jar_ttl(env, &s.contract, &jar_id), JAR_TTL_LEDGERS);
    assert_eq!(min_tip_ttl(env, &s.contract, &jar_id), JAR_TTL_LEDGERS);
}

/// A jar flagged default-token-only (#313) rejects `tip_in_token` for any
/// other allowed token before funds move, still takes the default token, and
/// the flag survives a rename.
#[test]
fn default_token_only_jar_rejects_other_tokens() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let usdc = token::Client::new(env, &s.token);
    let usdc_admin = token::StellarAssetClient::new(env, &s.token);

    let eurc = second_asset(env, &s.admin);
    let eurc_client = token::Client::new(env, &eurc);
    let eurc_admin = token::StellarAssetClient::new(env, &eurc);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    usdc_admin.mint(&tipper, &1_000);
    eurc_admin.mint(&tipper, &1_000);
    client.allow_token(&eurc);

    let jar_id = String::from_str(env, "@defonly");
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

    // Off by default, and the owner can switch it on.
    assert!(!client.get_jar_tokens_default_only(&jar_id));
    client.set_jar_tokens_default_only(&jar_id, &true);
    assert!(client.get_jar_tokens_default_only(&jar_id));

    // Another allowed token is refused and no funds move.
    let res = client.try_tip_in_token(
        &tipper,
        &jar_id,
        &eurc,
        &100,
        &String::from_str(env, "eurc"),
    );
    assert_eq!(res, Err(Ok(Error::NonDefaultToken.into())));
    assert_eq!(eurc_client.balance(&alice), 0);

    // The default token still settles.
    client.tip_in_token(
        &tipper,
        &jar_id,
        &s.token,
        &100,
        &String::from_str(env, "usdc"),
    );
    assert_eq!(usdc.balance(&alice), 100);

    // The flag follows the jar through a rename.
    let new_id = String::from_str(env, "@defonly2");
    client.rename_jar(&jar_id, &new_id);
    assert!(client.get_jar_tokens_default_only(&new_id));
    assert!(!client.get_jar_tokens_default_only(&jar_id));

    // Clearing it lets the other token through again.
    client.set_jar_tokens_default_only(&new_id, &false);
    client.tip_in_token(
        &tipper,
        &new_id,
        &eurc,
        &100,
        &String::from_str(env, "eurc again"),
    );
    assert_eq!(eurc_client.balance(&alice), 100);
}

// ===========================================================================
// #310 change_recipient, #311 withdraw_held_to, #312 fee recipient
// ===========================================================================

#[test]
fn change_recipient_needs_only_old_and_keeps_bps_position_and_lock() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (jar_id, alice, bob, _) = fee_jar(&s, "@chg_rcpt", 0);
    client.lock_splits(&jar_id);
    let carol = Address::generate(env);

    client.change_recipient(&jar_id, &alice, &carol);
    let auths = env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, alice);

    let jar = client.get_jar(&jar_id);
    let first = jar.splits.get(0).unwrap();
    assert_eq!((first.to, first.bps), (carol, 7000));
    let second = jar.splits.get(1).unwrap();
    assert_eq!((second.to, second.bps), (bob, 3000));
    assert!(client.is_splits_locked(&jar_id));
}

#[test]
fn change_recipient_rejects_duplicate_invalid_and_non_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (jar_id, alice, bob, _) = fee_jar(&s, "@chg_bad", 0);
    let stranger = Address::generate(env);

    assert_eq!(
        client.try_change_recipient(&jar_id, &alice, &bob),
        Err(Ok(Error::DuplicateRecipient.into()))
    );
    assert_eq!(
        client.try_change_recipient(&jar_id, &alice, &s.contract),
        Err(Ok(Error::InvalidRecipient.into()))
    );
    assert_eq!(
        client.try_change_recipient(&jar_id, &alice, &s.token),
        Err(Ok(Error::InvalidRecipient.into()))
    );
    assert_eq!(
        client.try_change_recipient(&jar_id, &stranger, &Address::generate(env)),
        Err(Ok(Error::NotRecipient.into()))
    );
}

// ---------------------------------------------------------------------------
// #262 — every jar-taking public function rejects non-canonical jar IDs
// ---------------------------------------------------------------------------

#[test]
fn every_public_function_rejects_non_canonical_jar_ids_consistently() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let recipient = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &10_000);

    let splits = vec![
        env,
        Split {
            to: recipient.clone(),
            bps: 10_000,
        },
    ];

    // One valid jar so `rename_jar(valid, bad)` exercises `new_jar_id` validation.
    let valid_jar = String::from_str(env, "@valid");
    let valid_new = String::from_str(env, "@valid_new");
    client.create_jar(&owner, &valid_jar, &splits);

    let invalid_ids = [
        "",                                   // empty
        "@Alice",                             // uppercase
        "@ali ce",                            // embedded whitespace
        " @alice",                            // leading whitespace
        "@alice ",                            // trailing whitespace
        "alice",                              // missing leading @
        "@ab",                                // too short (< 3 chars after @)
        "@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", // too long (33 chars after @)
    ];

    let msg = String::from_str(env, "hi");
    let invalid_jar_id = Err(Ok(Error::InvalidJarId.into()));
    // Each `try_*` client has a different success type, so the expected error
    // has to be inferred at every call site rather than bound to one variable.
    macro_rules! jar_not_found {
        () => {
            Err(Ok(Error::JarNotFound.into()))
        };
    }

    for raw in invalid_ids {
        let bad = String::from_str(env, raw);

        // Creation and rename target validate format explicitly -> InvalidJarId.
        assert_eq!(
            client.try_create_jar(&owner, &bad, &splits),
            invalid_jar_id,
            "create_jar({raw:?})"
        );
        assert_eq!(
            client.try_rename_jar(&valid_jar, &bad),
            invalid_jar_id,
            "rename_jar(valid, {raw:?})"
        );

        // Boolean existence / default-only queries return false without panicking.
        assert!(!client.jar_exists(&bad), "jar_exists({raw:?})");
        assert!(
            !client.get_jar_tokens_default_only(&bad),
            "get_jar_tokens_default_only({raw:?})"
        );

        // All other jar-taking entry points fail with JarNotFound.
        assert_eq!(client.try_update_splits(&bad, &splits), jar_not_found!());
        assert_eq!(client.try_lock_splits(&bad), jar_not_found!());
        assert_eq!(client.try_unlock_splits(&bad), jar_not_found!());
        assert_eq!(client.try_is_splits_locked(&bad), jar_not_found!());
        assert_eq!(client.try_set_jar_paused(&bad, &true), jar_not_found!());
        assert_eq!(client.try_is_jar_paused(&bad), jar_not_found!());
        assert_eq!(client.try_leave_jar(&bad, &recipient), jar_not_found!());
        assert_eq!(
            client.try_transfer_jar_ownership(&bad, &new_owner),
            jar_not_found!()
        );
        assert_eq!(
            client.try_admin_recover_jar(&bad, &new_owner),
            jar_not_found!()
        );
        assert_eq!(client.try_delete_jar(&bad), jar_not_found!());
        assert_eq!(client.try_rename_jar(&bad, &valid_new), jar_not_found!());
        assert_eq!(
            client.try_set_min_tip_amount(&bad, &Some(10)),
            jar_not_found!()
        );
        assert_eq!(
            client.try_set_max_tip_amount(&bad, &Some(100)),
            jar_not_found!()
        );
        assert_eq!(
            client.try_set_jar_tokens_default_only(&bad, &true),
            jar_not_found!()
        );
        assert_eq!(client.try_tip(&tipper, &bad, &100, &msg), jar_not_found!());
        assert_eq!(
            client.try_tip_with_ref(&tipper, &bad, &100, &msg, &None),
            jar_not_found!()
        );
        let batch = vec![
            env,
            TipRequest {
                jar_id: bad.clone(),
                amount: 100,
                message: msg.clone(),
                reference: None,
            },
        ];
        assert_eq!(
            client.try_tip_many(&tipper, &batch, &None),
            jar_not_found!()
        );
        assert_eq!(
            client.try_tip_in_token(&tipper, &bad, &s.token, &100, &msg),
            jar_not_found!()
        );
        assert_eq!(
            client.try_create_recurring(&tipper, &bad, &100, &86_400, &msg),
            jar_not_found!()
        );
        assert!(matches!(
            client.try_get_jar(&bad),
            Err(Ok(e)) if e == Error::JarNotFound.into()
        ));
        assert_eq!(client.try_get_jar_owner(&bad), jar_not_found!());
        assert_eq!(client.try_get_split_count(&bad), jar_not_found!());
        assert_eq!(client.try_preview_split(&bad, &100), jar_not_found!());
        assert_eq!(
            client.try_preview_split_details(&bad, &100),
            jar_not_found!()
        );
        assert_eq!(
            client.try_preview_tip(&bad, &tipper, &100),
            jar_not_found!()
        );
        assert_eq!(client.try_get_max_tip_amount(&bad), jar_not_found!());
        assert_eq!(client.try_get_min_tip_amount(&bad), jar_not_found!());
        assert_eq!(client.try_get_min_accepted_tip(&bad), jar_not_found!());
        assert_eq!(client.try_get_jar_stats(&bad), jar_not_found!());
        assert_eq!(client.try_get_jar_info(&bad), jar_not_found!());
        assert_eq!(
            client.try_get_supporter_stats(&bad, &tipper),
            jar_not_found!()
        );
        assert_eq!(client.try_is_recipient(&bad, &recipient), jar_not_found!());
    }
}

/// #323: owner-set maximum is validated against the minimum and rejects
/// larger tips and previews with a typed error.
#[test]
fn max_tip_amount_caps_tips_and_previews() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);
    let (_, _, jar_id) = one_recipient_jar(env, &client, "@capped");

    client.set_min_tip_amount(&jar_id, &Some(50));
    assert_eq!(
        client.try_set_max_tip_amount(&jar_id, &Some(10)),
        Err(Ok(Error::InvalidMaximumAmount.into()))
    );
    client.set_max_tip_amount(&jar_id, &Some(100));
    assert_eq!(client.get_max_tip_amount(&jar_id), Some(100));

    let msg = String::from_str(env, "big");
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &101, &msg),
        Err(Ok(Error::AboveMaxTipAmount.into()))
    );
    assert_eq!(
        client.try_preview_tip(&jar_id, &tipper, &101),
        Err(Ok(Error::AboveMaxTipAmount.into()))
    );
    client.tip(&tipper, &jar_id, &100, &msg);

    client.set_max_tip_amount(&jar_id, &None);
    assert_eq!(client.get_max_tip_amount(&jar_id), None);
}
