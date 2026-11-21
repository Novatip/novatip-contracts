//! Regression coverage for public split previews and jar lifecycle operations.
//! Each test uses the contract's existing Soroban test setup, not a mock implementation.
use super::*;
use soroban_sdk::vec;

fn three_recipient_jar(env: &Env, client: &TipSplitterClient, slug: &str) -> (String, Vec<Split>) {
    let owner = Address::generate(env);
    let id = String::from_str(env, slug);
    let splits = vec![
        env,
        Split { to: Address::generate(env), bps: 3333 },
        Split { to: Address::generate(env), bps: 3333 },
        Split { to: Address::generate(env), bps: 3334 },
    ];
    client.create_jar(&owner, &id, &splits);
    (id, splits)
}

/// Issue #250: the recipient/amount pairing must be identical to the
/// ordered splits and to the scalar preview both with and without fees.
#[test]
fn preview_split_details_pairs_recipients_and_allocates_dust_after_fees() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (id, _) = three_recipient_jar(env, &client, "@preview_details");
    let stored = client.get_jar(&id);

    for fee_bps in [0_u32, 100_u32] {
        client.set_fee_bps(&fee_bps);
        // 103 is deliberately indivisible by three; the final recipient gets dust.
        let amount = 103_i128;
        let fee = amount * i128::from(fee_bps) / 10_000;
        let expected_net = amount - fee;
        let amounts = client.preview_split(&id, &amount);
        let details = client.preview_split_details(&id, &amount);
        assert_eq!(details.len(), stored.splits.len());
        let mut total = 0_i128;
        for idx in 0..details.len() {
            let entry = details.get(idx).unwrap();
            assert_eq!(entry.to, stored.splits.get(idx).unwrap().to);
            assert_eq!(entry.amount, amounts.get(idx).unwrap());
            total += entry.amount;
        }
        assert_eq!(total, expected_net);
        assert_eq!(details.get(2).unwrap().amount, expected_net - details.get(0).unwrap().amount - details.get(1).unwrap().amount);
    }
}

/// Random amounts cover fee rounding and residual dust without requiring
/// external transfers or manipulating persistent state.
proptest! {
    #![proptest_config(snapshot_stable(32))]

    #[test]
    fn preview_split_details_property_total_equals_post_fee(
        amount in 1_000_i128..1_000_000_000_i128,
        fee_bps in 0_u32..=500_u32,
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let (id, _) = three_recipient_jar(env, &client, "@preview_prop");
        client.set_fee_bps(&fee_bps);
        let shares = client.preview_split(&id, &amount);
        let details = client.preview_split_details(&id, &amount);
        let stored = client.get_jar(&id);
        let mut sum = 0_i128;
        for idx in 0..details.len() {
            let item = details.get(idx).unwrap();
            prop_assert_eq!(item.to, stored.splits.get(idx).unwrap().to);
            prop_assert_eq!(item.amount, shares.get(idx).unwrap());
            sum += item.amount;
        }
        prop_assert_eq!(sum, amount - amount * i128::from(fee_bps) / 10_000);
    }
}

/// Issue #249: deleting a jar must remove its keyed metadata and release the
/// slug, without losing recipient-keyed escrow credit.
#[test]
fn delete_jar_clears_stats_minimum_and_event_and_allows_a_new_owner() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let old_owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let recipient = Address::generate(env);
    let tipper = Address::generate(env);
    let id = String::from_str(env, "@recycle");
    let splits = vec![env, Split { to: recipient.clone(), bps: 10000 }];
    token_admin.mint(&tipper, &1_000);
    client.create_jar(&old_owner, &id, &splits);
    client.set_min_tip_amount(&id, &Some(20));
    client.tip(&tipper, &id, &100, &String::from_str(env, "ok"));
    assert_eq!(client.get_jar_stats(&id).total, 100);

    client.delete_jar(&id);
    let expected_topics = vec![env, symbol_short!("jar_del").into_val(env), id.clone().into_val(env)];
    let deletion: std::vec::Vec<_> = env.events().all().iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics).collect();
    assert_eq!(deletion.len(), 1);
    let payload: JarDeletedEvent = TryFromVal::try_from_val(env, &deletion[0].2).unwrap();
    assert_eq!(payload.version, 1);
    assert_eq!(payload.jar_id, id);

    assert!(!client.jar_exists(&id));
    assert!(client.try_get_jar(&id).is_err());
    assert_eq!(client.try_tip(&tipper, &id, &100, &String::from_str(env, "again")), Err(Ok(Error::JarNotFound.into())));
    env.as_contract(&s.contract, || {
        assert!(!env.storage().persistent().has(&DataKey::Stats(id.clone())));
        assert!(!env.storage().persistent().has(&DataKey::MinTip(id.clone())));
    });

    client.create_jar(&new_owner, &id, &splits);
    assert_eq!(client.get_jar(&id).owner, new_owner);
    assert_eq!(client.get_jar_stats(&id).total, 0);
    assert_eq!(client.get_min_tip_amount(&id), None);
}

