//! Cross-feature regressions for batching, leaving jars, allowances and tokens.
use super::*;
use soroban_sdk::vec;

fn one_jar(env: &Env, client: &TipSplitterClient<'_>, slug: &str, to: &Address) -> String {
    let id = String::from_str(env, slug);
    client.create_jar(
        &Address::generate(env),
        &id,
        &vec![
            env,
            Split {
                to: to.clone(),
                bps: 10_000,
            },
        ],
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
        client.try_tip_many(
            &tipper,
            &vec![env, request(env, &good, 100), request(env, &paused, 100)],
            &None
        ),
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
        client.try_tip_many(
            &tipper,
            &vec![env, request(env, &id, 100), request(env, &id, 99)],
            &None
        ),
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
    let sequences = client.tip_many(
        &tipper,
        &vec![env, request(env, &one, 200), request(env, &two, 300)],
        &None,
    );
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
    assert_eq!(
        client.get_jar_stats(&one),
        JarStats {
            tip_count: 1,
            total: 200
        }
    );
    assert_eq!(
        client.get_jar_stats(&two),
        JarStats {
            tip_count: 1,
            total: 300
        }
    );
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
    let seqs = client.tip_many(
        &tipper,
        &vec![env, request(env, &id, 100), request(env, &id, 200)],
        &None,
    );
    assert_eq!(seqs, vec![env, 1_u64, 2_u64]);
    let events = tip_events(env, &s.contract, &id);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[1].seq, 2);
    assert_eq!(events[0].amount, 100);
    assert_eq!(events[1].amount, 200);
    assert_eq!(
        client.get_jar_stats(&id),
        JarStats {
            tip_count: 2,
            total: 300
        }
    );
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
        client.create_jar(
            &owner,
            &id,
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
        if locked {
            client.lock_splits(&id);
        }
        if paused {
            client.set_jar_paused(&id, &true);
        }

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
        let expected = vec![
            env,
            symbol_short!("splits").into_val(env),
            id.clone().into_val(env),
        ];
        let updates: std::vec::Vec<_> = env
            .events()
            .all()
            .iter()
            .filter(|e| e.0 == s.contract && e.1 == expected)
            .collect();
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
    client.create_jar(
        &owner,
        &id,
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
    client.set_min_tip_amount(&id, &Some(100));
    client.leave_jar(&id, &alice);
    let expected = vec![
        env,
        symbol_short!("splits").into_val(env),
        id.clone().into_val(env),
    ];
    let emitted: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected)
        .collect();
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

// #253: Fee currency, escrow currency and minimum policy must not cross assets.
#[test]
fn alternate_token_fee_is_paid_in_that_token_and_event_names_the_asset() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let other = second_asset(env, &s.admin);
    let other_token = token::Client::new(env, &other);
    let default_token = token::Client::new(env, &s.token);
    let recipient = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &other).mint(&tipper, &1_000);
    client.allow_token(&other);
    let jar = one_jar(env, &client, "@otherfee", &recipient);
    client.set_fee_bps(&100);
    client.tip_in_token(&tipper, &jar, &other, &500, &String::from_str(env, "fee"));
    let event = last_tip_event(env, &s.contract, &jar);
    assert_eq!(event.token, other);
    assert_eq!(event.fee, 5);
    assert_eq!(
        event.breakdown,
        vec![env, (recipient.clone(), 495_i128, false)]
    );
    assert_eq!(other_token.balance(&recipient), 495);
    assert_eq!(other_token.balance(&s.admin), 5);
    assert_eq!(other_token.balance(&tipper), 500);
    assert_eq!(default_token.balance(&recipient), 0);
    assert_eq!(default_token.balance(&s.admin), 0);
    assert_eq!(
        client.get_jar_stats(&jar),
        JarStats {
            tip_count: 1,
            total: 500
        }
    );
}

#[test]
fn alternate_token_undeliverable_share_is_held_and_event_marks_it() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let other = env.register(TestToken, ());
    let other_token = TestTokenClient::new(env, &other);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    other_token.mint(&tipper, &1_000);
    client.allow_token(&other);
    let jar = String::from_str(env, "@otherheld");
    client.create_jar(
        &Address::generate(env),
        &jar,
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
    other_token.block(&bob);
    client.tip_in_token(&tipper, &jar, &other, &100, &String::from_str(env, "held"));
    let event = last_tip_event(env, &s.contract, &jar);
    assert_eq!(event.token, other);
    assert_eq!(
        event.breakdown,
        vec![
            env,
            (alice.clone(), 70_i128, false),
            (bob.clone(), 30_i128, true)
        ]
    );
    assert_eq!(other_token.balance(&alice), 70);
    assert_eq!(other_token.balance(&bob), 0);
    assert_eq!(other_token.balance(&s.contract), 30);
    assert_eq!(other_token.balance(&tipper), 900);
    assert_eq!(client.get_held(&bob, &other), 30);
    assert_eq!(client.get_held(&bob, &s.token), 0);
    assert_eq!(
        client.get_jar_stats(&jar),
        JarStats {
            tip_count: 1,
            total: 100
        }
    );
    other_token.unblock();
    client.withdraw_held(&bob, &other);
    assert_eq!(other_token.balance(&bob), 30);
    assert_eq!(other_token.balance(&s.contract), 0);
    assert_eq!(client.get_held(&bob, &other), 0);
}

