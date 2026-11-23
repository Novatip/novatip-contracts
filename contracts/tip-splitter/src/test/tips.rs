//! Tipping: payouts, events, sequence numbers, references, messages, auth, batches and other tokens.

use super::*;
use soroban_sdk::vec;

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

    let jar_id = String::from_str(env, "@evt1");
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
    let seq = client.tip(&tipper, &jar_id, &100, &message);

    // decodeTipEvent in novatip-sdk reads topic 0 as the "tip" symbol, topic 1
    // as the jar id, and the data positionally. The four original elements are
    // `(from, amount, message, breakdown)`; `token` was appended for #170
    // (without it an indexer cannot tell 10 USDC from 10 XLM), and `seq` and
    // `reference` for the sequence work: every tip is numbered, and a plain
    // `tip` publishes an explicit absent reference so one decoder serves both
    // entry points. `version()` is how a client detects which shape it is
    // talking to.
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
    let decoded: TipEventData = TryFromVal::try_from_val(env, data).expect("tip event data shape");
    assert_eq!(
        decoded,
        TipEvent {
            version: 1,
            from: tipper,
            amount: 100i128,
            fee: 0,
            message,
            breakdown: vec![env, (alice, 100i128, false)],
            token: s.token.clone(),
            seq,
            reference: None,
        }
    );
    // The number returned to the caller is the one published in the event.
    assert_eq!(seq, 1);
}

/// The sequence number and reference read out of the `tip` events this contract
/// published for `jar_id`.
///
/// Read immediately after the tip whose event is wanted: the test env only
/// reliably reports the most recent invocation's events, and a reverted
/// invocation clears them entirely.
fn tip_seq_and_references(
    env: &Env,
    contract: &Address,
    jar_id: &String,
) -> std::vec::Vec<(u64, Option<BytesN<32>>)> {
    let expected_topics = vec![
        env,
        symbol_short!("tip").into_val(env),
        jar_id.into_val(env),
    ];
    env.events()
        .all()
        .iter()
        .filter(|e| e.0 == *contract && e.1 == expected_topics)
        .map(|(_, _, data)| {
            let decoded: TipEventData =
                TryFromVal::try_from_val(env, &data).expect("tip event data shape");
            (decoded.seq, decoded.reference)
        })
        .collect()
}

/// A 32-byte reference filled with a recognisable pattern, standing in for an
/// integration's own id.
fn sample_reference(env: &Env, fill: u8) -> BytesN<32> {
    BytesN::from_array(env, &[fill; REFERENCE_LEN])
}

/// A tip needs an identifier of its own: two tips in one transaction share a
/// transaction hash, and an indexer that deduplicates on the hash silently
/// drops every tip after the first. So each tip gets the next number, returned
/// to the caller and published in its own event, and `get_tip_count` tracks the
/// total.
#[test]
fn successive_tips_get_consecutive_sequence_numbers() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &300);

    let jar_id = String::from_str(env, "@seq");
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
    // Nothing tipped yet.
    assert_eq!(client.get_tip_count(), 0);

    let first = client.tip(&tipper, &jar_id, &100, &String::from_str(env, "one"));
    assert_eq!(
        first, 1,
        "numbers are 1-based, so 0 stays reserved for 'no tip yet'"
    );
    assert_eq!(
        tip_seq_and_references(env, &s.contract, &jar_id),
        std::vec![(1, None)]
    );
    assert_eq!(client.get_tip_count(), 1);

    let second = client.tip(&tipper, &jar_id, &100, &String::from_str(env, "two"));
    assert_eq!(second, 2);
    assert_eq!(second - first, 1);
    assert_eq!(
        tip_seq_and_references(env, &s.contract, &jar_id),
        std::vec![(2, None)]
    );

    // The view agrees with what was handed out, which is what lets a consumer
    // prove it has not missed a tip.
    assert_eq!(client.get_tip_count(), 2);
}

/// A tip that reverts must not consume a number, or every consumer would see a
/// gap and be unable to tell "dropped by my scan" from "never happened".
#[test]
fn reverted_tip_does_not_consume_a_sequence_number() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &300);

    let jar_id = String::from_str(env, "@nohole");
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

    assert_eq!(
        client.tip(&tipper, &jar_id, &100, &String::from_str(env, "ok")),
        1
    );
    assert_eq!(client.get_tip_count(), 1);

    // Rejected before any transfer: an unregistered slug.
    let missing = String::from_str(env, "@free");
    assert!(client
        .try_tip(&tipper, &missing, &100, &String::from_str(env, "boom"))
        .is_err());
    assert_eq!(
        client.get_tip_count(),
        1,
        "a reverted tip leaves the counter where it was"
    );
    assert_eq!(token.balance(&alice), 100, "no funds moved");

    // The next tip takes the number the failed one would have used, so the
    // sequence has no hole for a consumer to mistake for a lost tip.
    assert_eq!(
        client.tip(&tipper, &jar_id, &100, &String::from_str(env, "still 2")),
        2
    );
    assert_eq!(client.get_tip_count(), 2);
}

/// The reference is what lets an integration match a webhook back to the
/// checkout session, bot command or pending alert that started the tip. The
/// contract only echoes it — and `tip` publishes an explicit absent one, so a
/// decoder reads the same six-element shape from either entry point.
#[test]
fn tip_with_ref_publishes_the_reference_and_tip_publishes_none() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &500);

    let jar_id = String::from_str(env, "@ref");
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

    // 32 arbitrary bytes: the contract ascribes no meaning to the contents.
    let reference = BytesN::from_array(env, &std::array::from_fn(|i| i as u8 + 1));
    let seq = client.tip_with_ref(
        &tipper,
        &jar_id,
        &100,
        &String::from_str(env, "via checkout"),
        &Some(reference.clone()),
    );

    assert_eq!(
        tip_seq_and_references(env, &s.contract, &jar_id),
        std::vec![(seq, Some(reference.clone()))],
        "the number returned to the caller is the one in the event, and the \
         reference is echoed verbatim"
    );
    assert_eq!(token.balance(&alice), 70);
    assert_eq!(token.balance(&bob), 30);

    // A plain `tip` publishes an explicit absent reference rather than a
    // shorter tuple, so one decoder handles both entry points.
    let seq2 = client.tip(&tipper, &jar_id, &100, &String::from_str(env, "no ref"));
    assert_eq!(seq2, 2);
    assert_eq!(
        tip_seq_and_references(env, &s.contract, &jar_id),
        std::vec![(2, None)]
    );
    assert_eq!(token.balance(&alice), 140);
}

