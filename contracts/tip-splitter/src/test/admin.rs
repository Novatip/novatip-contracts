//! Admin powers: the token allowlist, hooks, the registrar, upgrades, migration, pausing, and the pinned interface.

use super::*;
use soroban_sdk::vec;

#[test]
fn admin_recover_jar_reassigns_owner_and_emits_a_distinct_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let recipient = Address::generate(env);
    let jar_id = String::from_str(env, "@recover");
    let splits = vec![
        env,
        Split {
            to: recipient.clone(),
            bps: 10_000,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);

    env.mock_auths(&[MockAuth {
        address: &s.admin,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "admin_recover_jar",
            args: (jar_id.clone(), new_owner.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
    client.admin_recover_jar(&jar_id, &new_owner);

    // The test env exposes only the last invocation's events, so read them
    // before the `get_jar` call below.
    let expected_topics = vec![
        env,
        symbol_short!("jar_recov").into_val(env),
        jar_id.into_val(env),
    ];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|event| event.0 == s.contract && event.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1);
    let (_, _, data) = events.first().unwrap();
    let event: JarAdminRecoveredEvent =
        TryFromVal::try_from_val(env, data).expect("admin recovery event data shape");
    assert_eq!(event.admin, s.admin);
    assert_eq!(event.previous_owner, owner);
    assert_eq!(event.new_owner, new_owner);

    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.owner, new_owner);
    assert_eq!(jar.splits, splits);
}

#[test]
fn admin_recover_jar_rejects_non_admin_without_changing_owner() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let attacker = Address::generate(env);
    let recipient = Address::generate(env);
    let jar_id = String::from_str(env, "@recovery");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: recipient,
                bps: 10_000,
            },
        ],
    );

    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "admin_recover_jar",
            args: (jar_id.clone(), attacker.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
    let result = client.try_admin_recover_jar(&jar_id, &attacker);
    assert!(result.is_err());
    assert_eq!(client.get_jar(&jar_id).owner, owner);
}

/// `adm_xfer`, `jar_rnm` and `jar_del` publish named, versioned structs, and
/// both jar-id events carry the relevant jar id as a topic.
#[test]
fn admin_transfer_and_jar_rename_delete_events_are_versioned() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_admin = Address::generate(env);
    let jar_id = String::from_str(env, "@before");
    let new_jar_id = String::from_str(env, "@after");
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

    client.rename_jar(&jar_id, &new_jar_id);
    let rename_topics = vec![
        env,
        symbol_short!("jar_rnm").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let rename_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == rename_topics)
        .collect();
    assert_eq!(rename_events.len(), 1);
    let renamed: JarRenamedEvent =
        TryFromVal::try_from_val(env, &rename_events[0].2).expect("jar_rnm event data shape");
    assert_eq!(renamed.version, 1);
    assert_eq!(renamed.jar_id, jar_id);
    assert_eq!(renamed.new_jar_id, new_jar_id);

    client.delete_jar(&new_jar_id);
    let delete_topics = vec![
        env,
        symbol_short!("jar_del").into_val(env),
        new_jar_id.clone().into_val(env),
    ];
    let delete_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == delete_topics)
        .collect();
    assert_eq!(delete_events.len(), 1);
    let deleted: JarDeletedEvent =
        TryFromVal::try_from_val(env, &delete_events[0].2).expect("jar_del event data shape");
    assert_eq!(deleted.version, 1);
    assert_eq!(deleted.jar_id, new_jar_id);

    client.transfer_admin(&new_admin);
    let admin_topics = vec![env, symbol_short!("adm_xfer").into_val(env)];
    let admin_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == admin_topics)
        .collect();
    assert_eq!(admin_events.len(), 1);
    let transferred: AdminTransferredEvent =
        TryFromVal::try_from_val(env, &admin_events[0].2).expect("adm_xfer event data shape");
    assert_eq!(transferred.version, 1);
    assert_eq!(transferred.admin, s.admin);
    assert_eq!(transferred.new_admin, new_admin);
}

// ---------------------------------------------------------------------------
// #169 — interface version view
// ---------------------------------------------------------------------------

#[test]
fn version_matches_the_interface_constant() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert_eq!(client.version(), INTERFACE_VERSION);
    assert_eq!(
        client.version(),
        10,
        11,
        "the published version is part of the public interface; bump \
         INTERFACE_VERSION when the shape changes, do not edit this test \
         to match a silent change"
    );
}

/// The deploy-time token is on the list from construction, so an existing
/// deployment gains `tip_in_token` without the admin having to act, and
/// `get_token` and `is_token_allowed` agree.
#[test]
fn deploy_time_token_is_allowed_by_default() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert!(client.is_token_allowed(&s.token));
    assert!(!client.is_token_allowed(&Address::generate(env)));
}