#[test]
fn held_shares_in_two_tokens_are_withdrawn_per_token() {
    let s = setup_hostile();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let default_token = TestTokenClient::new(env, &s.token);
    let other = env.register(TestToken, ());
    let other_token = TestTokenClient::new(env, &other);
    client.allow_token(&other);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    default_token.mint(&tipper, &1_000);
    other_token.mint(&tipper, &1_000);
    let jar = String::from_str(env, "@twotokens");
    client.create_jar(
        &Address::generate(env),
        &jar,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );

    // Bob can receive neither token, so both shares are escrowed.
    default_token.block(&bob);
    other_token.block(&bob);
    client.tip(&tipper, &jar, &100, &String::from_str(env, "first"));
    client.tip_in_token(&tipper, &jar, &other, &40, &String::from_str(env, "second"));
    assert_eq!(client.get_held(&bob, &s.token), 100);
    assert_eq!(client.get_held(&bob, &other), 40);
    assert_eq!(default_token.balance(&s.contract), 100);
    assert_eq!(other_token.balance(&s.contract), 40);

    default_token.unblock();
    other_token.unblock();

    // Each withdrawal pays only its own token.
    client.withdraw_held(&bob, &other);
    assert_eq!(other_token.balance(&bob), 40);
    assert_eq!(default_token.balance(&bob), 0);
    assert_eq!(client.get_held(&bob, &other), 0);
    assert_eq!(client.get_held(&bob, &s.token), 100);

    client.withdraw_held(&bob, &s.token);
    assert_eq!(default_token.balance(&bob), 100);
    assert_eq!(client.get_held(&bob, &s.token), 0);

    // The contract ends holding nothing in either token.
    assert_eq!(default_token.balance(&s.contract), 0);
    assert_eq!(other_token.balance(&s.contract), 0);
    assert_eq!(default_token.balance(&tipper), 900);
    assert_eq!(other_token.balance(&tipper), 960);
}

#[test]
fn default_token_minimum_applies_only_to_default_and_stats_sum_raw_units() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let other = second_asset(env, &s.admin);
    client.allow_token(&other);
    let default_token = token::Client::new(env, &s.token);
    let other_token = token::Client::new(env, &other);
    let recipient = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &s.token).mint(&tipper, &1_000);
    token::StellarAssetClient::new(env, &other).mint(&tipper, &1_000);
    let jar = one_jar(env, &client, "@mintoken", &recipient);
    client.set_min_tip_amount(&jar, &Some(100));
    assert_eq!(
        client.try_tip_in_token(&tipper, &jar, &s.token, &99, &String::from_str(env, "low")),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    assert_eq!(default_token.balance(&tipper), 1_000);
    client.tip_in_token(&tipper, &jar, &other, &50, &String::from_str(env, "other"));
    assert_eq!(last_tip_event(env, &s.contract, &jar).token, other);
    client.tip_in_token(
        &tipper,
        &jar,
        &s.token,
        &100,
        &String::from_str(env, "default"),
    );
    assert_eq!(last_tip_event(env, &s.contract, &jar).token, s.token);
    assert_eq!(default_token.balance(&recipient), 100);
    assert_eq!(other_token.balance(&recipient), 50);
    assert_eq!(default_token.balance(&tipper), 900);
    assert_eq!(other_token.balance(&tipper), 950);
    // These stats are intentionally raw amounts from different currencies.
    assert_eq!(
        client.get_jar_stats(&jar),
        JarStats {
            tip_count: 2,
            total: 150
        }
    );
    assert_eq!(client.get_tip_count(), 2);
}