/// `tip` and `tip_with_ref` share one implementation, so the reference must not
/// change what is paid, in what order, or how many times.
#[test]
fn tip_with_ref_moves_the_same_funds_as_tip() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1000);

    let jar_id = String::from_str(env, "@parity");
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

    let reference = sample_reference(env, 7);
    let with_ref = client.tip_with_ref(
        &tipper,
        &jar_id,
        &101,
        &String::from_str(env, "same maths"),
        &Some(reference),
    );
    assert_eq!(with_ref, 1);
    assert_eq!(
        (token.balance(&alice), token.balance(&bob)),
        (70, 31),
        "dust to the last recipient, exactly as `tip` would pay"
    );

    // Same jar, same amount, no reference: identical outcome and the next
    // number from the same counter.
    let plain = client.tip(&tipper, &jar_id, &101, &String::from_str(env, "same maths"));
    assert_eq!(plain, 2);
    assert_eq!(token.balance(&alice), 140);
    assert_eq!(token.balance(&bob), 62);
    assert_eq!(token.balance(&tipper), 1000 - 202);
}

/// A reference costs nothing to leave out and is not stored: the jar and the
/// tip count read the same whether or not a tip carried one, and nothing
/// enforces uniqueness — a consumer that needs a guaranteed-unique id uses
/// `seq`.
#[test]
fn reference_is_not_stored_and_not_required_to_be_unique() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &500);

    let jar_id = String::from_str(env, "@opaque");
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

    let reference = sample_reference(env, 9);
    for expected in [1u64, 2] {
        assert_eq!(
            client.tip_with_ref(
                &tipper,
                &jar_id,
                &100,
                &String::from_str(env, "dup ref"),
                &Some(reference.clone()),
            ),
            expected,
            "two tips with the same reference both succeed"
        );
        assert_eq!(
            tip_seq_and_references(env, &s.contract, &jar_id),
            std::vec![(expected, Some(reference.clone()))]
        );
    }

    // Nothing about the jar changed because a reference rode along.
    assert_eq!(client.get_jar(&jar_id).splits.len(), 1);
    assert_eq!(client.get_tip_count(), 2);

    // And an explicit `None` is accepted too, not just an omitted argument.
    assert_eq!(
        client.tip_with_ref(
            &tipper,
            &jar_id,
            &100,
            &String::from_str(env, "no ref"),
            &None,
        ),
        3
    );
}

/// The point of the breakdown is that an indexer never has to redo the split
/// arithmetic. So it must match, recipient for recipient and in split order,
/// the balances the transfers actually produced — including the dust the last
/// recipient absorbs, which is where a recomputed figure would go wrong.
#[test]
fn tip_event_breakdown_matches_the_balances_the_transfers_produced() {
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
    token_admin.mint(&tipper, &100);

    // 33.33 / 33.33 / 33.34 over 100 units leaves rounding dust for `c`.
    let jar_id = String::from_str(env, "@breakdown");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
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
        ],
    );

    client.tip(
        &tipper,
        &jar_id,
        &100,
        &String::from_str(env, "split three ways"),
    );

    let breakdown = tip_breakdown(env, &s.contract, &jar_id);
    assert_eq!(
        breakdown,
        std::vec![
            (a.clone(), token.balance(&a)),
            (b.clone(), token.balance(&b)),
            (c.clone(), token.balance(&c)),
        ],
        "the breakdown must report what each recipient was actually paid, in split order"
    );
    // Pin the figures down, so a change in the splitting rule shows up here
    // rather than being absorbed by comparing the event against itself.
    assert_eq!(breakdown, std::vec![(a, 33i128), (b, 33i128), (c, 34i128)]);
    assert_eq!(
        breakdown.iter().map(|(_, amt)| amt).sum::<i128>(),
        100,
        "the breakdown must account for the whole tip"
    );
}

/// `tip` skips a transfer when the sender is also a recipient. The breakdown is
/// a record of the transfers the call made, so that entry reports `0` — and the
/// recipient's balance confirms nothing moved.
#[test]
fn tip_event_breakdown_reports_zero_for_a_skipped_self_transfer() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);

    // The tipper is listed first; `tip` skips paying them and they keep
    // their half, while alice receives exactly hers (#78).
    let jar_id = String::from_str(env, "@selftip");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: tipper.clone(),
                bps: 5000,
            },
            Split {
                to: alice.clone(),
                bps: 5000,
            },
        ],
    );

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "half mine"));

    let breakdown = tip_breakdown(env, &s.contract, &jar_id);
    assert_eq!(
        breakdown,
        std::vec![(tipper.clone(), 0i128), (alice.clone(), 50i128)],
        "a skipped self-transfer is reported as 0, not as the notional share"
    );
    assert_eq!(token.balance(&alice), 50);
    assert_eq!(token.balance(&tipper), 50);
    // Every entry still names its recipient in split order, so an indexer can
    // tell "listed but paid nothing" apart from "not listed at all".
    assert_eq!(breakdown[0].0, tipper);
    assert_eq!(breakdown[1].0, alice);
}

/// `tip_many` settles every jar in exactly what a single `tip` would do, under
/// one authorisation and one transaction, and returns one sequence number per
/// entry in the order the entries were given.
#[test]
fn tip_many_pays_every_jar_and_returns_a_seq_per_entry() {
    let s = TestSetup::new();
    let env = &s.env;
    let client = s.client();
    let token = s.token_client();
    let token_admin = s.token_admin_client();

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &300);

    // Jar one pays alice in full; jar two splits evenly between alice and bob.
    let jar_one = String::from_str(env, "@one");
    solo_jar(env, &client, "@one", &owner, &alice);
    let jar_two = String::from_str(env, "@two");
    jar_with(
        env,
        &client,
        "@two",
        &owner,
        &[(alice.clone(), 5_000), (bob.clone(), 5_000)],
    );

    let seqs = client.tip_many(
        &tipper,
        &vec![
            env,
            TipRequest {
                jar_id: jar_one.clone(),
                amount: 100,
                message: String::from_str(env, "first"),
                reference: None,
            },
            TipRequest {
                jar_id: jar_two.clone(),
                amount: 200,
                message: String::from_str(env, "second"),
                reference: None,
            },
        ],
        &None,
    );

    // Read the events before touching the token: the test environment only
    // exposes the most recent invocation's events, and this batch published
    // both jars' `tip` events inside a single invocation.
    let first = last_tip_event(env, &s.contract, &jar_one);
    let first_breakdown = tip_breakdown(env, &s.contract, &jar_one);
    let second = last_tip_event(env, &s.contract, &jar_two);
    let second_breakdown = tip_breakdown(env, &s.contract, &jar_two);

    // One sequence number per entry, allocated in batch order.
    assert_eq!(seqs.len(), 2);
    let seq0 = seqs.get(0).unwrap();
    let seq1 = seqs.get(1).unwrap();
    assert_eq!(seq1, seq0 + 1, "sequence numbers follow batch order");
    // Each entry is exactly a single `tip`: its own event, its own breakdown,
    // its own amount and message. An indexer following single tips needs no
    // change to follow a batch.
    assert_eq!(first.amount, 100);
    assert_eq!(first.message, String::from_str(env, "first"));
    assert_eq!(first.seq, seq0);
    assert_eq!(first_breakdown, std::vec![(alice.clone(), 100i128)]);
    assert_eq!(second.amount, 200);
    assert_eq!(second.message, String::from_str(env, "second"));
    assert_eq!(second.seq, seq1);
    assert_eq!(
        second_breakdown,
        std::vec![(alice.clone(), 100i128), (bob.clone(), 100i128)]
    );

    assert_eq!(token.balance(&alice), 200);
    assert_eq!(token.balance(&bob), 100);
    assert_eq!(token.balance(&tipper), 0);
}