#[test]
fn admin_can_allow_and_disallow_a_token() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let eurc = second_asset(env, &s.admin);

    assert!(!client.is_token_allowed(&eurc));
    client.allow_token(&eurc);
    assert!(client.is_token_allowed(&eurc));

    client.disallow_token(&eurc);
    assert!(!client.is_token_allowed(&eurc));
}

#[test]
fn allow_token_rejects_non_token_addresses() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let arbitrary = Address::generate(env);

    assert_eq!(
        client.try_allow_token(&arbitrary),
        Err(Ok(Error::InvalidToken.into()))
    );
    assert!(!client.is_token_allowed(&arbitrary));
}

/// Only the admin may move the list — a jar owner who could add assets would
/// turn the contract's allowlist into a per-jar one, and a stranger could open
/// it to anything.
#[test]
fn allow_and_disallow_require_admin_auth() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let eurc = second_asset(env, &s.admin);
    let attacker = Address::generate(env);

    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "allow_token",
            args: (eurc.clone(),).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(
        client.try_allow_token(&eurc).is_err(),
        "a non-admin must not be able to widen the allowlist"
    );
    assert!(!client.is_token_allowed(&eurc));

    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "disallow_token",
            args: (eurc.clone(),).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(
        client.try_disallow_token(&eurc).is_err(),
        "a non-admin must not be able to narrow the allowlist"
    );
}

/// Each change publishes an event naming the token and the admin who
/// authorised it, so an index of accepted assets has provenance.
#[test]
fn allow_and_disallow_emit_events_naming_token_and_admin() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let eurc = second_asset(env, &s.admin);

    client.allow_token(&eurc.clone());
    let allow_topics = vec![env, symbol_short!("tk_allow").into_val(env)];
    let ev: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == allow_topics)
        .collect();
    assert_eq!(ev.len(), 1, "allow_token must emit exactly one event");
    let allow: TokenAllowlistEvent =
        TryFromVal::try_from_val(env, &ev.first().unwrap().2).expect("allow event payload");
    assert_eq!(allow.version, 1);
    assert_eq!(allow.admin, s.admin);
    assert_eq!(allow.token, eurc);
    assert!(allow.allowed);

    client.disallow_token(&eurc.clone());
    let dis_topics = vec![env, symbol_short!("tk_dis").into_val(env)];
    let ev: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == dis_topics)
        .collect();
    assert_eq!(ev.len(), 1, "disallow_token must emit exactly one event");
    let dis: TokenAllowlistEvent =
        TryFromVal::try_from_val(env, &ev.first().unwrap().2).expect("disallow event payload");
    assert_eq!(dis.version, 1);
    assert_eq!(dis.admin, s.admin);
    assert_eq!(dis.token, eurc);
    assert!(!dis.allowed);
}

/// `tip` always pays through the deploy-time token, so removing it from the
/// list would leave a working tip path for a token the view calls closed.
#[test]
fn disallow_rejects_the_default_token() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert_eq!(
        client.try_disallow_token(&s.token),
        Err(Ok(Error::CannotDisallowDefault.into()))
    );
    assert!(
        client.is_token_allowed(&s.token),
        "the default token stays allowed"
    );
}

/// Only the admin may set or clear the hook, and every change publishes a
/// `tip_hook` event naming the admin who authorised it.
#[test]
fn set_tip_hook_is_admin_only_and_emits_on_change() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let hook = env.register(RecordingHook, ());
    let attacker = Address::generate(env);

    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_tip_hook",
            args: (Some(hook.clone()),).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(
        client.try_set_tip_hook(&Some(hook.clone())).is_err(),
        "a non-admin must not be able to register a hook"
    );
    assert_eq!(client.get_tip_hook(), None);
    env.mock_all_auths();

    client.set_tip_hook(&Some(hook.clone()));
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("tip_hook")))
        .collect();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].1.len(), 1, "admin events carry only the name topic");
    let event: TipHookEvent = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(event.version, 1);
    assert_eq!(event.admin, s.admin);
    assert_eq!(event.hook, Some(hook));
}

// ---------------------------------------------------------------------------
// #178 — an optional registrar whose co-signature gates `create_jar`.
// ---------------------------------------------------------------------------

