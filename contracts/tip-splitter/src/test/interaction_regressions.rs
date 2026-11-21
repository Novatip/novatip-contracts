//! Cross-feature regressions for batching, leaving jars, allowances and tokens.
use super::*;
use soroban_sdk::vec;

fn one_jar(env: &Env, client: &TipSplitterClient<'_>, slug: &str, to: &Address) -> String {
    let id = String::from_str(env, slug);
    client.create_jar(
        &Address::generate(env),
        &id,
        &vec![env, Split { to: to.clone(), bps: 10_000 }],
    );
    id
}

fn request(env: &Env, id: &String, amount: i128) -> TipRequest {
    TipRequest {
        jar_id: id.clone(),
        amount,
        message: String::from_str(env, "batch"),
        reference: None,
    }
}

fn tip_events(env: &Env, contract: &Address, id: &String) -> std::vec::Vec<TipEvent> {
    env.events()
        .all()
        .iter()
        .filter(|e| {
            e.0 == *contract
                && topic_is_symbol(env, &e.1, 0, symbol_short!("tip"))
                && topic_at::<String>(env, &e.1, 1) == *id
        })
        .map(|e| TipEvent::try_from_val(env, &e.2).expect("tip event data"))
        .collect()
}

// #252: A rejected second entry must roll back the successful first entry.
#[test]
fn batch_paused_middle_jar_rolls_back_all_balances_and_sequences() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &s.token).mint(&tipper, &500);
    let good = one_jar(env, &client, "@batchgood", &alice);
    let paused = one_jar(env, &client, "@batchpause", &bob);
    client.set_jar_paused(&paused, &true);
    assert_eq!(
        client.try_tip_many(&tipper, &vec![env, request(env, &good, 100), request(env, &paused, 100)], &None),
        Err(Ok(Error::JarPaused.into()))
    );
    assert_eq!(token.balance(&tipper), 500);
    assert_eq!(token.balance(&alice), 0);
    assert_eq!(token.balance(&bob), 0);
    assert_eq!(client.get_tip_count(), 0);
    assert_eq!(client.get_jar_stats(&good).total, 0);
    assert_eq!(client.get_jar_stats(&paused).total, 0);
}

#[test]
fn batch_minimum_applies_per_entry_and_preserves_all_state_on_failure() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &s.token).mint(&tipper, &500);
    let id = one_jar(env, &client, "@batchmin", &alice);
    client.set_min_tip_amount(&id, &Some(100));
    assert_eq!(
        client.try_tip_many(&tipper, &vec![env, request(env, &id, 100), request(env, &id, 99)], &None),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    assert_eq!(token.balance(&tipper), 500);
    assert_eq!(token.balance(&alice), 0);
    assert_eq!(client.get_tip_count(), 0);
    assert_eq!(client.get_jar_stats(&id).total, 0);
    assert_eq!(client.get_min_tip_amount(&id), Some(100));
}

#[test]
fn batch_charges_protocol_fee_on_each_entry() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &s.token).mint(&tipper, &1_000);
    let one = one_jar(env, &client, "@batchfee1", &alice);
    let two = one_jar(env, &client, "@batchfee2", &bob);
    client.set_fee_bps(&100);
    let sequences = client.tip_many(&tipper, &vec![env, request(env, &one, 200), request(env, &two, 300)], &None);
    assert_eq!(sequences, vec![env, 1_u64, 2_u64]);
    let first = tip_events(env, &s.contract, &one);
    let second = tip_events(env, &s.contract, &two);
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].fee, 2);
    assert_eq!(second[0].fee, 3);
    assert_eq!(first[0].seq, 1);
    assert_eq!(second[0].seq, 2);
    assert_eq!(token.balance(&s.admin), 5);
    assert_eq!(token.balance(&alice), 198);
    assert_eq!(token.balance(&bob), 297);
    assert_eq!(token.balance(&tipper), 500);
    assert_eq!(client.get_jar_stats(&one), JarStats { tip_count: 1, total: 200 });
    assert_eq!(client.get_jar_stats(&two), JarStats { tip_count: 1, total: 300 });
}