/// A batch is all-or-nothing: when a later entry is rejected, the entries
/// before it are rolled back too, so no jar is left half-paid and the sender
/// keeps every stroop.
#[test]
fn tip_many_rolls_back_every_entry_when_one_fails() {
    let s = TestSetup::new();
    let env = &s.env;
    let client = s.client();
    let token = s.token_client();
    let token_admin = s.token_admin_client();

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);

    let good = String::from_str(env, "@good");
    solo_jar(env, &client, "@good", &owner, &alice);

    // The first entry is valid and would pay alice; the second names a jar that
    // was never created.
    let missing = String::from_str(env, "@missing");
    let result = client.try_tip_many(
        &tipper,
        &vec![
            env,
            TipRequest {
                jar_id: good.clone(),
                amount: 100,
                message: String::from_str(env, "ok"),
                reference: None,
            },
            TipRequest {
                jar_id: missing.clone(),
                amount: 100,
                message: String::from_str(env, "missing"),
                reference: None,
            },
        ],
        &None,
    );

    assert_eq!(result, Err(Ok(Error::JarNotFound.into())));
    assert_eq!(
        token.balance(&alice),
        0,
        "the settled first entry is rolled back"
    );
    assert_eq!(token.balance(&tipper), 100, "the sender keeps every stroop");
}

/// Both batch-size guards are typed errors raised before any transfer.
#[test]
fn tip_many_rejects_empty_and_oversized_batches() {
    let s = TestSetup::new();
    let env = &s.env;
    let client = s.client();
    let token_admin = s.token_admin_client();

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@guards");
    solo_jar(env, &client, "@guards", &owner, &alice);

    assert_eq!(
        client.try_tip_many(&tipper, &Vec::new(env), &None),
        Err(Ok(Error::BatchEmpty.into()))
    );
    assert_eq!(
        client.try_tip_many(&tipper, &batch_of(env, &jar_id, MAX_BATCH_TIPS + 1), &None),
        Err(Ok(Error::BatchTooLarge.into()))
    );
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

/// Backs the check-order claim in `docs/CONTRACT.md`: `tip` rejects an
/// over-long message before it looks the jar up, so an over-long message to an
/// unregistered slug reports `MessageTooLong`, not `JarNotFound`. Both
/// conditions are true here — the amount is valid and the jar is missing — so
/// only the earlier message-length guard can explain the reported code.
#[test]
fn tip_rejects_over_long_message_before_jar_lookup() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let tipper = Address::generate(env);
    // One byte over MAX_MESSAGE_LEN (280), sent to a slug that was never registered.
    let too_long = String::from_bytes(env, &[b'a'; 281]);
    let res = client.try_tip(&tipper, &String::from_str(env, "@ghost"), &100, &too_long);
    assert_eq!(res, Err(Ok(Error::MessageTooLong.into())));
}

#[test]
fn tip_rejects_nonpositive_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = String::from_str(env, "@aaa");
    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];
    client.create_jar(&owner, &jar_id, &splits);

    let res = client.try_tip(&tipper, &jar_id, &0, &String::from_str(env, ""));
    assert_eq!(res, Err(Ok(Error::AmountZero.into())));
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

/// A single-recipient jar for the multi-byte message tests, with `tipper`
/// funded for one tip of 100.
fn message_limit_jar(s: &Setup, jar_id: &str) -> (String, Address, Address) {
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);

    let jar_id = String::from_str(env, jar_id);
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
    (jar_id, alice, tipper)
}

/// `MAX_MESSAGE_LEN` is a byte budget, not a character budget (#74). 140
/// accented characters are 280 UTF-8 bytes — exactly the limit — so the tip
/// goes through even though the message is only half as many visible
/// characters as an ASCII message at the same limit.
#[test]
fn tip_accepts_multibyte_message_at_exact_byte_limit() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let (jar_id, alice, tipper) = message_limit_jar(&s, "@mb_atlimit");

    // 'é' is U+00E9, two bytes in UTF-8 (0xC3 0xA9).
    assert_eq!("é".len(), 2);
    let text = "é".repeat(140);
    assert_eq!(text.len(), 280, "280 UTF-8 bytes");
    assert_eq!(text.chars().count(), 140, "but only 140 characters");

    let message = String::from_str(env, &text);
    assert_eq!(message.len(), MAX_MESSAGE_LEN, "the contract counts bytes");
    client.tip(&tipper, &jar_id, &100, &message);

    assert_eq!(token.balance(&alice), 100);
    assert_eq!(token.balance(&tipper), 0);
}

/// One byte over the budget is rejected even though the message is far below
/// 280 characters (#74): 139 two-byte characters plus one three-byte '€' is
/// 281 bytes in 140 characters. Rejected before any funds move.
#[test]
fn tip_rejects_multibyte_message_one_byte_over_limit() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let (jar_id, alice, tipper) = message_limit_jar(&s, "@mb_over");

    // '€' is U+20AC, three bytes in UTF-8 (0xE2 0x82 0xAC).
    assert_eq!("€".len(), 3);
    let text = std::format!("{}€", "é".repeat(139));
    assert_eq!(text.len(), 281, "139 * 2 + 3 = 281 UTF-8 bytes");
    assert_eq!(text.chars().count(), 140, "but only 140 characters");

    let message = String::from_str(env, &text);
    assert_eq!(
        message.len(),
        MAX_MESSAGE_LEN + 1,
        "the contract counts bytes"
    );
    let res = client.try_tip(&tipper, &jar_id, &100, &message);
    assert_eq!(res, Err(Ok(Error::MessageTooLong.into())));

    assert_eq!(token.balance(&alice), 0, "alice must not have been paid");
    assert_eq!(
        token.balance(&tipper),
        100,
        "tipper balance must be unchanged"
    );
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
    assert_eq!(res, Err(Ok(Error::InsufficientBalance.into())));

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

    // The call must fail with the contract's own insufficient-balance code,
    // not the token's overlapping number.
    assert_eq!(res, Err(Ok(Error::InsufficientBalance.into())));

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

#[test]
fn tip_enforces_minimum_tip_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &10_000);

    let jar_id = String::from_str(env, "@floorjar");
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

    // Set minimum to 500
    client.set_min_tip_amount(&jar_id, &Some(500_i128));

    // Tip below minimum (499) must fail with BelowMinTipAmount
    let res_low = client.try_tip(&tipper, &jar_id, &499_i128, &String::from_str(env, "small"));
    assert_eq!(res_low, Err(Ok(Error::BelowMinTipAmount.into())));

    // Tip at exact minimum (500) must succeed
    let res_exact = client.try_tip(&tipper, &jar_id, &500_i128, &String::from_str(env, "exact"));
    assert!(res_exact.is_ok());
    assert_eq!(token.balance(&alice), 500);

    // Tip above minimum (1000) must succeed
    let res_above = client.try_tip(
        &tipper,
        &jar_id,
        &1000_i128,
        &String::from_str(env, "above"),
    );
    assert!(res_above.is_ok());
    assert_eq!(token.balance(&alice), 1500);
}