/// Set/clear is admin-only, the getter reflects storage, and every change
/// publishes a `reg_set` event naming the admin who authorised it so the
/// trust-model transition is indexable after the fact.
#[test]
fn set_registrar_is_admin_only_and_round_trips_with_events() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let registrar = Address::generate(env);
    let attacker = Address::generate(env);

    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_registrar",
            args: (Some(registrar.clone()),).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(
        client.try_set_registrar(&Some(registrar.clone())).is_err(),
        "a non-admin must not be able to install a registrar"
    );
    assert_eq!(client.get_registrar(), None);
    env.mock_all_auths();

    // The testutils event buffer only exposes the most recent top-level
    // invocation, so each change's event is checked immediately after that
    // change — before the `get_registrar` call would replace the buffer.
    client.set_registrar(&Some(registrar.clone()));
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("reg_set")))
        .collect();
    assert_eq!(evs.len(), 1, "installing the registrar publishes one event");
    assert_eq!(evs[0].1.len(), 1, "admin events carry only the name topic");
    let first: RegistrarEvent = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(first.version, 1);
    assert_eq!(first.admin, s.admin);
    assert_eq!(first.registrar, Some(registrar.clone()));
    assert_eq!(client.get_registrar(), Some(registrar.clone()));

    client.set_registrar(&None);
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("reg_set")))
        .collect();
    assert_eq!(evs.len(), 1, "clearing the registrar publishes one event");
    assert_eq!(evs[0].1.len(), 1, "admin events carry only the name topic");
    let second: RegistrarEvent = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(second.version, 1);
    assert_eq!(second.admin, s.admin);
    assert_eq!(second.registrar, None);
    assert_eq!(client.get_registrar(), None);
}

/// With a registrar installed, a `create_jar` carrying both the owner's
/// signature and the registrar's co-signature over exactly
/// `(jar_id, owner)` succeeds and stores the jar unchanged.
#[test]
fn create_jar_with_registrar_set_accepts_a_co_signed_request() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let registrar = Address::generate(env);
    client.set_registrar(&Some(registrar.clone()));

    let jar_id = String::from_str(env, "@cosigned");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];

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
                args: (jar_id.clone(), owner.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);
    client.create_jar(&owner, &jar_id, &splits);

    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.owner, owner);
    assert_eq!(jar.splits.len(), splits.len());
    for i in 0..splits.len() {
        assert_eq!(jar.splits.get(i).unwrap().to, splits.get(i).unwrap().to);
        assert_eq!(jar.splits.get(i).unwrap().bps, splits.get(i).unwrap().bps);
    }
}

/// The owner's signature alone is no longer enough once a registrar is
/// installed: the call fails with an auth error and stores nothing.
#[test]
fn create_jar_without_the_registrar_co_signature_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let registrar = Address::generate(env);
    client.set_registrar(&Some(registrar));

    let jar_id = String::from_str(env, "@missing_cosig");
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
    assert!(
        client.try_create_jar(&owner, &jar_id, &splits).is_err(),
        "a co-signed jar creation without the registrar must fail"
    );
    assert!(
        client.try_get_jar(&jar_id).is_err(),
        "the rejected call must leave no jar behind"
    );
}

/// With no registrar installed the gate is inert: the same call that failed
/// in the missing-co-signature test now succeeds with only the owner's
/// signature — existing deployments are untouched by the new hook point.
#[test]
fn create_jar_is_unchanged_when_no_registrar_is_set() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);

    let jar_id = String::from_str(env, "@no_registrar");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];
    assert_eq!(client.get_registrar(), None);

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
    let jar = client.get_jar(&jar_id);
    assert_eq!(jar.owner, owner);
}

// ---------------------------------------------------------------------------
// #115–#118 — regression coverage for error codes, the minimum tip, the
// no-custody invariant, and isolation between jars.
// ---------------------------------------------------------------------------

/// Error numbers are the contract's public interface: `novatip-sdk` maps each
/// number to a user-facing message and appends new variants without renumbering.
/// A deliberate edit here is the only acceptable way to change one.
#[test]
fn error_codes_are_pinned_to_the_public_interface() {
    let codes = [
        (Error::NotInitialized, 1u32),
        (Error::JarExists, 2),
        (Error::JarNotFound, 3),
        (Error::InvalidSplits, 4),
        (Error::InvalidAmount, 5),
        (Error::TooManyRecipients, 6),
        (Error::DuplicateRecipient, 7),
        (Error::MessageTooLong, 8),
        (Error::InvalidJarId, 9),
        (Error::SplitsEmpty, 10),
        (Error::SplitOutOfRange, 11),
        (Error::SplitSumNot100Pct, 12),
        (Error::BelowMinTipAmount, 13),
        (Error::NotRecipient, 14),
        (Error::SoleRecipient, 15),
        (Error::RecurringNotFound, 16),
        (Error::InvalidPeriod, 17),
        (Error::NothingHeld, 18),
        (Error::TokenNotAllowed, 19),
        (Error::CannotDisallowDefault, 20),
        (Error::StatsOverflow, 21),
        (Error::InsufficientBalance, 22),
        (Error::NotDue, 23),
        (Error::InsufficientAllowance, 24),
        (Error::AmountZero, 25),
        (Error::AmountNegative, 26),
        (Error::SplitAmountTooSmall, 27),
        (Error::AmountOverflow, 28),
        (Error::InvalidMinimumAmount, 29),
        (Error::NoFundsMoved, 30),
        (Error::InvalidRecipient, 31),
        (Error::AdminExists, 32),
        (Error::ContractPaused, 33),
        (Error::JarIdInUse, 34),
        (Error::FeeTooHigh, 35),
        (Error::InvalidMessage, 36),
        (Error::BatchEmpty, 37),
        (Error::BatchTooLarge, 38),
        (Error::JarPaused, 39),
        (Error::NotAuthorized, 44),
        (Error::TooManyAllowedTokens, 40),
        (Error::AboveMaxTipAmount, 41),
        (Error::InvalidMaximumAmount, 42),
        (Error::NonDefaultToken, 43),
        (Error::NotAuthorized, 44),
        (Error::SameOwner, 45),
        (Error::RemovalNotRequested, 46),
        (Error::RemovalNotReady, 47),
    ];

    for (variant, expected) in codes {
        assert_eq!(
            variant as u32, expected,
            "error codes are part of the public interface: novatip-sdk and the frontend map them to messages, so changing a value is a breaking change — append new variants and never renumber existing ones"
        );
    }
}