#[test]
fn batch_repeated_jar_creates_distinct_events_and_totals() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let recipient = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &s.token).mint(&tipper, &400);
    let id = one_jar(env, &client, "@batchrepeat", &recipient);
    let seqs = client.tip_many(&tipper, &vec![env, request(env, &id, 100), request(env, &id, 200)], &None);
    assert_eq!(seqs, vec![env, 1_u64, 2_u64]);
    let events = tip_events(env, &s.contract, &id);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[1].seq, 2);
    assert_eq!(events[0].amount, 100);
    assert_eq!(events[1].amount, 200);
    assert_eq!(client.get_jar_stats(&id), JarStats { tip_count: 2, total: 300 });
    assert_eq!(client.get_tip_count(), 2);
    assert_eq!(token.balance(&recipient), 300);
    assert_eq!(token.balance(&tipper), 100);
}

#[test]
fn batch_contract_pause_blocks_before_any_transfer() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let recipient = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &s.token).mint(&tipper, &200);
    let id = one_jar(env, &client, "@batchstop", &recipient);
    client.pause();
    assert_eq!(
        client.try_tip_many(&tipper, &vec![env, request(env, &id, 100)], &None),
        Err(Ok(Error::ContractPaused.into()))
    );
    assert_eq!(token.balance(&recipient), 0);
    assert_eq!(token.balance(&tipper), 200);
    assert_eq!(client.get_jar_stats(&id).total, 0);
    assert_eq!(client.get_tip_count(), 0);
}

// #251: Recipient removal must not be mistaken for an owner-only split edit.
#[test]
fn leave_locked_and_paused_jar_preserves_flags_and_publishes_new_splits() {
    for (locked, paused) in [(true, false), (false, true), (true, true)] {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let owner = Address::generate(env);
        let alice = Address::generate(env);
        let bob = Address::generate(env);
        let id = String::from_str(env, "@leavetest");
        client.create_jar(&owner, &id, &vec![
            env, Split { to: alice.clone(), bps: 6000 },
            Split { to: bob.clone(), bps: 4000 },
        ]);
        if locked { client.lock_splits(&id); }
        if paused { client.set_jar_paused(&id, &true); }

        env.mock_auths(&[MockAuth {
            address: &alice,
            invoke: &MockAuthInvoke {
                contract: &s.contract,
                fn_name: "leave_jar",
                args: (id.clone(), alice.clone()).into_val(env),
                sub_invokes: &[],
            },
        }]);
        client.leave_jar(&id, &alice);
        let expected = vec![env, symbol_short!("splits").into_val(env), id.clone().into_val(env)];
        let updates: std::vec::Vec<_> = env.events().all().iter()
            .filter(|e| e.0 == s.contract && e.1 == expected).collect();
        assert_eq!(updates.len(), 1);
        let event: SplitsUpdatedEvent = TryFromVal::try_from_val(env, &updates[0].2).unwrap();
        env.mock_all_auths();
        let after = client.get_jar(&id);
        assert_eq!(event.version, 1);
        assert_eq!(event.count, 1);
        assert_eq!(event.splits, after.splits);
        assert_eq!(after.splits.get(0).unwrap().to, bob);
        assert_eq!(after.splits.get(0).unwrap().bps, 10_000);
        assert_eq!(client.is_splits_locked(&id), locked);
        assert_eq!(client.is_jar_paused(&id), paused);
    }
}

#[test]
fn leave_jar_preserves_minimum_and_enforces_it_after_redistribution() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &s.token).mint(&tipper, &300);
    let id = String::from_str(env, "@leavemin");
    client.create_jar(&owner, &id, &vec![
        env, Split { to: alice.clone(), bps: 5000 },
        Split { to: bob.clone(), bps: 5000 },
    ]);
    client.set_min_tip_amount(&id, &Some(100));
    client.leave_jar(&id, &alice);
    let expected = vec![env, symbol_short!("splits").into_val(env), id.clone().into_val(env)];
    let emitted: std::vec::Vec<_> = env.events().all().iter()
        .filter(|e| e.0 == s.contract && e.1 == expected).collect();
    assert_eq!(emitted.len(), 1);
    let update: SplitsUpdatedEvent = TryFromVal::try_from_val(env, &emitted[0].2).unwrap();
    let remaining = client.get_jar(&id).splits;
    assert_eq!(update.splits, remaining);
    assert_eq!(client.get_min_tip_amount(&id), Some(100));
    assert_eq!(
        client.try_tip(&tipper, &id, &99, &String::from_str(env, "below")),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    assert_eq!(token.balance(&bob), 0);
    assert_eq!(token.balance(&tipper), 300);
    client.tip(&tipper, &id, &100, &String::from_str(env, "allowed"));
    assert_eq!(token.balance(&bob), 100);
    assert_eq!(client.get_jar_stats(&id).total, 100);
}