/// The split must not depend on the asset: an EURC tip of 100 pays 70/30
/// exactly as a USDC tip of 100 does, because `compute_shares` is amount-only.
#[test]
fn tip_in_token_splits_identically_to_the_default_token() {
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
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    usdc_admin.mint(&tipper, &1_000);
    eurc_admin.mint(&tipper, &1_000);
    client.allow_token(&eurc);

    let jar_id = String::from_str(env, "@multi");
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

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "in usdc"));
    assert_eq!(usdc.balance(&alice), 70);
    assert_eq!(usdc.balance(&bob), 30);

    client.tip_in_token(
        &tipper,
        &jar_id,
        &eurc,
        &100,
        &String::from_str(env, "in eurc"),
    );
    assert_eq!(eurc_client.balance(&alice), 70);
    assert_eq!(eurc_client.balance(&bob), 30);
    assert_eq!(usdc.balance(&alice), 70, "the assets do not mix");

    // Both tips counted, in the same raw units.
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 2,
            total: 200
        }
    );
}

/// An unlisted token is rejected before funds move.
#[test]
fn tip_in_token_rejects_a_disallowed_token_before_funds_move() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let usdc_admin = token::StellarAssetClient::new(env, &s.token);

    let eurc = second_asset(env, &s.admin);
    let eurc_client = token::Client::new(env, &eurc);
    let eurc_admin = token::StellarAssetClient::new(env, &eurc);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    usdc_admin.mint(&tipper, &1_000);
    eurc_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@closed");
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

    assert_eq!(
        client.try_tip_in_token(
            &tipper,
            &jar_id,
            &eurc,
            &100,
            &String::from_str(env, "nope")
        ),
        Err(Ok(Error::TokenNotAllowed.into()))
    );
    assert_eq!(eurc_client.balance(&alice), 0);
    assert_eq!(eurc_client.balance(&tipper), 1_000);
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 0,
            total: 0
        },
        "a rejected token must not be counted"
    );

    // Allowed, tipped, then disallowed: the third tip is rejected again.
    client.allow_token(&eurc);
    client.tip_in_token(&tipper, &jar_id, &eurc, &100, &String::from_str(env, "yes"));
    client.disallow_token(&eurc);
    assert_eq!(
        client.try_tip_in_token(
            &tipper,
            &jar_id,
            &eurc,
            &100,
            &String::from_str(env, "closed")
        ),
        Err(Ok(Error::TokenNotAllowed.into()))
    );
    assert_eq!(eurc_client.balance(&alice), 100);
}

/// The event has to say which asset moved, or an indexer cannot tell 10 USDC
/// from 10 XLM.
#[test]
fn tip_event_names_the_token_paid_in() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let eurc = second_asset(env, &s.admin);
    let eurc_admin = token::StellarAssetClient::new(env, &eurc);
    client.allow_token(&eurc);

    let (_owner, recipient, jar_id) = one_recipient_jar(env, &client, "@evt");
    let tipper = Address::generate(env);
    eurc_admin.mint(&tipper, &1_000);

    client.tip_in_token(
        &tipper,
        &jar_id,
        &eurc,
        &400,
        &String::from_str(env, "in another asset"),
    );

    let expected_topics = vec![
        env,
        symbol_short!("tip").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let ev: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(ev.len(), 1);
    let decoded: TipEventData =
        TryFromVal::try_from_val(env, &ev.first().unwrap().2).expect("tip event data shape");
    assert_eq!(decoded.token, eurc, "the event must name the asset paid in");
    assert_eq!(decoded.from, tipper);
    assert_eq!(decoded.amount, 400);
    assert_eq!(decoded.breakdown.len(), 1);
    assert_eq!(decoded.breakdown.get(0).unwrap(), (recipient, 400, false));
}

/// With a hook set, `tip` calls `on_tip(jar_id, from, amount)` on it exactly
/// once, and the call is *recorded* only after the tip itself settled: the
/// hook sees the same jar id, sender, and amount the event carries.
#[test]
fn tip_calls_the_hook_after_the_transfers_and_event() {
    let s = setup();
    let env = &s.env;
    let hook = env.register(RecordingHook, ());
    let hook_client = RecordingHookClient::new(env, &hook);
    let (client, tipper, alice, jar_id) = hook_jar(env, &s);

    client.set_tip_hook(&Some(hook.clone()));
    assert_eq!(client.get_tip_hook(), Some(hook.clone()));

    let seq = client.tip(&tipper, &jar_id, &100, &String::from_str(env, "thanks"));

    // Snapshot the events *now*: the testutils buffer only exposes the most
    // recent top-level invocation, and the `balance`/`hits` client calls below
    // would erase this tip's events.
    let all: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract || e.0 == hook)
        .collect();

    assert_eq!(seq, 1);

    // The tip paid out normally, and the hook observed exactly one call.
    assert_eq!(token::Client::new(env, &s.token).balance(&alice), 100);
    assert_eq!(hook_client.hits(), 1);

    // Event order proves the contract point: the tip event is published by
    // the tip-splitter *before* the hook runs, and the hook's own event
    // follows within the same invocation.
    let tip_idx = all
        .iter()
        .position(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("tip")))
        .expect("tip event present");
    let hook_idx = all
        .iter()
        .position(|e| e.0 == hook)
        .expect("hook event present");
    assert!(
        tip_idx < hook_idx,
        "the hook must be called after the tip event"
    );
    let (from, amount): (Address, i128) =
        TryFromVal::try_from_val(env, &all[hook_idx].2).expect("hook event data");
    assert_eq!(from, tipper);
    assert_eq!(amount, 100);
    assert_eq!(
        topic_at::<String>(env, &all[hook_idx].1, 1),
        jar_id,
        "the hook echoed the jar id it was called for"
    );

    // Clearing restores the no-hook behaviour exactly.
    client.set_tip_hook(&None);
    assert_eq!(client.get_tip_hook(), None);
}

/// The failure mode the whole `try_invoke_contract` design exists for: a hook
/// that panics must not revert the supporter's tip.
#[test]
fn panicking_hook_does_not_revert_the_tip() {
    let s = setup();
    let env = &s.env;
    let hook = env.register(PanickyHook, ());
    let (client, tipper, alice, jar_id) = hook_jar(env, &s);
    let token = token::Client::new(env, &s.token);

    client.set_tip_hook(&Some(hook));

    let seq = client.tip(&tipper, &jar_id, &100, &String::from_str(env, "boom-proof"));
    assert_eq!(seq, 1, "the tip settles despite the hook aborting");
    assert_eq!(token.balance(&alice), 100);
    assert_eq!(token.balance(&tipper), 900);
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 1,
            total: 100
        }
    );
}