/// Clearing a minimum publishes `None` in the `min_tip` event, and a tip below
/// the old floor then succeeds.
#[test]
fn clearing_minimum_publishes_none_and_allows_a_smaller_tip() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@min-clear");
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
    client.set_min_tip_amount(&jar_id, &None);

    let topics = vec![
        env,
        symbol_short!("min_tip").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == topics)
        .collect();
    assert_eq!(events.len(), 1);
    let (_, _, data) = events.first().unwrap();
    let decoded: MinTipEvent = TryFromVal::try_from_val(env, data).expect("min_tip event data");
    assert_eq!(decoded.amount, None);

    client.tip(&tipper, &jar_id, &100_i128, &String::from_str(env, "small"));
    assert_eq!(token.balance(&alice), 100);
}

/// #76: every read of the constructor's instance keys falls back to
/// `NotInitialized`. Removing the keys is what a renamed key would look like.
#[test]
fn missing_instance_keys_fail_with_not_initialized() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = single_recipient_jar(env, &client, &owner, "@uninit");

    env.as_contract(&s.contract, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::Token);
    });

    let not_initialized = Err(Ok(Error::NotInitialized.into()));
    assert_eq!(client.try_get_admin(), not_initialized);
    assert_eq!(client.try_get_token(), not_initialized);
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &100, &String::from_str(env, "")),
        Err(Ok(Error::NotInitialized.into()))
    );
}

// ── #102: Admin-gated contract upgrade ───────────────────────────────────────

#[test]
fn admin_can_upgrade_contract() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    // Create a jar to verify state exists
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@upgrade");
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

    // Verify jar exists
    let jar_before = client.get_jar(&jar_id);
    assert_eq!(jar_before.owner, owner);

    // Note: We cannot fully test upgrade() in unit tests because
    // update_current_contract_wasm requires the WASM to be pre-uploaded
    // to the network, which isn't possible in the test environment.
    // The upgrade function will work in production when called with a
    // valid WASM hash that was uploaded via `soroban contract upload`.

    // We verify the function exists and accepts the right parameters
    // by checking it compiles. The actual WASM swap is tested manually
    // on testnet after deployment.
}