// #254: Collecting a recurring payment must use the same fee and split
// accounting as a direct tip, while charging exactly the scheduled allowance.
#[test]
fn recurring_fee_matches_manual_tip_and_debits_allowance_exactly() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let supporter = Address::generate(env);
    let jar = String::from_str(env, "@recfee");
    client.create_jar(
        &Address::generate(env),
        &jar,
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
    token_admin.mint(&supporter, &4_000);
    client.set_fee_bps(&100);
    let initial_allowance = 2_000;
    token.approve(
        &supporter,
        &s.contract,
        &initial_allowance,
        &(env.ledger().sequence() + 10_000),
    );
    let id = client.create_recurring(
        &supporter,
        &jar,
        &1_000,
        &100,
        &String::from_str(env, "same"),
    );
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due);

    env.mock_auths(&[]);
    client.collect_recurring(&id);
    let collected = last_tip_event(env, &s.contract, &jar);
    env.mock_all_auths();
    assert_eq!(collected.token, s.token);
    assert_eq!(collected.fee, 10);
    assert_eq!(
        collected.breakdown,
        vec![
            env,
            (alice.clone(), 693_i128, false),
            (bob.clone(), 297_i128, false)
        ]
    );
    assert_eq!(
        token.allowance(&supporter, &s.contract),
        initial_allowance - 1_000
    );
    assert_eq!(token.balance(&s.admin), 10);
    assert_eq!(token.balance(&alice), 693);
    assert_eq!(token.balance(&bob), 297);
    assert_eq!(token.balance(&supporter), 3_000);
    assert_eq!(
        client.get_jar_stats(&jar),
        JarStats {
            tip_count: 1,
            total: 1_000
        }
    );

    client.tip(&supporter, &jar, &1_000, &String::from_str(env, "same"));
    let manual = last_tip_event(env, &s.contract, &jar);
    assert_eq!(collected.amount, manual.amount);
    assert_eq!(collected.fee, manual.fee);
    assert_eq!(collected.breakdown, manual.breakdown);
    assert_eq!(collected.token, manual.token);
    assert_eq!(collected.message, manual.message);
    assert_eq!(manual.seq, collected.seq + 1);
    assert_eq!(token.allowance(&supporter, &s.contract), 1_000);
    assert_eq!(
        client.get_jar_stats(&jar),
        JarStats {
            tip_count: 2,
            total: 2_000
        }
    );
    assert_eq!(token.balance(&s.admin), 20);
    assert_eq!(token.balance(&alice), 1_386);
    assert_eq!(token.balance(&bob), 594);
}

#[test]
fn recurring_held_share_matches_manual_tip_and_charges_full_allowance() {
    let s = setup_hostile();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = TestTokenClient::new(env, &s.token);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let supporter = Address::generate(env);
    let jar = String::from_str(env, "@recheld");
    client.create_jar(
        &Address::generate(env),
        &jar,
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
    token.mint(&supporter, &4_000);
    client.set_fee_bps(&100);
    token.approve(
        &supporter,
        &s.contract,
        &2_000,
        &(env.ledger().sequence() + 10_000),
    );
    let id = client.create_recurring(
        &supporter,
        &jar,
        &1_000,
        &100,
        &String::from_str(env, "held"),
    );
    token.block(&bob);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due);

    env.mock_auths(&[]);
    client.collect_recurring(&id);
    let collected = last_tip_event(env, &s.contract, &jar);
    env.mock_all_auths();
    assert_eq!(collected.token, s.token);
    assert_eq!(collected.fee, 10);
    assert_eq!(
        collected.breakdown,
        vec![
            env,
            (alice.clone(), 693_i128, false),
            (bob.clone(), 297_i128, true)
        ]
    );
    assert_eq!(token.allowance(&supporter, &s.contract), 1_000);
    assert_eq!(token.balance(&supporter), 3_000);
    assert_eq!(token.balance(&s.admin), 10);
    assert_eq!(token.balance(&alice), 693);
    assert_eq!(token.balance(&bob), 0);
    assert_eq!(token.balance(&s.contract), 297);
    assert_eq!(client.get_held(&bob, &s.token), 297);
    assert_eq!(
        client.get_jar_stats(&jar),
        JarStats {
            tip_count: 1,
            total: 1_000
        }
    );

    client.tip(&supporter, &jar, &1_000, &String::from_str(env, "held"));
    let manual = last_tip_event(env, &s.contract, &jar);
    assert_eq!(collected.amount, manual.amount);
    assert_eq!(collected.fee, manual.fee);
    assert_eq!(collected.breakdown, manual.breakdown);
    assert_eq!(collected.token, manual.token);
    assert_eq!(collected.message, manual.message);
    assert_eq!(manual.seq, collected.seq + 1);
    assert_eq!(token.allowance(&supporter, &s.contract), 1_000);
    assert_eq!(client.get_held(&bob, &s.token), 594);
    assert_eq!(token.balance(&s.contract), 594);
    assert_eq!(token.balance(&s.admin), 20);
    assert_eq!(
        client.get_jar_stats(&jar),
        JarStats {
            tip_count: 2,
            total: 2_000
        }
    );
}