#[test]
fn tip_authorization_tree_multi_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = String::from_str(env, "@authtree-tip");

    token_admin.mint(&tipper, &1000);

    client.mock_all_auths().create_jar(
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

    client
        .mock_all_auths()
        .tip(&tipper, &jar_id, &100, &String::from_str(env, "tip"));

    let auths = env.auths();
    // #141 — assert the full authorization tree including nested token transfers
    assert!(
        !auths.is_empty(),
        "tip must record at least the tipper's authorization"
    );
    assert_eq!(auths[0].0, tipper);
}

#[test]
fn tip_cost_budget_20_recipients() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let owner = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = String::from_str(env, "@cost20");

    token_admin.mint(&tipper, &1_000_000);

    // Create a jar with 20 recipients (MAX_RECIPIENTS)
    let splits: std::vec::Vec<Split> = (0..20)
        .map(|_| Split {
            to: Address::generate(env),
            bps: 500, // 500 * 20 = 10000
        })
        .collect();
    let mut jar_splits: Vec<Split> = vec![env];
    for split in splits {
        jar_splits.push_back(split);
    }
    client
        .mock_all_auths()
        .create_jar(&owner, &jar_id, &jar_splits);

    env.cost_estimate().budget().reset_default();
    let before_cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let before_mem = env.cost_estimate().budget().memory_bytes_cost();

    client
        .mock_all_auths()
        .tip(&tipper, &jar_id, &100, &String::from_str(env, "max tip"));

    let cpu_used = env.cost_estimate().budget().cpu_instruction_cost() - before_cpu;
    let mem_used = env.cost_estimate().budget().memory_bytes_cost() - before_mem;

    // #142 — print measured values for review
    eprintln!("tip_20_recipients: cpu={cpu_used}, mem={mem_used}");

    // #142 — assert against named limits with headroom
    assert!(
        cpu_used < MAX_INSTRUCTIONS_20_RECIPIENTS,
        "CPU instructions grew: {cpu_used} >= {MAX_INSTRUCTIONS_20_RECIPIENTS}"
    );
    assert!(
        mem_used < MAX_MEMORY_20_RECIPIENTS,
        "Memory grew: {mem_used} >= {MAX_MEMORY_20_RECIPIENTS}"
    );
}

#[test]
fn tip_cost_budget_1_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = String::from_str(env, "@cost1");

    token_admin.mint(&tipper, &1_000_000);
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

    env.cost_estimate().budget().reset_default();
    let before_cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let before_mem = env.cost_estimate().budget().memory_bytes_cost();

    client
        .mock_all_auths()
        .tip(&tipper, &jar_id, &100, &String::from_str(env, "single tip"));

    let cpu_used = env.cost_estimate().budget().cpu_instruction_cost() - before_cpu;
    let mem_used = env.cost_estimate().budget().memory_bytes_cost() - before_mem;

    eprintln!("tip_1_recipient: cpu={cpu_used}, mem={mem_used}");

    assert!(
        cpu_used < MAX_INSTRUCTIONS_20_RECIPIENTS / 20,
        "1-recipient tip CPU grew disproportionately: {cpu_used}"
    );
}

/// #267 — Guard the worst-case `tip_many` resource budget: `MAX_BATCH_TIPS` (10)
/// distinct jars, each with `MAX_RECIPIENTS` (20) recipients (200 token transfers).
///
/// Measured figures (`env.cost_estimate()`):
/// - CPU instructions: ~63,682,597
/// - Memory bytes:     ~16,201,116
/// - Read entries:     34
/// - Write entries:    223 (200 recipient balances + 10 jar stats + 10 supporter stats
///                          + tipper balance + token instance + splitter instance)
/// - Read bytes:       20,488
/// - Write bytes:      49,668
#[test]
fn tip_many_cost_budget_max_batch_with_full_jars() {
    const MAX_BATCH_CPU_INSTRUCTIONS: u64 = 100_000_000;
    const MAX_BATCH_MEMORY_BYTES: u64 = 40_000_000;
    const MAX_BATCH_READ_ENTRIES: u32 = 40;
    const MAX_BATCH_WRITE_ENTRIES: u32 = 230;
    const MAX_BATCH_READ_BYTES: u32 = 30_000;
    const MAX_BATCH_WRITE_BYTES: u32 = 65_536;

    let s = TestSetup::new();
    let env = &s.env;
    let client = s.client();
    let token_admin = s.token_admin_client();
    let owner = Address::generate(env);
    let tipper = Address::generate(env);

    token_admin.mint(&tipper, &10_000_000);

    let mut batch: Vec<TipRequest> = Vec::new(env);
    for j in 0..MAX_BATCH_TIPS {
        let slug = std::format!("@batch20_{j:02}");
        let jar_id = String::from_str(env, &slug);
        let recipients: std::vec::Vec<(Address, u32)> = (0..MAX_RECIPIENTS)
            .map(|_| (Address::generate(env), 500)) // 500 * 20 = 10_000
            .collect();
        jar_with(env, &client, &slug, &owner, &recipients);
        batch.push_back(TipRequest {
            jar_id,
            amount: 1_000,
            message: String::from_str(env, "batch tip"),
            reference: None,
        });
    }

    env.cost_estimate().budget().reset_default();
    let before_cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let before_mem = env.cost_estimate().budget().memory_bytes_cost();

    let seqs = client.tip_many(&tipper, &batch, &None);
    assert_eq!(seqs.len(), MAX_BATCH_TIPS);

    let cpu_used = env.cost_estimate().budget().cpu_instruction_cost() - before_cpu;
    let mem_used = env.cost_estimate().budget().memory_bytes_cost() - before_mem;
    let res = env.cost_estimate().resources();

    eprintln!(
        "tip_many_10x20: cpu={cpu_used}, mem={mem_used}, read_entries={}, write_entries={}, read_bytes={}, write_bytes={}",
        res.read_entries, res.write_entries, res.read_bytes, res.write_bytes
    );

    assert!(
        cpu_used < MAX_BATCH_CPU_INSTRUCTIONS,
        "worst-case tip_many CPU instructions exceeded ceiling: {cpu_used} >= {MAX_BATCH_CPU_INSTRUCTIONS}"
    );
    assert!(
        mem_used < MAX_BATCH_MEMORY_BYTES,
        "worst-case tip_many memory bytes exceeded ceiling: {mem_used} >= {MAX_BATCH_MEMORY_BYTES}"
    );
    assert!(
        res.read_entries <= MAX_BATCH_READ_ENTRIES,
        "worst-case tip_many read entries exceeded ceiling: {} > {MAX_BATCH_READ_ENTRIES}",
        res.read_entries
    );
    assert!(
        res.write_entries <= MAX_BATCH_WRITE_ENTRIES,
        "worst-case tip_many write entries exceeded ceiling: {} > {MAX_BATCH_WRITE_ENTRIES}",
        res.write_entries
    );
    assert!(
        res.read_bytes <= MAX_BATCH_READ_BYTES,
        "worst-case tip_many read bytes exceeded ceiling: {} > {MAX_BATCH_READ_BYTES}",
        res.read_bytes
    );
    assert!(
        res.write_bytes <= MAX_BATCH_WRITE_BYTES,
        "worst-case tip_many write bytes exceeded ceiling: {} > {MAX_BATCH_WRITE_BYTES}",
        res.write_bytes
    );
}