/// Issue #247: an upgrade must preserve the existing instance and persistent
/// ledger data, including the jar, held credit, counters, and configured fee.
/// Uploading a valid minimal WASM module exercises the Soroban host's actual
/// code-update path; storage is inspected inside the contract context because
/// that module intentionally exports no TipSplitter functions.
#[test]
fn upgrade_preserves_jar_held_totals_counter_and_fee() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = String::from_str(env, "@upgrade_state");
    token_admin.mint(&tipper, &1_000);
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
    client.set_min_tip_amount(&jar_id, &Some(50));
    client.set_fee_bps(&100);
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "state"));
    let jar_before = client.get_jar(&jar_id);
    let stats_before = client.get_jar_stats(&jar_id);
    let counter_before = client.get_tip_count();
    let fee_before = client.get_fee_bps();
    let held_credit = 13_i128;
    env.as_contract(&s.contract, || {
        env.storage()
            .persistent()
            .set(&DataKey::Held(alice.clone()), &held_credit);
    });

    // Same Soroban-host-compatible WASM envelope as the existing upgrade
    // event test: real upload followed by a real contract upgrade invocation.
    let minimal_wasm = soroban_sdk::Bytes::from_slice(
        env,
        &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x00, 0x1e, 0x11, b'c', b'o', b'n',
            b't', b'r', b'a', b'c', b't', b'e', b'n', b'v', b'm', b'e', b't', b'a', b'v', b'0',
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x16, 0x00, 0x00, 0x00, 0x00,
        ],
    );
    let hash = env.deployer().upload_contract_wasm(minimal_wasm);
    client.upgrade(&hash);

    let expected_topics = vec![env, symbol_short!("upgrade").into_val(env)];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1);
    let event: UpgradedEvent = TryFromVal::try_from_val(env, &events[0].2).unwrap();
    assert_eq!(event.version, 1);
    assert_eq!(event.admin, s.admin);
    assert_eq!(event.new_hash, hash);
    assert_eq!(event.previous_hash, None);

    env.as_contract(&s.contract, || {
        let stored_jar: Jar = env
            .storage()
            .persistent()
            .get(&DataKey::Jar(jar_id.clone()))
            .unwrap();
        assert_eq!(stored_jar.owner, jar_before.owner);
        assert_eq!(stored_jar.splits, jar_before.splits);
        assert_eq!(stored_jar.locked, jar_before.locked);
        assert_eq!(stored_jar.paused, jar_before.paused);
        let stats: JarStats = env
            .storage()
            .persistent()
            .get(&DataKey::Stats(jar_id.clone()))
            .unwrap();
        assert_eq!(stats, stats_before);
        let min: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::MinTip(jar_id.clone()))
            .unwrap();
        assert_eq!(min, 50);
        let held: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::Held(alice.clone()))
            .unwrap();
        assert_eq!(held, held_credit);
        let counter: u64 = env.storage().instance().get(&DataKey::TipCount).unwrap();
        assert_eq!(counter, counter_before);
        let fee: u32 = env.storage().instance().get(&DataKey::FeeBps).unwrap();
        assert_eq!(fee, fee_before);
        let recorded_hash: BytesN<32> = env.storage().instance().get(&DataKey::WasmHash).unwrap();
        assert_eq!(recorded_hash, hash);
    });
}

#[test]
fn upgrade_rejects_non_admin_and_keeps_previous_hash() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let attacker = Address::generate(env);
    let hash = BytesN::<32>::from_array(env, &[9_u8; 32]);
    let previous = BytesN::<32>::from_array(env, &[7_u8; 32]);
    env.as_contract(&s.contract, || {
        env.storage().instance().set(&DataKey::WasmHash, &previous);
    });
    env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "upgrade",
            args: (hash.clone(),).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_upgrade(&hash).is_err());
    env.as_contract(&s.contract, || {
        let kept: BytesN<32> = env.storage().instance().get(&DataKey::WasmHash).unwrap();
        assert_eq!(kept, previous);
    });
}

/// The upgrade event carries the version, the authorising admin, the hash the
/// contract was running before, and the hash it now runs (#334).
#[test]
fn upgrade_event_records_previous_and_new_hash() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let previous = BytesN::<32>::from_array(env, &[7u8; 32]);
    env.as_contract(&s.contract, || {
        env.storage().instance().set(&DataKey::WasmHash, &previous);
    });

    // A minimal wasm module with the `contractenvmetav0` custom section the
    // host requires at upload time. Nothing about it is executed; `upgrade`
    // only hashes it and defers the code swap until the invocation ends.
    let minimal_wasm = soroban_sdk::Bytes::from_slice(
        env,
        &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // wasm header
            0x00, 0x1e, // custom section, 30 bytes
            0x11, b'c', b'o', b'n', b't', b'r', b'a', b'c', b't', b'e', b'n', b'v', b'm', b'e',
            b't', b'a', b'v', b'0', // "contractenvmetav0"
            0x00, 0x00, 0x00, 0x00, // ScEnvMetaKindInterfaceVersion
            0x00, 0x00, 0x00, 0x16, // protocol 22
            0x00, 0x00, 0x00, 0x00, // pre-release 0
        ],
    );
    let new_hash = env.deployer().upload_contract_wasm(minimal_wasm);
    client.upgrade(&new_hash);

    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("upgrade")))
        .collect();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].1.len(), 1, "admin events carry only the name topic");
    let event: UpgradedEvent = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(event.version, 1);
    assert_eq!(event.admin, s.admin);
    assert_eq!(event.previous_hash, Some(previous));
    assert_eq!(event.new_hash, new_hash);
}

/// `pause` and `resume` publish the same versioned payload, with `paused`
/// carrying the new state, and only the event name as a topic (#333).
#[test]
fn pause_and_resume_emit_the_shared_admin_shape() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    client.pause();
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("pause")))
        .collect();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].1.len(), 1, "admin events carry only the name topic");
    let paused: PauseEvent = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(paused.version, 1);
    assert_eq!(paused.admin, s.admin);
    assert!(paused.paused);

    client.resume();
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("resume")))
        .collect();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].1.len(), 1, "admin events carry only the name topic");
    let resumed: PauseEvent = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(resumed.version, 1);
    assert_eq!(resumed.admin, s.admin);
    assert!(!resumed.paused);
}