#[test]
fn delete_jar_requires_owner_authorization_and_retains_original_state_on_failure() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let stranger = Address::generate(env);
    let recipient = Address::generate(env);
    let id = String::from_str(env, "@no_delete");
    client.create_jar(&owner, &id, &vec![env, Split { to: recipient, bps: 10000 }]);
    client.set_min_tip_amount(&id, &Some(50));

    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "delete_jar",
            args: (id.clone(),).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_delete_jar(&id).is_err());
    assert_eq!(client.get_jar(&id).owner, owner);
    assert_eq!(client.get_min_tip_amount(&id), Some(50));
    env.mock_all_auths();
    assert_eq!(client.try_delete_jar(&String::from_str(env, "@missing")), Err(Ok(Error::JarNotFound.into())));
}

#[test]
fn delete_jar_does_not_remove_recipient_held_balances() {
    let s = setup_hostile();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = TestTokenClient::new(env, &s.token);
    let owner = Address::generate(env);
    let held_recipient = Address::generate(env);
    let tipper = Address::generate(env);
    let id = String::from_str(env, "@held_del");
    token.mint(&tipper, &1_000);
    client.create_jar(&owner, &id, &vec![env, Split { to: held_recipient.clone(), bps: 10000 }]);
    token.block(&held_recipient);
    client.tip(&tipper, &id, &100, &String::from_str(env, "escrow"));
    assert_eq!(client.get_held(&held_recipient), 100);

    client.delete_jar(&id);
    assert!(!client.jar_exists(&id));
    assert_eq!(client.get_held(&held_recipient), 100);
    token.unblock();
    client.withdraw_held(&held_recipient);
    assert_eq!(client.get_held(&held_recipient), 0);
    assert_eq!(token.balance(&held_recipient), 100);
}

/// Issue #248: rename must move the entire jar configuration and its history,
/// with the previous slug no longer resolving.
#[test]
fn rename_jar_moves_stats_minimum_and_flags_and_emits_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let owner = Address::generate(env);
    let recipient = Address::generate(env);
    let tipper = Address::generate(env);
    let old = String::from_str(env, "@before");
    let new = String::from_str(env, "@after");
    token_admin.mint(&tipper, &1_000);
    client.create_jar(&owner, &old, &vec![env, Split { to: recipient.clone(), bps: 10000 }]);
    client.set_min_tip_amount(&old, &Some(50));
    client.tip(&tipper, &old, &100, &String::from_str(env, "before"));
    client.lock_splits(&old);
    client.set_jar_paused(&old, &true);
    let before = client.get_jar(&old);
    let stats = client.get_jar_stats(&old);

    client.rename_jar(&old, &new);
    let expected_topics = vec![env, symbol_short!("jar_rnm").into_val(env), old.clone().into_val(env)];
    let renames: std::vec::Vec<_> = env.events().all().iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics).collect();
    assert_eq!(renames.len(), 1);
    let payload: JarRenamedEvent = TryFromVal::try_from_val(env, &renames[0].2).unwrap();
    assert_eq!(payload.jar_id, old);
    assert_eq!(payload.new_jar_id, new);
    assert_eq!(payload.version, 1);

    assert!(client.try_get_jar(&old).is_err());
    let after = client.get_jar(&new);
    assert_eq!(after.owner, before.owner);
    assert_eq!(after.splits, before.splits);
    assert_eq!(after.locked, before.locked);
    assert_eq!(after.paused, before.paused);
    assert!(after.locked && after.paused);
    assert_eq!(client.get_jar_stats(&new), stats);
    assert_eq!(client.get_min_tip_amount(&new), Some(50));
    env.as_contract(&s.contract, || {
        assert!(!env.storage().persistent().has(&DataKey::Stats(old.clone())));
        assert!(!env.storage().persistent().has(&DataKey::MinTip(old.clone())));
    });
    client.set_jar_paused(&new, &false);
    client.tip(&tipper, &new, &75, &String::from_str(env, "after"));
    assert_eq!(client.get_jar_stats(&new).total, 175);
    assert_eq!(token::Client::new(env, &s.token).balance(&recipient), 175);
}

#[test]
fn rename_jar_rejects_existing_missing_and_unauthorized_slugs() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let stranger = Address::generate(env);
    let recipient = Address::generate(env);
    let old = String::from_str(env, "@rename_src");
    let taken = String::from_str(env, "@rename_dst");
    let missing = String::from_str(env, "@rename_missing");
    let splits = vec![env, Split { to: recipient, bps: 10000 }];
    client.create_jar(&owner, &old, &splits);
    client.create_jar(&owner, &taken, &splits);
    assert_eq!(client.try_rename_jar(&old, &taken), Err(Ok(Error::JarIdInUse.into())));
    assert_eq!(client.try_rename_jar(&missing, &String::from_str(env, "@newslug")), Err(Ok(Error::JarNotFound.into())));

    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "rename_jar",
            args: (old.clone(), String::from_str(env, "@unauth")).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_rename_jar(&old, &String::from_str(env, "@unauth")).is_err());
    assert_eq!(client.get_jar(&old).owner, owner);
    assert!(client.jar_exists(&taken));
}