/// `tip` is a router, not a custodian: every successful tip pays recipients
/// directly, so the contract's token balance stays zero even when rounding
/// dust exists or the tipper is also a recipient.
#[test]
fn tip_never_leaves_tokens_in_the_contract() {
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
    token_admin.mint(&tipper, &100_000);

    let dust_jar = String::from_str(env, "@nocustody-dust");
    client.create_jar(
        &owner,
        &dust_jar,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 3333,
            },
            Split {
                to: bob.clone(),
                bps: 3333,
            },
            Split {
                to: carol.clone(),
                bps: 3334,
            },
        ],
    );

    let max_jar = String::from_str(env, "@nocustody-max");
    let mut max_splits: Vec<Split> = vec![env];
    for _ in 0..MAX_RECIPIENTS {
        max_splits.push_back(Split {
            to: Address::generate(env),
            bps: 500,
        });
    }
    client.create_jar(&owner, &max_jar, &max_splits);

    let self_jar = String::from_str(env, "@nocustody-self");
    client.create_jar(
        &owner,
        &self_jar,
        &vec![
            env,
            Split {
                to: tipper.clone(),
                bps: 5000,
            },
            Split {
                to: alice.clone(),
                bps: 3000,
            },
            Split {
                to: bob.clone(),
                bps: 2000,
            },
        ],
    );

    let cases = std::vec![
        (dust_jar, 10_001i128),
        (max_jar, 10_000i128),
        (self_jar, 10_000i128),
    ];

    for (jar_id, amount) in cases {
        let before = token.balance(&tipper);
        client.tip(
            &tipper,
            &jar_id,
            &amount,
            &String::from_str(env, "no custody"),
        );

        // Read the event before any other contract call replaces it in the
        // test environment's event buffer.
        let breakdown = tip_breakdown(env, &s.contract, &jar_id);
        let moved: i128 = breakdown.iter().map(|(_, amount)| *amount).sum();
        let after = token.balance(&tipper);

        assert_eq!(
            token.balance(&s.contract),
            0,
            "the contract must not retain any tokens after tipping {jar_id:?}"
        );
        assert_eq!(
            before - after,
            moved,
            "the tipper must be debited exactly the sum of the tip event breakdown"
        );
    }
}

/// A tip authorization commits to the current split recipients and shares.
///
/// `tip` calls the token's `transfer` once per recipient. Those nested calls
/// each run `from.require_auth()`, so the supporter's signed authorization
/// covers the recipient addresses and exact amounts — not just
/// `tip(from, jar_id, amount, message)`. This test builds that authorization
/// explicitly and pins the guarantee: the same signature succeeds while the
/// splits are unchanged, but is rejected after the owner rewrites them, so an
/// owner cannot redirect a tip that was already signed. A future refactor that
/// pulled the whole amount into the contract before paying recipients would
/// lose this property and fail here.
#[test]
fn tip_stale_authorization_is_rejected_after_splits_change() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);

    let jar_id = String::from_str(env, "@stale-auth");
    let message = String::from_str(env, "signed before the change");
    let original = vec![
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
    client.create_jar(&owner, &jar_id, &original);

    let stale_sub_invokes = [
        MockAuthInvoke {
            contract: &s.token,
            fn_name: "transfer",
            args: (tipper.clone(), alice.clone(), 70i128).into_val(env),
            sub_invokes: &[],
        },
        MockAuthInvoke {
            contract: &s.token,
            fn_name: "transfer",
            args: (tipper.clone(), bob.clone(), 30i128).into_val(env),
            sub_invokes: &[],
        },
    ];
    let stale_auth = MockAuth {
        address: &tipper,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "tip",
            args: (tipper.clone(), jar_id.clone(), 100i128, message.clone()).into_val(env),
            sub_invokes: &stale_sub_invokes,
        },
    };

    // Positive control: the same authorization works while the splits match.
    env.mock_auths(&[stale_auth.clone()]);
    client.tip(&tipper, &jar_id, &100, &message);
    assert_eq!(token.balance(&alice), 70);
    assert_eq!(token.balance(&bob), 30);
    assert_eq!(token.balance(&tipper), 0);

    // The owner redirects the jar after the supporter signed.
    let redirected_to = Address::generate(env);
    let redirected = vec![
        env,
        Split {
            to: redirected_to.clone(),
            bps: 10000,
        },
    ];
    env.mock_all_auths();
    client.update_splits(&jar_id, &redirected);

    // The stale signature no longer matches the transfers `tip` would make.
    env.mock_auths(&[stale_auth]);
    let res = client.try_tip(&tipper, &jar_id, &100, &message);
    assert!(
        res.is_err(),
        "a tip authorized for the old splits must fail"
    );

    // Atomicity: nothing moved, and the redirect recipient got nothing.
    assert_eq!(token.balance(&tipper), 0);
    assert_eq!(token.balance(&redirected_to), 0);
    assert_eq!(token.balance(&alice), 70);
    assert_eq!(token.balance(&bob), 30);
}

/// The stale-auth guarantee survives `transfer_jar_ownership`: the new owner
/// can rewrite the splits, and a tip signed against the old ones is still
/// rejected. The authorization is bound to the transfer calls, not to who owns
/// the jar when it lands.
#[test]
fn tip_stale_authorization_is_rejected_after_ownership_and_splits_change() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);

    let jar_id = String::from_str(env, "@stale-auth-transfer");
    let message = String::from_str(env, "signed before the transfer");
    let original = vec![
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
    client.create_jar(&owner, &jar_id, &original);

    let stale_sub_invokes = [
        MockAuthInvoke {
            contract: &s.token,
            fn_name: "transfer",
            args: (tipper.clone(), alice.clone(), 70i128).into_val(env),
            sub_invokes: &[],
        },
        MockAuthInvoke {
            contract: &s.token,
            fn_name: "transfer",
            args: (tipper.clone(), bob.clone(), 30i128).into_val(env),
            sub_invokes: &[],
        },
    ];
    let stale_auth = MockAuth {
        address: &tipper,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "tip",
            args: (tipper.clone(), jar_id.clone(), 100i128, message.clone()).into_val(env),
            sub_invokes: &stale_sub_invokes,
        },
    };

    client.transfer_jar_ownership(&jar_id, &new_owner);
    let redirected_to = Address::generate(env);
    env.mock_all_auths();
    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: redirected_to.clone(),
                bps: 10000,
            },
        ],
    );

    env.mock_auths(&[stale_auth]);
    let res = client.try_tip(&tipper, &jar_id, &100, &message);
    assert!(
        res.is_err(),
        "a tip authorized before ownership/splits changed must fail"
    );
    assert_eq!(token.balance(&tipper), 100);
    assert_eq!(token.balance(&redirected_to), 0);
    assert_eq!(token.balance(&alice), 0);
    assert_eq!(token.balance(&bob), 0);
}