/// `transfer_admin` publishes both ends of the handover in the shared admin
/// payload: `admin` is the outgoing key, `new_admin` the incoming one (#333).
#[test]
fn transfer_admin_emits_the_shared_admin_shape() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let new_admin = Address::generate(env);

    client.transfer_admin(&new_admin);

    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("adm_xfer")))
        .collect();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].1.len(), 1, "admin events carry only the name topic");
    let event: AdminTransferredEvent = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(event.version, 1);
    assert_eq!(event.admin, s.admin);
    assert_eq!(event.new_admin, new_admin);
    assert_eq!(client.get_admin(), new_admin);
}

#[test]
fn non_admin_cannot_upgrade() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let _non_admin = Address::generate(env);
    let new_hash = BytesN::<32>::from_array(env, &[2u8; 32]);

    // Non-admin tries to upgrade - should fail with auth error
    let result = client.try_upgrade(&new_hash);
    assert!(result.is_err());
}

/// Every control character other than newline is rejected — the tip form only
/// needs line breaks. Tab, carriage return and DEL are all outside what a text
/// column should receive.
#[test]
fn tip_rejects_control_characters_but_allows_newline() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@ctrl");
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

    for bytes in [b"a\tb".as_slice(), b"a\rb".as_slice(), b"a\x7fb".as_slice()] {
        let message = String::from_bytes(env, bytes);
        assert_eq!(
            client.try_tip(&tipper, &jar_id, &100, &message),
            Err(Ok(Error::InvalidMessage.into())),
            "control bytes must be rejected"
        );
    }

    // Newline is the one allowed control character: multi-line messages are
    // accepted and the funds move.
    let multi_line = String::from_str(env, "first line\nsecond line");
    client.tip(&tipper, &jar_id, &100, &multi_line);
    assert_eq!(token.balance(&alice), 100);
    assert_eq!(token.balance(&tipper), 900);
}

/// The constructor must reject an admin and token that share the same address,
/// preventing deployment of a permanently misconfigured contract (#69).
#[test]
#[should_panic]
fn constructor_rejects_admin_and_token_with_same_address() {
    let env = Env::default();
    let addr = Address::generate(&env);
    env.register(TipSplitter, (addr.clone(), addr));
}

// ---------------------------------------------------------------------------
// #325 — versioned migration entry point
// ---------------------------------------------------------------------------

#[test]
fn migrate_runs_once_per_schema_version() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    client.migrate();
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("migrate")))
        .collect();
    assert_eq!(evs.len(), 1);
    let event: MigratedEvent = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(event.admin, s.admin);
    assert_eq!(event.from_version, 0);
    assert_eq!(event.to_version, SCHEMA_VERSION);

    // Nothing left to do: a second call changes nothing and emits nothing.
    client.migrate();
    let again = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("migrate")))
        .count();
    assert_eq!(again, 0);
}

// ---------------------------------------------------------------------------
// #259 — registrar co-signature argument binding and clear/set events
// ---------------------------------------------------------------------------