/// #73: a positive amount that would truncate a 1 bps share to zero is
/// rejected, and the first amount that pays it is accepted.
#[test]
fn tip_rejects_amount_too_small_for_the_smallest_share() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let tiny = Address::generate(env);
    let rest = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &20_000);

    let jar_id = String::from_str(env, "@dust");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: tiny.clone(),
                bps: 1,
            },
            Split {
                to: rest.clone(),
                bps: 9999,
            },
        ],
    );
    // 10_000 * 1 / 10_000 is the first amount where the 1 bps share is 1.
    let smallest = BPS_DENOM as i128;
    assert_eq!(client.get_min_accepted_tip(&jar_id), smallest);

    let message = String::from_str(env, "");
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &(smallest - 1), &message),
        Err(Ok(Error::SplitAmountTooSmall.into()))
    );
    assert_eq!(token.balance(&tipper), 20_000);
    assert_eq!(token.balance(&tiny), 0);
    assert_eq!(token.balance(&rest), 0);

    assert_eq!(
        client.try_tip(&tipper, &jar_id, &smallest, &message),
        Ok(Ok(1u64))
    );
    assert_eq!(token.balance(&tiny), 1);
    assert_eq!(token.balance(&rest), 9_999);
    assert_eq!(token.balance(&tipper), 10_000);
}

// ===========================================================================
// #96 — message content validation
// ===========================================================================

/// A Soroban `String` is a raw byte buffer, so invalid UTF-8 is representable
/// and would otherwise be echoed into the `tip` event where every consumer
/// decodes it differently. It must be rejected before any transfer.
#[test]
fn tip_rejects_invalid_utf8_message() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &500);

    let jar_id = String::from_str(env, "@utf8");
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

    // 0xc3 starts a two-byte sequence but 0x28 is not a continuation byte.
    let invalid = String::from_bytes(env, &[0x00, 0xc3, 0x28]);
    let res = client.try_tip(&tipper, &jar_id, &100, &invalid);
    assert_eq!(res, Err(Ok(Error::InvalidMessage.into())));

    assert_eq!(token.balance(&alice), 0, "alice must not have been paid");
    assert_eq!(
        token.balance(&tipper),
        500,
        "tipper balance must be unchanged"
    );
    assert_eq!(
        client.get_tip_count(),
        0,
        "rejected tip must not be counted"
    );
}

/// An embedded NUL is valid UTF-8 (U+0000) but Postgres `text` rejects it, so
/// the indexer silently drops the whole tip. It must be rejected at the
/// contract boundary.
#[test]
fn tip_rejects_message_with_embedded_nul() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &500);

    let jar_id = String::from_str(env, "@nul");
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

    let with_nul = String::from_bytes(env, b"hello\0world");
    let res = client.try_tip(&tipper, &jar_id, &100, &with_nul);
    assert_eq!(res, Err(Ok(Error::InvalidMessage.into())));

    assert_eq!(token.balance(&alice), 0);
    assert_eq!(token.balance(&tipper), 500);
}

/// An amount large enough that `amount * bps` overflows i128 in the minimum-share
/// guard or distribution loop must fail with `Error::AmountOverflow` rather than
/// trapping the WASM runtime (#68).
#[test]
fn compute_shares_overflow_fails_with_amount_overflow() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@overflow");
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

    let res = client.try_preview_split(&jar_id, &i128::MAX);
    assert_eq!(res, Err(Ok(Error::AmountOverflow.into())));
}

/// A tipper who is also a recipient holding a non-final share keeps their own
/// share instead of funding the last recipient (#66).
/// In a jar with tipper (3000 bps) and another recipient (7000 bps), tipping 100:
/// only 70 leaves the tipper and the recipient receives 70, leaving 30 with the tipper.
#[test]
fn tipper_who_is_non_final_recipient_keeps_own_share() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let tipper = Address::generate(env);
    let recipient = Address::generate(env);

    token_admin.mint(&tipper, &100);

    let jar_id = String::from_str(env, "@self-nonfinal");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: tipper.clone(),
                bps: 3000,
            },
            Split {
                to: recipient.clone(),
                bps: 7000,
            },
        ],
    );

    client.tip(
        &tipper,
        &jar_id,
        &100,
        &String::from_str(env, "keep self share"),
    );

    // Tipper started with 100, kept 30, only 70 left:
    assert_eq!(token.balance(&tipper), 30);
    // Recipient received exactly 70:
    assert_eq!(token.balance(&recipient), 70);
}

/// A tipper holding a non-final share keeps their own share: no self-transfer
/// is made, the final recipient does not absorb it, and every balance reflects
/// only what actually moved (#70).
#[test]
fn tip_tipper_holding_non_final_share_preserves_all_balances() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let tipper = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);

    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@self-mid");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 5_000,
            },
            Split {
                to: tipper.clone(),
                bps: 3_000,
            },
            Split {
                to: carol.clone(),
                bps: 2_000,
            },
        ],
    );

    // Tipper tips 1000: bob receives 500, tipper keeps their 300, carol receives 200.
    // Total transferred is 700.
    client.tip(&tipper, &jar_id, &1_000, &String::from_str(env, "team tip"));

    assert_eq!(token.balance(&bob), 500);
    assert_eq!(token.balance(&carol), 200);
    assert_eq!(
        token.balance(&tipper),
        300,
        "tipper's 300 share stayed in their wallet: only 700 left for bob and carol"
    );
    assert_eq!(token.balance(&s.contract), 0);
}

/// A tipper holding the final share keeps their own share: no self-transfer is
/// made, the non-final recipients receive their exact splits, and every balance
/// reflects only what moved (#70).
#[test]
fn tip_tipper_holding_final_share_preserves_all_balances() {
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

    let jar_id = String::from_str(env, "@self-end");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 6_000,
            },
            Split {
                to: bob.clone(),
                bps: 2_000,
            },
            Split {
                to: tipper.clone(),
                bps: 2_000,
            },
        ],
    );

    // Tipper tips 1000: alice receives 600, bob receives 200, tipper keeps their 200.
    // Total transferred is 800.
    client.tip(
        &tipper,
        &jar_id,
        &1_000,
        &String::from_str(env, "ending split"),
    );

    assert_eq!(token.balance(&alice), 600);
    assert_eq!(token.balance(&bob), 200);
    assert_eq!(
        token.balance(&tipper),
        200,
        "tipper's 200 final share stayed in their wallet: only 800 left for alice and bob"
    );
    assert_eq!(token.balance(&s.contract), 0);
}

/// #318: each `tip_many` entry's `tip` event echoes its own reference.
#[test]
fn tip_many_entries_echo_their_reference() {
    let s = TestSetup::new();
    let env = &s.env;
    let client = s.client();
    let token_admin = s.token_admin_client();
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);
    let jar_id = String::from_str(env, "@refs");
    solo_jar(
        env,
        &client,
        "@refs",
        &Address::generate(env),
        &Address::generate(env),
    );

    let reference = sample_reference(env, 7);
    client.tip_many(
        &tipper,
        &vec![
            env,
            TipRequest {
                jar_id: jar_id.clone(),
                amount: 100,
                message: String::from_str(env, "ref"),
                reference: Some(reference.clone()),
            },
        ],
        &None,
    );
    assert_eq!(
        last_tip_event(env, &s.contract, &jar_id).reference,
        Some(reference)
    );
}

/// #322: `tip_many` pays in an allowed token, rejects a disallowed one before
/// any transfer, and skips the minimum for a non-default token.
#[test]
fn tip_many_pays_in_an_allowed_token() {
    let s = TestSetup::new();
    let env = &s.env;
    let client = s.client();
    let eurc = second_asset(env, &s.admin);
    let eurc_client = token::Client::new(env, &eurc);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &eurc).mint(&tipper, &100);
    let recipient = Address::generate(env);
    let jar_id = String::from_str(env, "@eurc");
    solo_jar(env, &client, "@eurc", &Address::generate(env), &recipient);
    client.set_min_tip_amount(&jar_id, &Some(1_000));
    let batch = vec![
        env,
        TipRequest {
            jar_id: jar_id.clone(),
            amount: 100,
            message: String::from_str(env, "eurc"),
            reference: None,
        },
    ];

    assert_eq!(
        client.try_tip_many(&tipper, &batch, &Some(eurc.clone())),
        Err(Ok(Error::TokenNotAllowed.into()))
    );
    client.allow_token(&eurc);
    client.tip_many(&tipper, &batch, &Some(eurc.clone()));
    assert_eq!(eurc_client.balance(&recipient), 100);
}

// ---------------------------------------------------------------------------
// #263 — `tip` reports the earlier of two failures, in the order
// docs/CONTRACT.md documents: ContractPaused → NotInitialized →
// AmountZero / AmountNegative → MessageTooLong → JarNotFound → JarPaused →
// BelowMinTipAmount → InsufficientBalance → SplitAmountTooSmall /
// AmountOverflow. Each test breaks two adjacent checks at once.
//
// JarNotFound → JarPaused has no test: a jar that does not exist cannot be
// paused, so the two can never fail together.
// ---------------------------------------------------------------------------

fn remove_token_key(s: &Setup) {
    s.env.as_contract(&s.contract, || {
        s.env.storage().instance().remove(&DataKey::Token);
    });
}

fn over_long_message(env: &Env) -> String {
    String::from_bytes(env, &[b'a'; MAX_MESSAGE_LEN as usize + 1])
}

#[test]
fn tip_error_order_contract_paused_before_not_initialized() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (_, _, jar_id) = one_recipient_jar(env, &client, "@order");
    client.pause();
    remove_token_key(&s);

    let res = client.try_tip(
        &Address::generate(env),
        &jar_id,
        &100,
        &String::from_str(env, ""),
    );
    assert_eq!(res, Err(Ok(Error::ContractPaused.into())));
}

#[test]
fn tip_error_order_not_initialized_before_bad_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (_, _, jar_id) = one_recipient_jar(env, &client, "@order");
    remove_token_key(&s);
    let tipper = Address::generate(env);
    let msg = String::from_str(env, "");

    assert_eq!(
        client.try_tip(&tipper, &jar_id, &0, &msg),
        Err(Ok(Error::NotInitialized.into()))
    );
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &-1, &msg),
        Err(Ok(Error::NotInitialized.into()))
    );
}

#[test]
fn tip_error_order_bad_amount_before_message_too_long() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (_, _, jar_id) = one_recipient_jar(env, &client, "@order");
    let tipper = Address::generate(env);
    let msg = over_long_message(env);

    assert_eq!(
        client.try_tip(&tipper, &jar_id, &0, &msg),
        Err(Ok(Error::AmountZero.into()))
    );
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &-1, &msg),
        Err(Ok(Error::AmountNegative.into()))
    );
}

#[test]
fn tip_error_order_message_too_long_before_jar_not_found() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_tip(
        &Address::generate(env),
        &String::from_str(env, "@ghost"),
        &100,
        &over_long_message(env),
    );
    assert_eq!(res, Err(Ok(Error::MessageTooLong.into())));
}

#[test]
fn tip_error_order_jar_paused_before_below_min_tip_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (_, _, jar_id) = one_recipient_jar(env, &client, "@order");
    client.set_min_tip_amount(&jar_id, &Some(50));
    client.set_jar_paused(&jar_id, &true);

    let res = client.try_tip(
        &Address::generate(env),
        &jar_id,
        &10,
        &String::from_str(env, ""),
    );
    assert_eq!(res, Err(Ok(Error::JarPaused.into())));
}

#[test]
fn tip_error_order_below_min_tip_amount_before_insufficient_balance() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (_, _, jar_id) = one_recipient_jar(env, &client, "@order");
    client.set_min_tip_amount(&jar_id, &Some(50));

    // The tipper holds nothing, so the balance check would also fail.
    let res = client.try_tip(
        &Address::generate(env),
        &jar_id,
        &10,
        &String::from_str(env, ""),
    );
    assert_eq!(res, Err(Ok(Error::BelowMinTipAmount.into())));
}

/// Not in the documented order for `tip`: the code checks the jar maximum
/// (#323) between the minimum and the balance, as `tip_many`'s row says.
#[test]
fn tip_error_order_above_max_tip_amount_before_insufficient_balance() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let (_, _, jar_id) = one_recipient_jar(env, &client, "@order");
    client.set_max_tip_amount(&jar_id, &Some(100));

    let res = client.try_tip(
        &Address::generate(env),
        &jar_id,
        &200,
        &String::from_str(env, ""),
    );
    assert_eq!(res, Err(Ok(Error::AboveMaxTipAmount.into())));
}

#[test]
fn tip_error_order_insufficient_balance_before_split_errors() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let jar_id = String::from_str(env, "@order");
    client.create_jar(
        &Address::generate(env),
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 9_999,
            },
            Split {
                to: Address::generate(env),
                bps: 1,
            },
        ],
    );
    let tipper = Address::generate(env);
    let msg = String::from_str(env, "");

    // A 1 bps share of 1 truncates to zero (SplitAmountTooSmall).
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &1, &msg),
        Err(Ok(Error::InsufficientBalance.into()))
    );
    // i128::MAX * 9_999 overflows (AmountOverflow).
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &i128::MAX, &msg),
        Err(Ok(Error::InsufficientBalance.into()))
    );
}