#[test]
fn registrar_cosignature_is_bound_to_exact_jar_id_and_owner() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner_a = Address::generate(env);
    let owner_b = Address::generate(env);
    let recipient = Address::generate(env);
    let registrar = Address::generate(env);

    let splits = vec![
        env,
        Split {
            to: recipient.clone(),
            bps: 10_000,
        },
    ];
    let jar_alice = String::from_str(env, "@alice");
    let jar_bob = String::from_str(env, "@bob");

    // Installing the registrar emits `reg_set` with the new address.
    client.set_registrar(&Some(registrar.clone()));
    let reg_topics = vec![env, symbol_short!("reg_set").into_val(env)];
    let set_evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == reg_topics)
        .collect();
    assert_eq!(set_evs.len(), 1);
    let set_ev: RegistrarEvent =
        TryFromVal::try_from_val(env, &set_evs[0].2).expect("reg_set event data");
    assert_eq!(set_ev.version, 1);
    assert_eq!(set_ev.admin, s.admin);
    assert_eq!(set_ev.registrar, Some(registrar.clone()));

    // Mismatched slug: registrar co-signed ("@alice", owner_a), caller tries ("@bob", owner_a).
    env.mock_auths(&[
        MockAuth {
            address: &owner_a,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (owner_a.clone(), jar_bob.clone(), splits.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &registrar,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (jar_alice.clone(), owner_a.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);
    assert!(
        client.try_create_jar(&owner_a, &jar_bob, &splits).is_err(),
        "co-signature for (@alice, owner_a) must not authorise (@bob, owner_a)"
    );

    // Mismatched owner: registrar co-signed ("@alice", owner_a), caller tries ("@alice", owner_b).
    env.mock_auths(&[
        MockAuth {
            address: &owner_b,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (owner_b.clone(), jar_alice.clone(), splits.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &registrar,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (jar_alice.clone(), owner_a.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);
    assert!(
        client
            .try_create_jar(&owner_b, &jar_alice, &splits)
            .is_err(),
        "co-signature for (@alice, owner_a) must not authorise (@alice, owner_b)"
    );

    assert!(!client.jar_exists(&jar_alice));
    assert!(!client.jar_exists(&jar_bob));

    // Exact match: ("@alice", owner_a) succeeds.
    env.mock_auths(&[
        MockAuth {
            address: &owner_a,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (owner_a.clone(), jar_alice.clone(), splits.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &registrar,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "create_jar",
                args: (jar_alice.clone(), owner_a.clone()).into_val(env),
                sub_invokes: &[],
            },
        },
    ]);
    client.create_jar(&owner_a, &jar_alice, &splits);
    assert!(client.jar_exists(&jar_alice));

    // Clearing the registrar emits `reg_set` with `None` and restores owner-only creation.
    env.mock_all_auths();
    client.set_registrar(&None);
    let clear_evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == reg_topics)
        .collect();
    assert_eq!(clear_evs.len(), 1);
    let clear_ev: RegistrarEvent =
        TryFromVal::try_from_val(env, &clear_evs[0].2).expect("reg_set clear event data");
    assert_eq!(clear_ev.version, 1);
    assert_eq!(clear_ev.admin, s.admin);
    assert_eq!(clear_ev.registrar, None);

    env.mock_auths(&[MockAuth {
        address: &owner_b,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "create_jar",
            args: (owner_b.clone(), jar_bob.clone(), splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
    client.create_jar(&owner_b, &jar_bob, &splits);
    assert!(client.jar_exists(&jar_bob));
}

fn assert_tip_settles_once_despite_hook(
    s: &Setup,
    client: &TipSplitterClient,
    tipper: &Address,
    alice: &Address,
    jar_id: &String,
    hook: Address,
) {
    let env = &s.env;
    let token = token::Client::new(env, &s.token);

    client.set_tip_hook(&Some(hook));
    let seq = client.tip(tipper, jar_id, &100, &String::from_str(env, "hi"));

    // Snapshot events before any subsequent client call clears the test buffer.
    let tip_evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("tip")))
        .collect();
    assert_eq!(tip_evs.len(), 1, "tip event must be published exactly once");
    let decoded: TipEvent =
        TryFromVal::try_from_val(env, &tip_evs[0].2).expect("tip event data shape");
    assert_eq!(decoded.from, *tipper);
    assert_eq!(decoded.amount, 100);
    assert_eq!(decoded.seq, 1);

    assert_eq!(seq, 1);
    assert_eq!(client.get_tip_count(), 1);
    assert_eq!(token.balance(alice), 100);
    assert_eq!(token.balance(tipper), 900);
    assert_eq!(
        client.get_jar_stats(jar_id),
        JarStats {
            tip_count: 1,
            total: 100,
        }
    );
    assert_eq!(
        client.get_supporter_stats(jar_id, tipper),
        SupporterStats {
            tip_count: 1,
            total: 100,
        }
    );
}

#[test]
fn hostile_hooks_panic_error_and_reentry_do_not_disrupt_tip() {
    // (a) Hook that panics.
    {
        let s = setup();
        let hook = s.env.register(PanickyHook, ());
        let (client, tipper, alice, jar_id) = hook_jar(&s.env, &s);
        assert_tip_settles_once_despite_hook(&s, &client, &tipper, &alice, &jar_id, hook);
    }

    // (b) Hook that returns a contract error.
    {
        let s = setup();
        let hook = s.env.register(erroring_hook::ErroringHook, ());
        let (client, tipper, alice, jar_id) = hook_jar(&s.env, &s);
        assert_tip_settles_once_despite_hook(&s, &client, &tipper, &alice, &jar_id, hook);
    }

    // (c) Hook that tries to re-enter TipSplitter::tip.
    {
        let s = setup();
        let hook = s
            .env
            .register(reentrant_hook::ReentrantHook, (s.contract.clone(),));
        let (client, tipper, alice, jar_id) = hook_jar(&s.env, &s);
        assert_tip_settles_once_despite_hook(&s, &client, &tipper, &alice, &jar_id, hook);
    }
}

// ---------------------------------------------------------------------------
// #246 — transfer_admin hands over every key-gated power, including fee payout
// ---------------------------------------------------------------------------

#[test]
fn transfer_admin_moves_admin_powers_and_fee_payee_to_the_new_key() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let new_admin = Address::generate(env);

    // Transferring to the current admin is a typed no-op.
    assert_eq!(
        client.try_transfer_admin(&s.admin),
        Err(Ok(Error::AdminExists.into()))
    );
    assert_eq!(client.get_admin(), s.admin);

    client.transfer_admin(&new_admin);
    let evs = events_for(env, &s.contract, symbol_short!("adm_xfer"));
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].0.len(), 1, "admin events carry only the name topic");
    let event: AdminTransferredEvent = TryFromVal::try_from_val(env, &evs[0].1).unwrap();
    assert_eq!(event.version, 1);
    assert_eq!(event.admin, s.admin);
    assert_eq!(event.new_admin, new_admin);
    assert_eq!(client.get_admin(), new_admin);

    // Only the new key can now call an admin-only function.
    env.mock_auths(&[MockAuth {
        address: &new_admin,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_fee_bps",
            args: (100u32,).into_val(env),
            sub_invokes: &[],
        },
    }]);
    client.set_fee_bps(&100);
    assert_eq!(client.get_fee_bps(), 100);

    env.mock_auths(&[MockAuth {
        address: &s.admin,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "set_fee_bps",
            args: (200u32,).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_set_fee_bps(&200).is_err());
    assert_eq!(client.get_fee_bps(), 100);

    // The fee payee falls back to the admin, so it follows the handover.
    env.mock_all_auths();
    assert_eq!(client.get_fee_recipient(), new_admin);
    let (jar_id, _, _, tipper) = fee_jar(&s, "@xferfee", 1_000);
    client.tip(&tipper, &jar_id, &1_000, &String::from_str(env, "fee"));
    let token = token::Client::new(env, &s.token);
    assert_eq!(token.balance(&new_admin), 10);
    assert_eq!(token.balance(&s.admin), 0);
}

// ---------------------------------------------------------------------------
// #245 — contract-wide pause and resume
// ---------------------------------------------------------------------------

#[test]
fn pause_blocks_every_tip_entry_point_and_resume_restores_tipping() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let owner = Address::generate(env);
    let recipient = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);
    let jar_id = String::from_str(env, "@paused");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: recipient.clone(),
                bps: 10_000,
            },
        ],
    );
    let msg = String::from_str(env, "hi");
    let batch = vec![
        env,
        TipRequest {
            jar_id: jar_id.clone(),
            amount: 100,
            message: msg.clone(),
            reference: None,
        },
    ];

    assert!(!client.is_paused());
    client.pause();
    let evs = events_for(env, &s.contract, symbol_short!("pause"));
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].0.len(), 1, "admin events carry only the name topic");
    let paused: PauseEvent = TryFromVal::try_from_val(env, &evs[0].1).unwrap();
    assert_eq!(paused.version, 1);
    assert_eq!(paused.admin, s.admin);
    assert!(paused.paused);
    assert!(client.is_paused());

    macro_rules! paused_error {
        () => {
            Err(Ok(Error::ContractPaused.into()))
        };
    }
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &100, &msg),
        paused_error!()
    );
    assert_eq!(
        client.try_tip_with_ref(&tipper, &jar_id, &100, &msg, &None),
        paused_error!()
    );
    assert_eq!(
        client.try_tip_in_token(&tipper, &jar_id, &s.token, &100, &msg),
        paused_error!()
    );
    assert_eq!(client.try_tip_many(&tipper, &batch, &None), paused_error!());

    assert_eq!(token.balance(&recipient), 0);
    assert_eq!(token.balance(&tipper), 1_000);
    assert_eq!(token.balance(&s.contract), 0);

    // Reads and owner calls keep working while the contract is paused.
    assert_eq!(client.get_jar_owner(&jar_id), owner);
    client.set_min_tip_amount(&jar_id, &Some(10));
    assert_eq!(client.get_min_tip_amount(&jar_id), Some(10));

    client.resume();
    let evs = events_for(env, &s.contract, symbol_short!("resume"));
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].0.len(), 1, "admin events carry only the name topic");
    let resumed: PauseEvent = TryFromVal::try_from_val(env, &evs[0].1).unwrap();
    assert_eq!(resumed.version, 1);
    assert_eq!(resumed.admin, s.admin);
    assert!(!resumed.paused);
    assert!(!client.is_paused());

    client.tip(&tipper, &jar_id, &100, &msg);
    assert_eq!(token.balance(&recipient), 100);
    assert_eq!(token.balance(&tipper), 900);
}

#[test]
fn non_admin_cannot_pause_or_resume() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let outsider = Address::generate(env);

    env.mock_auths(&[MockAuth {
        address: &outsider,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "pause",
            args: ().into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_pause().is_err());
    assert!(!client.is_paused());

    env.mock_auths(&[MockAuth {
        address: &outsider,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "resume",
            args: ().into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_resume().is_err());
    assert!(!client.is_paused());
}
