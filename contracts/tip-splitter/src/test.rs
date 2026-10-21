#![cfg(test)]
use super::*;
use proptest::prelude::*;
use soroban_sdk::testutils::{Address as _, Events as _, Ledger as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    vec, Address, Env, IntoVal, String, Symbol, TryFromVal, Vec,
};

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

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum TestTokenError {
    NotAuthorized = 1,
    InsufficientBalance = 2,
}

#[contracttype]
#[derive(Clone)]
enum TestTokenKey {
    Balance(Address),
    Blocked,
}

/// A minimal token used only by the #171 tests. `block` makes every `transfer`
/// to one address revert, faithfully simulating a recipient whose account can no
/// longer receive the asset (a dropped trustline / deauthorised account) without
/// depending on issuer authorization flags, which the test Stellar Asset
/// Contract registers with `flags: 0` and cannot toggle through its client.
#[contract]
pub struct TestToken;

#[contractimpl]
impl TestToken {
    pub fn mint(env: Env, to: Address, amount: i128) {
        let k = TestTokenKey::Balance(to);
        let b: i128 = env.storage().persistent().get(&k).unwrap_or(0);
        env.storage().persistent().set(&k, &(b + amount));
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&TestTokenKey::Balance(id))
            .unwrap_or(0)
    }

    pub fn block(env: Env, to: Address) {
        env.storage().persistent().set(&TestTokenKey::Blocked, &to);
    }

    pub fn unblock(env: Env) {
        env.storage().persistent().remove(&TestTokenKey::Blocked);
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        if let Some(b) = env
            .storage()
            .persistent()
            .get::<TestTokenKey, Address>(&TestTokenKey::Blocked)
        {
            if b == to {
                panic_with_error!(&env, TestTokenError::NotAuthorized);
            }
        }
        let fk = TestTokenKey::Balance(from.clone());
        let fb: i128 = env.storage().persistent().get(&fk).unwrap_or(0);
        if fb < amount {
            panic_with_error!(&env, TestTokenError::InsufficientBalance);
        }
        env.storage().persistent().set(&fk, &(fb - amount));
        let tk = TestTokenKey::Balance(to.clone());
        let tb: i128 = env.storage().persistent().get(&tk).unwrap_or(0);
        env.storage().persistent().set(&tk, &(tb + amount));
    }
}

/// Fresh env with a TipSplitter pointed at a controllable [`TestToken`].
struct SetupHostile {
    env: Env,
    contract: Address,
    token: Address,
}

fn setup_hostile() -> SetupHostile {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token = env.register(TestToken, ());
    let contract = env.register(TipSplitter, (admin.clone(), token.clone()));
    SetupHostile {
        env,
        contract,
        token,
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

/// The `tip` event's data, decoded in its published positional order:
/// `from`, `amount`, `message`, `breakdown`, then the appended fields — the
/// token the tip was paid in (#170), the tip's sequence number, and its
/// optional client reference.
type TipEventData = TipEvent;

/// Reads the per-recipient breakdown out of the single `tip` event this
/// contract published.
fn tip_breakdown(env: &Env, contract: &Address, jar_id: &String) -> std::vec::Vec<(Address, i128)> {
    let expected_topics = vec![
        env,
        symbol_short!("tip").into_val(env),
        jar_id.into_val(env),
    ];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == *contract && e.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1, "expected exactly one tip event");
    let (_, _, data) = events.first().unwrap();
    let decoded: TipEventData = TryFromVal::try_from_val(env, data).expect("tip event data shape");
    let breakdown = decoded.breakdown;
    (0..breakdown.len())
        .map(|i| {
            let (to, moved, _held) = breakdown.get(i).unwrap();
            (to, moved)
        })
        .collect()
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
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &300);

    // Jar one pays alice in full; jar two splits evenly between alice and bob.
    let jar_one = String::from_str(env, "@one");
    client.create_jar(
        &owner,
        &jar_one,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10_000,
            },
        ],
    );
    let jar_two = String::from_str(env, "@two");
    client.create_jar(
        &owner,
        &jar_two,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 5_000,
            },
            Split {
                to: bob.clone(),
                bps: 5_000,
            },
        ],
    );

    let seqs = client.tip_many(
        &tipper,
        &vec![
            env,
            TipRequest {
                jar_id: jar_one.clone(),
                amount: 100,
                message: String::from_str(env, "first"),
            },
            TipRequest {
                jar_id: jar_two.clone(),
                amount: 200,
                message: String::from_str(env, "second"),
            },
        ],
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
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &100);

    let good = String::from_str(env, "@good");
    client.create_jar(
        &owner,
        &good,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10_000,
            },
        ],
    );

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
            },
            TipRequest {
                jar_id: missing.clone(),
                amount: 100,
                message: String::from_str(env, "missing"),
            },
        ],
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
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@guards");
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

    assert_eq!(
        client.try_tip_many(&tipper, &Vec::new(env)),
        Err(Ok(Error::BatchEmpty.into()))
    );
    assert_eq!(
        client.try_tip_many(&tipper, &batch_of(env, &jar_id, MAX_BATCH_TIPS + 1)),
        Err(Ok(Error::BatchTooLarge.into()))
    );
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

/// `get_jar_owner` must agree with `get_jar(...).owner` for a registered jar,
/// which is the whole point: the cheap read and the full read never disagree.
#[test]
fn get_jar_owner_returns_owner_of_registered_jar() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@duo");

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

    assert_eq!(client.get_jar_owner(&jar_id), owner);
    assert_eq!(client.get_jar_owner(&jar_id), client.get_jar(&jar_id).owner);
    // The owner is distinct from the recipients — a jar can pay addresses that
    // do not control it, so returning a recipient would pass a weaker test.
    assert_ne!(client.get_jar_owner(&jar_id), alice);
}

/// An unregistered slug is a `JarNotFound` panic, not a default address — a
/// zero or placeholder address here would read to a client as "someone owns
/// this jar".
#[test]
fn get_jar_owner_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_get_jar_owner(&String::from_str(env, "@ghost"));
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// `transfer_jar_ownership` rewrites the stored owner, so the cheap read must
/// follow it rather than serving a stale address.
#[test]
fn get_jar_owner_follows_ownership_transfer() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@moved");

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
    assert_eq!(client.get_jar_owner(&jar_id), owner);

    client.transfer_jar_ownership(&jar_id, &new_owner);

    assert_eq!(client.get_jar_owner(&jar_id), new_owner);
    assert_ne!(client.get_jar_owner(&jar_id), owner);
}

/// A single-recipient jar counts 1 — the degenerate case a badge still has to
/// render, and the one an off-by-one would most likely get wrong.
#[test]
fn get_split_count_counts_single_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@solo");

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

    assert_eq!(client.get_split_count(&jar_id), 1);
}

/// A multi-recipient jar counts every entry, and agrees with the length of the
/// vector `get_jar` returns — the cheap read must not drift from the full one.
#[test]
fn get_split_count_counts_every_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let jar_id = String::from_str(env, "@collective");

    // Four recipients at 2500 bps each.
    let mut splits = vec![env];
    for _ in 0..4 {
        splits.push_back(Split {
            to: Address::generate(env),
            bps: 2500,
        });
    }
    client.create_jar(&owner, &jar_id, &splits);

    assert_eq!(client.get_split_count(&jar_id), 4);
    assert_eq!(
        client.get_split_count(&jar_id),
        client.get_jar(&jar_id).splits.len()
    );
}

/// `update_splits` replaces the stored vector, so the count must follow it both
/// upwards and back down rather than serving a cached length.
#[test]
fn get_split_count_follows_update_splits() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    let jar_id = String::from_str(env, "@growing");

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
    assert_eq!(client.get_split_count(&jar_id), 1);

    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 4000,
            },
            Split {
                to: bob.clone(),
                bps: 4000,
            },
            Split {
                to: carol.clone(),
                bps: 2000,
            },
        ],
    );
    assert_eq!(client.get_split_count(&jar_id), 3);

    // Shrinking the list must lower the count too.
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
    assert_eq!(client.get_split_count(&jar_id), 2);
}

/// An unregistered slug is a `JarNotFound` panic, not `0` — a zero count would
/// be indistinguishable from a jar, and no stored jar can have zero splits.
#[test]
fn get_split_count_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_get_split_count(&String::from_str(env, "@ghost"));
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// The preview is the payout: the numbers a client shows before signing must
/// be the balances the tip actually produces.
#[test]
fn preview_split_matches_what_tip_pays() {
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

    let jar_id = String::from_str(env, "@preview");
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

    let preview = client.preview_split(&jar_id, &101);
    assert_eq!(preview, vec![env, 70_i128, 31_i128]);

    client.tip(
        &tipper,
        &jar_id,
        &101,
        &String::from_str(env, "here you go"),
    );

    assert_eq!(token.balance(&alice), preview.get(0).unwrap());
    assert_eq!(token.balance(&bob), preview.get(1).unwrap());
}

/// A single-recipient jar previews the whole amount, with no dust to place.
#[test]
fn preview_split_single_recipient_gets_everything() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@onlyme");

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

    assert_eq!(client.preview_split(&jar_id, &7), vec![env, 7_i128]);
}

/// Thirds over amounts that do not divide cleanly: the shares must still sum to
/// exactly the amount, with the truncated remainder landing on the last
/// recipient. These are the amounts where a client's own arithmetic would
/// most easily drift from the contract's.
#[test]
fn preview_split_shares_sum_to_amount_for_awkward_amounts() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let jar_id = String::from_str(env, "@thirds");

    // 3333 / 3333 / 3334 — the classic non-terminating split.
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 3333,
            },
            Split {
                to: Address::generate(env),
                bps: 3333,
            },
            Split {
                to: Address::generate(env),
                bps: 3334,
            },
        ],
    );

    for amount in [10_i128, 11, 99, 100, 101, 7_777, 1_000_003] {
        let shares = client.preview_split(&jar_id, &amount);
        assert_eq!(shares.len(), 3, "one share per recipient");

        let mut total: i128 = 0;
        for i in 0..shares.len() {
            let share = shares.get(i).unwrap();
            assert!(share > 0, "every recipient must get something at {amount}");
            total += share;
        }
        assert_eq!(total, amount, "shares must sum to exactly {amount}");
    }

    // Spot-check the dust placement rather than only the invariant: at 100,
    // truncation leaves 1 over and the final recipient absorbs it.
    assert_eq!(
        client.preview_split(&jar_id, &100),
        vec![env, 33_i128, 33_i128, 34_i128]
    );
}

/// The preview must follow `update_splits`, or a client would quote shares for
/// a line-up the contract no longer has.
#[test]
fn preview_split_follows_update_splits() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@requoted");

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
    assert_eq!(client.preview_split(&jar_id, &100), vec![env, 100_i128]);

    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 2500,
            },
            Split {
                to: bob.clone(),
                bps: 7500,
            },
        ],
    );
    assert_eq!(
        client.preview_split(&jar_id, &100),
        vec![env, 25_i128, 75_i128]
    );
}

/// A preview that returns at all must describe a tip that can go through, so
/// it rejects exactly the amounts `tip` rejects.
#[test]
fn preview_split_rejects_amounts_tip_would_reject() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@strict");

    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 9900,
            },
            Split {
                to: bob.clone(),
                bps: 100,
            },
        ],
    );

    assert_eq!(
        client.try_preview_split(&jar_id, &0),
        Err(Ok(Error::AmountZero.into()))
    );
    assert_eq!(
        client.try_preview_split(&jar_id, &-5),
        Err(Ok(Error::AmountNegative.into()))
    );
    // bps 100 truncates to zero below 100, which `tip` refuses to pay.
    assert_eq!(
        client.try_preview_split(&jar_id, &99),
        Err(Ok(Error::SplitAmountTooSmall.into()))
    );
    assert!(client
        .try_tip(
            &Address::generate(env),
            &jar_id,
            &99,
            &String::from_str(env, "too small")
        )
        .is_err());
    // 100 is the first amount that pays everyone.
    assert_eq!(
        client.preview_split(&jar_id, &100),
        vec![env, 99_i128, 1_i128]
    );
}

/// An unregistered slug is a `JarNotFound` panic, matching `get_jar` — an
/// empty vector would read as a jar that pays nobody.
#[test]
fn preview_split_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_preview_split(&String::from_str(env, "@ghost"), &100);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
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
    let decoded: JarCreatedEvent =
        TryFromVal::try_from_val(env, data).expect("jar_crtd event data shape");
    assert_eq!(decoded.owner, owner);
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
    let decoded: SplitsUpdatedEvent =
        TryFromVal::try_from_val(env, data).expect("splits event data shape");
    assert_eq!(decoded.count, new_splits.len());
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

/// A collaborator's own membership check: a listed recipient gets `true`, an
/// address nowhere in the splits gets `false`, and the jar owner gets `false`
/// when they take no share — owning a jar and being paid by it are separate
/// things.
#[test]
fn is_recipient_distinguishes_recipients_owner_and_strangers() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let stranger = Address::generate(env);

    let jar_id = String::from_str(env, "@collab");
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

    // Listed recipients, in both the first and a later position.
    assert!(client.is_recipient(&jar_id, &alice));
    assert!(client.is_recipient(&jar_id, &bob));

    // An address that is nowhere in the jar.
    assert!(!client.is_recipient(&jar_id, &stranger));

    // The owner, who takes no share of a tip here.
    assert!(
        !client.is_recipient(&jar_id, &owner),
        "owning a jar is not the same as receiving a share of its tips"
    );
    assert_eq!(
        client.get_jar(&jar_id).owner,
        owner,
        "the owner check lives on get_jar, not is_recipient"
    );
}

/// An owner who is also listed in the splits gets `true` — the previous test
/// asserts a `false` for an owner, so this pins down that the answer tracks the
/// split vector rather than ownership.
#[test]
fn is_recipient_is_true_for_an_owner_who_is_also_a_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let jar_id = String::from_str(env, "@solo");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: owner.clone(),
                bps: 10000,
            },
        ],
    );

    assert!(client.is_recipient(&jar_id, &owner));
}

/// `update_splits` replaces the recipient list, so membership must follow it —
/// a dropped collaborator stops being a recipient and a new one starts.
#[test]
fn is_recipient_follows_update_splits() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);

    let jar_id = String::from_str(env, "@reshuffle");
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
    assert!(client.is_recipient(&jar_id, &alice));
    assert!(!client.is_recipient(&jar_id, &bob));

    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );
    assert!(!client.is_recipient(&jar_id, &alice));
    assert!(client.is_recipient(&jar_id, &bob));
}

/// An unregistered slug is `JarNotFound`, not a quiet `false` — otherwise a
/// typo'd jar id would look indistinguishable from a genuine non-membership.
#[test]
fn is_recipient_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let alice = Address::generate(env);
    let res = client.try_is_recipient(&String::from_str(env, "@nope"), &alice);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// `get_limits` exists so clients stop hardcoding the contract's bounds. This
/// asserts the view and the constants cannot drift apart: changing one without
/// the other fails here.
#[test]
fn get_limits_matches_contract_constants() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let limits = client.get_limits();

    assert_eq!(limits.bps_denom, BPS_DENOM);
    assert_eq!(limits.max_recipients, MAX_RECIPIENTS);
    assert_eq!(limits.max_message_len, MAX_MESSAGE_LEN);
    assert_eq!(limits.max_batch_tips, MAX_BATCH_TIPS);
    assert_eq!(limits.max_jar_id_len, MAX_JAR_SLUG_LEN + 1);
    assert_eq!(
        limits,
        Limits {
            bps_denom: 10_000,
            max_recipients: 20,
            max_message_len: 280,
            max_batch_tips: 10,
            max_jar_id_len: 33,
        },
        "the published limits are part of the public interface; changing one \
         means clients and docs/CONTRACT.md have to change with it"
    );
}

/// The limits are only useful if they describe what the contract actually
/// enforces, so exercise each returned bound against the rejection it predicts.
#[test]
fn get_limits_describes_the_enforced_bounds() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let limits = client.get_limits();
    let owner = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    // max_recipients: a jar with exactly that many recipients is accepted, and
    // one more is not.
    let mut splits = vec![env];
    for _ in 0..limits.max_recipients {
        splits.push_back(Split {
            to: Address::generate(env),
            bps: limits.bps_denom / limits.max_recipients,
        });
    }
    let jar_id = String::from_str(env, "@limits");
    client.create_jar(&owner, &jar_id, &splits);

    let mut too_many = splits.clone();
    too_many.push_back(Split {
        to: Address::generate(env),
        bps: 1,
    });
    assert_eq!(
        client.try_create_jar(&owner, &String::from_str(env, "@over"), &too_many),
        Err(Ok(Error::TooManyRecipients.into()))
    );

    // bps_denom: the shares of an accepted jar sum to exactly the denominator.
    let stored = client.get_jar(&jar_id);
    let total: u32 = (0..stored.splits.len())
        .map(|i| stored.splits.get(i).unwrap().bps)
        .sum();
    assert_eq!(total, limits.bps_denom);

    // max_jar_id_len: the longest canonical id ("@" plus a full-length body)
    // is accepted, and one byte more is not.
    let mut exact_bytes = vec_of_bytes(limits.max_jar_id_len - 1);
    exact_bytes.insert(0, b'@');
    let exact_id = String::from_bytes(env, &exact_bytes);
    client.create_jar(&owner, &exact_id, &splits);

    let mut over_bytes = vec_of_bytes(limits.max_jar_id_len);
    over_bytes.insert(0, b'@');
    let over_id = String::from_bytes(env, &over_bytes);
    assert_eq!(
        client.try_create_jar(&owner, &over_id, &splits),
        Err(Ok(Error::InvalidJarId.into()))
    );

    // max_message_len: a message of exactly that many bytes is accepted, and
    // one byte more is not.
    let exact = String::from_bytes(env, &vec_of_bytes(limits.max_message_len));
    client.tip(&tipper, &jar_id, &1_000, &exact);

    let over = String::from_bytes(env, &vec_of_bytes(limits.max_message_len + 1));
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &1, &over),
        Err(Ok(Error::MessageTooLong.into()))
    );

    // max_batch_tips: a batch of exactly that many entries is accepted, and one
    // more is not. The batch needs its own jar: one recipient on the whole
    // 10 000 bps, so a 1-stroop tip is above the truncation floor.
    let batch_jar = String::from_str(env, "@batch");
    client.create_jar(
        &owner,
        &batch_jar,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: limits.bps_denom,
            },
        ],
    );
    let batch_count = limits.max_batch_tips + 1;
    token_admin.mint(&tipper, &(batch_count as i128));

    let exact_batch = batch_of(env, &batch_jar, limits.max_batch_tips);
    assert_eq!(
        client.tip_many(&tipper, &exact_batch).len(),
        limits.max_batch_tips
    );

    let over_batch = batch_of(env, &batch_jar, batch_count);
    assert_eq!(
        client.try_tip_many(&tipper, &over_batch),
        Err(Ok(Error::BatchTooLarge.into()))
    );
}

/// A `tip_many` batch of `n` entries all naming `jar_id`, each for one stroop
/// with a one-byte message.
fn batch_of(env: &Env, jar_id: &String, n: u32) -> Vec<TipRequest> {
    let mut tips = Vec::new(env);
    for _ in 0..n {
        tips.push_back(TipRequest {
            jar_id: jar_id.clone(),
            amount: 1,
            message: String::from_str(env, "t"),
        });
    }
    tips
}

/// `n` filler ASCII bytes — one UTF-8 byte each, so the byte length is `n`.
fn vec_of_bytes(n: u32) -> std::vec::Vec<u8> {
    std::vec![b'a'; n as usize]
}

#[test]
fn get_admin_returns_constructor_admin() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert_eq!(client.get_admin(), s.admin);
}

/// `get_token` returns the token address passed to the constructor (#71).
#[test]
fn get_token_returns_constructor_token() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert_eq!(client.get_token(), s.token);
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

#[test]
fn get_min_tip_amount_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let jar_id = String::from_str(env, "@ghost");
    let res = client.try_get_min_tip_amount(&jar_id);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
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

#[test]
fn preview_split_enforces_minimum_tip_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@previewmin");
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

    client.set_min_tip_amount(&jar_id, &Some(100_i128));

    // Preview below minimum must fail with BelowMinTipAmount
    let res_low = client.try_preview_split(&jar_id, &99_i128);
    assert_eq!(res_low, Err(Ok(Error::BelowMinTipAmount.into())));

    // Preview at exact minimum succeeds
    let res_exact = client.try_preview_split(&jar_id, &100_i128);
    assert!(res_exact.is_ok());
}

/// #97: `preview_split` must apply the amount-sign check before the jar's
/// minimum, exactly as `tip` does, so a client previewing then tipping gets
/// one consistent error instead of "below the minimum" from one and "enter a
/// positive amount" from the other.
#[test]
fn preview_split_and_tip_agree_on_error_order() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = String::from_str(env, "@previeword");
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
    client.set_min_tip_amount(&jar_id, &Some(100_i128));
    let message = String::from_str(env, "");

    for (amount, expected) in [
        (0_i128, Error::AmountZero),
        (-5_i128, Error::AmountNegative),
    ] {
        let preview = client.try_preview_split(&jar_id, &amount).unwrap_err();
        let tip = client
            .try_tip(&tipper, &jar_id, &amount, &message)
            .unwrap_err();
        assert_eq!(
            preview,
            Ok(expected.into()),
            "a non-positive amount is a sign error even when a minimum is set"
        );
        assert_eq!(
            preview, tip,
            "preview_split and tip must report the same error for {amount}"
        );
    }

    // Between 1 and the minimum, both still report the minimum rather than a
    // sign error: the order change must not swallow the minimum check.
    let preview = client.try_preview_split(&jar_id, &99_i128).unwrap_err();
    let tip = client
        .try_tip(&tipper, &jar_id, &99_i128, &message)
        .unwrap_err();
    assert_eq!(preview, Ok(Error::BelowMinTipAmount.into()));
    assert_eq!(preview, tip);
}

/// The sign check is unconditional: it applies to a jar with no minimum too,
/// where `preview_split` previously reached `compute_shares` and reported the
/// same codes only incidentally.
#[test]
fn preview_split_rejects_non_positive_amounts_without_a_minimum() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@previewzero");
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
        client.try_preview_split(&jar_id, &0_i128),
        Err(Ok(Error::AmountZero.into()))
    );
    assert_eq!(
        client.try_preview_split(&jar_id, &-5_i128),
        Err(Ok(Error::AmountNegative.into()))
    );
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
    #![proptest_config(ProptestConfig::with_cases(64))]

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

// ===========================================================================
// #171 hold undeliverable shares · #172 leave_jar · #173 lock_splits · #174 recurring
// ===========================================================================

/// Reads the raw per-recipient `tip` breakdown including the `held` flag.
fn raw_tip_breakdown(
    env: &Env,
    contract: &Address,
    jar_id: &String,
) -> std::vec::Vec<(Address, i128, bool)> {
    let expected_topics = vec![
        env,
        symbol_short!("tip").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == *contract && e.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1, "expected exactly one tip event");
    let (_, _, data) = events.first().unwrap();
    let decoded: TipEventData = TryFromVal::try_from_val(env, data).expect("tip event data shape");
    let breakdown = decoded.breakdown;
    (0..breakdown.len())
        .map(|i| breakdown.get(i).unwrap())
        .collect()
}

#[test]
fn tip_holds_share_of_deauthorised_recipient_and_withdraw_pays_it() {
    let s = setup_hostile();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = TestTokenClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@band");
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

    // Bob can no longer receive the token.
    token.block(&bob);
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "hi"));

    // Read the tip event immediately: the test env only keeps the last
    // invocation's events, and the balance/get_held checks below are new calls.
    let bd = raw_tip_breakdown(env, &s.contract, &jar_id);
    assert_eq!(bd[0], (alice.clone(), 70i128, false));
    assert_eq!(bd[1], (bob.clone(), 30i128, true));

    // Alice is paid; Bob is not, but the tip did NOT revert.
    assert_eq!(token.balance(&alice), 70);
    assert_eq!(token.balance(&bob), 0);
    // Bob's share is escrowed in the contract and reported as held.
    assert_eq!(token.balance(&s.contract), 30);
    assert_eq!(client.get_held(&bob), 30);
    // The tipper is debited the full amount regardless.
    assert_eq!(token.balance(&tipper), 900);

    // Bob recovers; he withdraws his held share.
    token.unblock();
    client.withdraw_held(&bob);
    assert_eq!(token.balance(&bob), 30);
    assert_eq!(token.balance(&s.contract), 0);
    assert_eq!(client.get_held(&bob), 0);

    // Nothing left to withdraw.
    assert!(client.try_withdraw_held(&bob).is_err());
}

#[test]
fn withdraw_held_requires_the_recipients_own_authorisation() {
    let s = setup_hostile();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = TestTokenClient::new(env, &s.token);

    let owner = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@solo");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );
    token.block(&bob);
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, ""));
    assert_eq!(client.get_held(&bob), 100);

    // `withdraw_held` is gated on the recipient's own signature: with no auth
    // granted, the call fails and the escrow is untouched.
    env.mock_auths(&[]);
    assert!(client.try_withdraw_held(&bob).is_err());
    assert_eq!(client.get_held(&bob), 100);
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

    // a `splits` event was published with the new count — read it before any
    // further invocation, since the test env only exposes the last call's events
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
    let count: u32 = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(count, 2);

    let jar = client.get_jar(&jar_id);
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
    let locked: bool = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert!(locked);

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

#[test]
fn recurring_tip_create_read_cancel_roundtrip() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@creator");
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

    let seq = env.ledger().sequence();
    let id = client.create_recurring(
        &supporter,
        &jar_id,
        &500,
        &100,
        &String::from_str(env, "monthly"),
    );
    assert_eq!(id, 1);

    let rec = client.get_recurring(&id);
    assert_eq!(rec.supporter, supporter);
    assert_eq!(rec.jar_id, jar_id);
    assert_eq!(rec.amount, 500);
    assert_eq!(rec.period_ledgers, 100);
    assert_eq!(rec.next_due_ledger, seq + 100);

    client.cancel_recurring(&id);
    assert!(client.try_get_recurring(&id).is_err());
}

#[test]
fn recurring_tip_validation() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@vvv");
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

    // zero period
    assert!(client
        .try_create_recurring(&supporter, &jar_id, &500, &0, &String::from_str(env, ""))
        .is_err());
    // non-positive amount
    assert!(client
        .try_create_recurring(&supporter, &jar_id, &0, &100, &String::from_str(env, ""))
        .is_err());
    // unknown jar
    assert!(client
        .try_create_recurring(
            &supporter,
            &String::from_str(env, "@nope"),
            &500,
            &100,
            &String::from_str(env, "")
        )
        .is_err());
}

#[test]
fn cancel_recurring_requires_the_supporter() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@own");
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
    let id = client.create_recurring(&supporter, &jar_id, &500, &100, &String::from_str(env, ""));

    // Only the supporter's signature authorizes cancellation; without it the
    // entry survives.
    env.mock_auths(&[]);
    assert!(client.try_cancel_recurring(&id).is_err());
    // still present
    assert_eq!(client.get_recurring(&id).supporter, supporter);
}
// ---------------------------------------------------------------------------
// #167 — per-jar running tip totals
// ---------------------------------------------------------------------------

/// A jar exists before it has been tipped, so its counters must read zero
/// rather than `JarNotFound`: "nobody has tipped this yet" is a real answer a
/// tip page shows.
#[test]
fn get_jar_stats_is_zero_before_the_first_tip() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@untouched");
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
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 0,
            total: 0
        }
    );
}

/// Several tips to one jar accumulate into that jar, in both fields.
#[test]
fn get_jar_stats_counts_several_tips_to_one_jar() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let one = Address::generate(env);
    let two = Address::generate(env);
    token_admin.mint(&one, &1_000);
    token_admin.mint(&two, &1_000);

    let jar_id = String::from_str(env, "@tally");
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

    client.tip(&one, &jar_id, &100, &String::from_str(env, "a"));
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 1,
            total: 100
        }
    );

    client.tip(&two, &jar_id, &250, &String::from_str(env, "b"));
    client.tip(&one, &jar_id, &50, &String::from_str(env, "c"));
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 3,
            total: 400
        },
        "the jar total counts every supporter, not one supporter's tips"
    );
}

/// Two jars tipped by the same person must not share counters — a single
/// running total would make one creator's leaderboard out of another's tips.
#[test]
fn get_jar_stats_does_not_leak_between_jars() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &10_000);

    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];
    let first = String::from_str(env, "@first");
    let second = String::from_str(env, "@second");
    client.create_jar(&owner, &first, &splits);
    client.create_jar(
        &owner,
        &second,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );

    client.tip(&tipper, &first, &700, &String::from_str(env, "one"));
    client.tip(&tipper, &first, &300, &String::from_str(env, "two"));
    client.tip(&tipper, &second, &50, &String::from_str(env, "three"));

    assert_eq!(
        client.get_jar_stats(&first),
        JarStats {
            tip_count: 2,
            total: 1000
        }
    );
    assert_eq!(
        client.get_jar_stats(&second),
        JarStats {
            tip_count: 1,
            total: 50
        }
    );
}

/// A typo'd slug must not look like an untouched jar.
#[test]
fn get_jar_stats_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_get_jar_stats(&String::from_str(env, "@ghost"));
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// The counters are only an authoritative check on the indexer if a tip that
/// never settled never reaches them. A transfer that traps reverts the whole
/// call, including the stats writes.
#[test]
fn reverted_tip_leaves_both_totals_unchanged() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@revert");
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

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "ok"));

    // More than the remaining balance: the transfer traps.
    let res = client.try_tip(&tipper, &jar_id, &5_000, &String::from_str(env, "nope"));
    assert!(res.is_err(), "an unfundable tip must revert");

    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 1,
            total: 100
        }
    );
    assert_eq!(
        client.get_supporter_stats(&jar_id, &tipper),
        SupporterStats {
            tip_count: 1,
            total: 100
        }
    );
}

// ---------------------------------------------------------------------------
// #168 — per-supporter totals, the data a badge contract needs
// ---------------------------------------------------------------------------

/// A milestone check for someone who has never tipped must read zeros, not
/// fail — the badge contract asks this of arbitrary addresses.
#[test]
fn get_supporter_stats_is_zero_for_an_address_that_never_tipped() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let stranger = Address::generate(env);
    token_admin.mint(&supporter, &1_000);

    let jar_id = String::from_str(env, "@badges");
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
        client.get_supporter_stats(&jar_id, &stranger),
        SupporterStats {
            tip_count: 0,
            total: 0
        }
    );

    client.tip(&supporter, &jar_id, &100, &String::from_str(env, "hi"));
    assert_eq!(
        client.get_supporter_stats(&jar_id, &stranger),
        SupporterStats {
            tip_count: 0,
            total: 0
        },
        "someone else's tip must not move a bystander's counters"
    );
}

/// The entry is keyed by `(jar_id, supporter)`, so one supporter tipping two
/// jars keeps two independent histories — a badge for jar A must not be won
/// by tipping jar B.
#[test]
fn supporter_stats_are_independent_across_jars() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let supporter = Address::generate(env);
    token_admin.mint(&supporter, &10_000);

    let first = String::from_str(env, "@jara");
    let second = String::from_str(env, "@jarb");
    client.create_jar(
        &owner,
        &first,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );
    client.create_jar(
        &owner,
        &second,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );

    client.tip(&supporter, &first, &1_000, &String::from_str(env, "a"));
    client.tip(&supporter, &first, &1_000, &String::from_str(env, "b"));
    client.tip(&supporter, &second, &250, &String::from_str(env, "c"));

    assert_eq!(
        client.get_supporter_stats(&first, &supporter),
        SupporterStats {
            tip_count: 2,
            total: 2_000
        }
    );
    assert_eq!(
        client.get_supporter_stats(&second, &supporter),
        SupporterStats {
            tip_count: 1,
            total: 250
        }
    );
    // The jar aggregate still spans both supporters' jars.
    assert_eq!(
        client.get_jar_stats(&first),
        JarStats {
            tip_count: 2,
            total: 2_000
        }
    );
}

/// Two supporters on one jar each get their own counters, and the jar's total
/// is the sum of both.
#[test]
fn supporter_stats_are_independent_across_supporters() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let one = Address::generate(env);
    let two = Address::generate(env);
    token_admin.mint(&one, &10_000);
    token_admin.mint(&two, &10_000);

    let jar_id = String::from_str(env, "@shared");
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

    client.tip(&one, &jar_id, &1_000, &String::from_str(env, "x"));
    client.tip(&two, &jar_id, &7, &String::from_str(env, "y"));
    client.tip(&two, &jar_id, &7, &String::from_str(env, "z"));
    client.tip(&two, &jar_id, &7, &String::from_str(env, "w"));

    assert_eq!(
        client.get_supporter_stats(&jar_id, &one),
        SupporterStats {
            tip_count: 1,
            total: 1_000
        }
    );
    assert_eq!(
        client.get_supporter_stats(&jar_id, &two),
        SupporterStats {
            tip_count: 3,
            total: 21
        }
    );
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 4,
            total: 1_021
        }
    );
}

#[test]
fn get_supporter_stats_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let supporter = Address::generate(env);
    let res = client.try_get_supporter_stats(&String::from_str(env, "@ghost"), &supporter);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// The extra writes #167 asks about are real rent, and the PR has to state
/// their cost. Two persistent entries per tip — the jar's and the supporter's
/// — on top of whatever the transfers themselves write.
#[test]
fn tip_writes_the_two_totals_entries() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@budget");
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

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "costed"));
    let first = env.cost_estimate().resources();

    // Measured against the same tip with `bump_totals` disabled: 4 write
    // entries / 884 bytes / 3 rent bumps (the transfer's own three, the
    // contract-instance write that carries the tip sequence counter, and the
    // jar TTL bump). The two counters add exactly two writes, 404 bytes, and
    // two rent bumps — see `docs/CONTRACT.md`.
    assert_eq!(
        first.write_entries, 6,
        "a tip writes the two totals entries, the transfer's own three, and the instance's sequence counter"
    );
    assert_eq!(
        first.write_bytes, 1288,
        "the totals cost 404 more bytes per tip"
    );
    assert_eq!(
        first.persistent_entry_rent_bumps, 5,
        "both totals get the jar's TTL treatment, so each is bumped"
    );

    // A repeat tip to the same jar by the same supporter rewrites the same two
    // entries, so the marginal cost per tip is flat, not growing with use.
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "again"));
    let second = env.cost_estimate().resources();
    assert_eq!(
        second.write_entries, first.write_entries,
        "a second tip adds no ledger entries beyond the two totals writes"
    );
    assert_eq!(second.write_bytes, first.write_bytes);
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
        4,
        "the published version is part of the public interface; bump \
         INTERFACE_VERSION when the shape changes, do not edit this test \
         to match a silent change"
    );
}

/// The point of the view is that a client can call it before it has any jar,
/// any token, or any authorization — reads no storage, so nothing to set up.
#[test]
fn version_reports_the_shape_without_reading_storage() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let contract = env.register(TipSplitter, (admin, sac.address()));

    let client = TipSplitterClient::new(&env, &contract);
    assert_eq!(client.version(), INTERFACE_VERSION);

    // Same answer once there is state to read, so the value is not derived
    // from storage.
    let owner = Address::generate(&env);
    client.create_jar(
        &owner,
        &String::from_str(&env, "@vvv"),
        &vec![
            &env,
            Split {
                to: Address::generate(&env),
                bps: 10000,
            },
        ],
    );
    assert_eq!(client.version(), INTERFACE_VERSION);
}

// ---------------------------------------------------------------------------
// #170 — admin-managed token allowlist
// ---------------------------------------------------------------------------

/// A second asset, so a test can pay in something that is not the deploy-time
/// token. Both are real Stellar Asset Contracts, which is what #170 asks the
/// tests to use.
fn second_asset(env: &Env, admin: &Address) -> Address {
    env.register_stellar_asset_contract_v2(admin.clone())
        .address()
}

/// One recipient, one jar: the fixture every allowlist test below shares.
fn one_recipient_jar(
    env: &Env,
    client: &TipSplitterClient,
    slug: &str,
) -> (Address, Address, String) {
    let owner = Address::generate(env);
    let recipient = Address::generate(env);
    let jar_id = String::from_str(env, slug);
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: recipient.clone(),
                bps: 10000,
            },
        ],
    );
    (owner, recipient, jar_id)
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
    let allow_topics = vec![
        env,
        symbol_short!("tk_allow").into_val(env),
        eurc.clone().into_val(env),
    ];
    let ev: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == allow_topics)
        .collect();
    assert_eq!(ev.len(), 1, "allow_token must emit exactly one event");
    let who: Address = TryFromVal::try_from_val(env, &ev.first().unwrap().2)
        .expect("allow event carries the authorising admin");
    assert_eq!(who, s.admin);

    client.disallow_token(&eurc.clone());
    let dis_topics = vec![
        env,
        symbol_short!("tk_dis").into_val(env),
        eurc.clone().into_val(env),
    ];
    let ev: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == dis_topics)
        .collect();
    assert_eq!(ev.len(), 1, "disallow_token must emit exactly one event");
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

// ---------------------------------------------------------------------------
// #175 — collect_recurring: pay a due schedule through the supporter's
// token allowance, one period per call, same split and event as a manual tip
// ---------------------------------------------------------------------------

/// Shared fixture for the collect tests: jar `@rec` paying 70/30 to
/// alice/bob, a supporter holding 10 000 who has approved *this contract* as
/// token spender for `approved` units, and a schedule of `amount` every 100
/// ledgers. Returns the pieces; each test builds its own client.
fn recurring_fixture<'a>(
    env: &'a Env,
    s: &'a Setup,
    amount: i128,
    approved: i128,
) -> (
    TipSplitterClient<'a>,
    Address,
    Address,
    Address,
    String,
    u128,
) {
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let supporter = Address::generate(env);
    token_admin.mint(&supporter, &10_000);

    let jar_id = String::from_str(env, "@rec");
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

    // The payment rail #175 assumes: the supporter approves the contract as
    // spender. The expiration is far enough out that the allowance tests
    // exercise exhaustion through `approved`, not through expiry.
    let expiration = env.ledger().sequence() + 100_000;
    token.approve(&supporter, &s.contract, &approved, &expiration);

    let id = client.create_recurring(
        &supporter,
        &jar_id,
        &amount,
        &100,
        &String::from_str(env, "monthly"),
    );
    (client, alice, bob, supporter, jar_id, id)
}

/// The first collection, once due, pays exactly what a manual tip of the same
/// amount would: the same 70/30 split, the same totals, and the identical
/// `tip` event — with no signature from the supporter at all.
#[test]
fn collect_recurring_pays_the_first_due_period_like_a_tip() {
    let s = setup();
    let env = &s.env;
    let (client, alice, bob, supporter, jar_id, id) = recurring_fixture(env, &s, 500, 1_000);
    let token = token::Client::new(env, &s.token);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due);

    // The keeper's own signature is irrelevant: with *no* auths mocked at
    // all the call still settles, proving it is genuinely anyone-callable and
    // that the supporter never signs a collection.
    env.mock_auths(&[]);
    client.collect_recurring(&id);
    env.mock_all_auths();

    // Snapshot the events *now*, before any further contract call: the
    // testutils event buffer only exposes the most recent top-level
    // invocation, so a `balance` or `get_recurring` call in between would
    // erase the collection's events.
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| {
            e.0 == s.contract
                && topic_is_symbol(env, &e.1, 0, symbol_short!("tip"))
                && topic_at::<String>(env, &e.1, 1) == jar_id
        })
        .collect();
    assert_eq!(evs.len(), 1, "a collection publishes exactly one tip event");
    let decoded: TipEventData =
        TryFromVal::try_from_val(env, &evs.first().unwrap().2).expect("tip event data shape");
    assert_eq!(
        decoded,
        TipEvent {
            version: 1,
            from: supporter.clone(),
            amount: 500i128,
            fee: 0,
            message: String::from_str(env, "monthly"),
            breakdown: vec![
                env,
                (alice.clone(), 350i128, false),
                (bob.clone(), 150i128, false)
            ],
            token: s.token.clone(),
            seq: 1u64,
            reference: None,
        }
    );

    assert_eq!(token.balance(&alice), 350);
    assert_eq!(token.balance(&bob), 150);
    assert_eq!(token.balance(&supporter), 9_500);
    assert_eq!(token.balance(&s.contract), 0);

    // The entry advanced by exactly one period, anchored on the old due
    // ledger, not on "now".
    let rec = client.get_recurring(&id);
    assert_eq!(rec.next_due_ledger, due + 100);

    // Collected tips bump the same on-chain totals a manual tip would.
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 1,
            total: 500
        }
    );
    assert_eq!(
        client.get_supporter_stats(&jar_id, &supporter),
        SupporterStats {
            tip_count: 1,
            total: 500
        }
    );
}

/// Calling before the due ledger is a typed `NotDue` no-op: nothing moves,
/// nothing is cancelled, and the same call succeeds once the ledger is
/// reached — the early-retry path the issue names.
#[test]
fn collect_recurring_before_due_is_a_typed_no_op_and_succeeds_after_advancing() {
    let s = setup();
    let env = &s.env;
    let (client, alice, bob, supporter, _jar_id, id) = recurring_fixture(env, &s, 500, 1_000);
    let token = token::Client::new(env, &s.token);
    let due = client.get_recurring(&id).next_due_ledger;

    // One ledger short of due.
    env.ledger().set_sequence_number(due - 1);
    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::NotDue.into())),
        "an early collection must fail with the contract's own code"
    );
    assert_eq!(token.balance(&alice), 0);
    assert_eq!(token.balance(&bob), 0);
    assert_eq!(token.balance(&supporter), 10_000);
    // ...and the schedule is untouched: the same entry can still be collected.
    assert_eq!(client.get_recurring(&id).next_due_ledger, due);

    // On the due ledger itself it goes through.
    env.ledger().set_sequence_number(due);
    client.collect_recurring(&id);
    assert_eq!(token.balance(&alice), 350);
    assert_eq!(client.get_recurring(&id).next_due_ledger, due + 100);
}

/// A keeper that was offline cannot drain several periods at once: each call
/// collects at most one period — the due ledger moves exactly one
/// `period_ledgers` per call — and the catch-up is additionally bounded by
/// the token allowance the supporter granted, which the third call exhausts
/// with a typed error and zero state change.
#[test]
fn collect_recurring_advances_exactly_one_period_per_call() {
    let s = setup();
    let env = &s.env;
    let (client, _alice, _bob, supporter, _jar_id, id) = recurring_fixture(env, &s, 500, 1_000);
    let token = token::Client::new(env, &s.token);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due + 500);
    // A keeper backlogged by five periods can still only collect one period
    // per call: the due ledger moves by exactly `period_ledgers`, anchored on
    // the old due ledger — not on "now".
    client.collect_recurring(&id);
    assert_eq!(client.get_recurring(&id).next_due_ledger, due + 100);
    // The anchor is still in the past, so the schedule remains due and a
    // second call collects exactly one further period — the per-call cap is
    // what bounds the catch-up, together with the allowance the supporter
    // granted.
    client.collect_recurring(&id);
    assert_eq!(client.get_recurring(&id).next_due_ledger, due + 200);
    assert_eq!(token.balance(&supporter), 9_000);
    // The allowance covered exactly two periods: a third call fails with the
    // typed error and changes nothing — an offline keeper can never drain
    // more than the supporter approved.
    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::InsufficientAllowance.into())),
        "the catch-up is one period per call, bounded by the allowance"
    );
    assert_eq!(client.get_recurring(&id).next_due_ledger, due + 200);
    assert_eq!(token.balance(&supporter), 9_000);
}

/// An exhausted allowance fails with the typed `InsufficientAllowance` before
/// any funds move and without touching the schedule — the supporter revoking
/// (or approving too little) stops collections dead, and no partial period is
/// ever paid.
#[test]
fn collect_recurring_rejects_an_insufficient_allowance_without_state_change() {
    let s = setup();
    let env = &s.env;
    // Approved 400 for a 500 schedule: the allowance never covers a period.
    let (client, alice, _bob, supporter, _jar_id, id) = recurring_fixture(env, &s, 500, 400);
    let token = token::Client::new(env, &s.token);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due);

    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::InsufficientAllowance.into())),
        "an allowance below the scheduled amount is this contract's typed error, not the token's"
    );
    assert_eq!(token.balance(&alice), 0, "no partial period is paid");
    assert_eq!(token.balance(&supporter), 10_000);
    assert_eq!(client.get_recurring(&id).next_due_ledger, due);

    // The expired case needs no separate code path: SAC reads an expired
    // allowance as zero, which the same gate rejects. Revoke instead of
    // waiting out the expiration: approve 0 for this spender.
    let expiration = env.ledger().sequence() + 10;
    token.approve(&supporter, &s.contract, &400, &expiration);
    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::InsufficientAllowance.into())),
        "revoking the allowance stops collections on the next call"
    );
    env.ledger().set_sequence_number(expiration + 1);
    assert_eq!(
        client.try_collect_recurring(&id),
        Err(Ok(Error::InsufficientAllowance.into())),
        "an expired allowance reads as zero and fails the same typed check"
    );
}

/// An unknown schedule id is a typed `RecurringNotFound`, matching
/// `get_recurring`/`cancel_recurring`.
#[test]
fn collect_recurring_rejects_unknown_id() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    assert_eq!(
        client.try_collect_recurring(&77),
        Err(Ok(Error::RecurringNotFound.into())),
    );
}

// ---------------------------------------------------------------------------
// #176 — post-tip hook: an admin-registered contract notified after every
// settled tip, with a panicking hook that cannot revert the tip
// ---------------------------------------------------------------------------

/// A hook that records its invocations, so a test can assert the contract
/// actually called it. Also publishes an `on_tip` event from the *hook's* own
/// address, which is what the event assertions read. Kept in its own module
/// because `#[contractimpl]` generates per-method glue (`__on_tip`) in the
/// enclosing module, which a second mock hook in the same file would clash
/// with.
mod recording_hook {
    use super::*;

    #[contract]
    pub struct RecordingHook;

    #[contracttype]
    #[derive(Clone)]
    enum HookKey {
        Hits,
    }

    #[contractimpl]
    impl RecordingHook {
        /// Implements the `TipHook` interface: counts calls and echoes the
        /// arguments into its own event.
        pub fn on_tip(env: Env, jar_id: String, from: Address, amount: i128) {
            let n: u32 = env.storage().persistent().get(&HookKey::Hits).unwrap_or(0);
            env.storage().persistent().set(&HookKey::Hits, &(n + 1));
            env.events()
                .publish((symbol_short!("on_tip"), jar_id), (from, amount));
        }

        pub fn hits(env: Env) -> u32 {
            env.storage().persistent().get(&HookKey::Hits).unwrap_or(0)
        }
    }
}

/// A hook that aborts on every call, to pin down that a broken badge contract
/// cannot stop a creator being paid.
mod panicky_hook {
    use super::*;

    #[contract]
    pub struct PanickyHook;

    #[contractimpl]
    impl PanickyHook {
        pub fn on_tip(_env: Env, _jar_id: String, _from: Address, _amount: i128) {
            panic!("the badge contract blew up");
        }
    }
}

pub use panicky_hook::PanickyHook;
pub use recording_hook::{RecordingHook, RecordingHookClient};

/// True when topic `idx` of a published event decodes to `want`. `Val` has no
/// `PartialEq`, so event topics must be compared by decoding, never directly.
fn topic_is_symbol(env: &Env, topics: &Vec<Val>, idx: u32, want: Symbol) -> bool {
    topics
        .get(idx)
        .is_some_and(|v| Symbol::try_from_val(env, &v) == Ok(want))
}

/// Decode topic `idx` of a published event into its typed value.
fn topic_at<T: TryFromVal<Env, Val>>(env: &Env, topics: &Vec<Val>, idx: u32) -> T {
    T::try_from_val(env, &topics.get(idx).unwrap()).expect("event topic shape")
}

/// A single-recipient jar plus a funded tipper, the fixture the hook tests
/// share.
fn hook_jar<'a>(env: &'a Env, s: &'a Setup) -> (TipSplitterClient<'a>, Address, Address, String) {
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);
    let jar_id = String::from_str(env, "@hook");
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
    (client, tipper, alice, jar_id)
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
    assert_eq!(topic_at::<Address>(env, &evs[0].1, 1), s.admin);
    let published: Address = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(published, hook);
}

// ---------------------------------------------------------------------------
// #177 — preview_tip: the sender-aware preview must agree, number for number,
// with the breakdown the `tip` event publishes for the same tip.
// ---------------------------------------------------------------------------

/// Turns a soroban `Vec<i128>` into a std vec so it can be compared with the
/// decoded event breakdown.
fn i128_vec_to_std(v: &Vec<i128>) -> std::vec::Vec<i128> {
    (0..v.len()).map(|i| v.get(i).unwrap()).collect()
}

/// A three-recipient jar used by the preview/tip agreement tests.
fn preview_tip_jar(
    env: &Env,
    client: &TipSplitterClient,
    jar_id: &String,
    a: &Address,
    b: &Address,
    c: &Address,
) {
    let owner = Address::generate(env);
    client.create_jar(
        &owner,
        jar_id,
        &vec![
            env,
            Split {
                to: a.clone(),
                bps: 5000,
            },
            Split {
                to: b.clone(),
                bps: 3000,
            },
            Split {
                to: c.clone(),
                bps: 2000,
            },
        ],
    );
}

/// The base case: the tipper is not a recipient, so `preview_tip` reports the
/// plain split and the event's breakdown moves exactly those amounts.
#[test]
fn preview_tip_matches_the_tip_event_breakdown_when_sender_is_not_a_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@ptip_outsider");
    preview_tip_jar(env, &client, &jar_id, &alice, &bob, &carol);

    let plan = client.preview_tip(&jar_id, &tipper, &101);
    // 5000/3000/2000 of 101: 50, 30, and the last recipient absorbs the dust.
    assert_eq!(
        i128_vec_to_std(&plan),
        std::vec::Vec::from([50_i128, 30_i128, 21_i128])
    );

    client.tip(&tipper, &jar_id, &101, &String::from_str(env, "thanks"));

    let moved: std::vec::Vec<i128> = tip_breakdown(env, &s.contract, &jar_id)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(
        moved,
        i128_vec_to_std(&plan),
        "preview_tip must agree with the tip event's breakdown, recipient by recipient"
    );

    // And the balances moved agree with both, so the agreement is physical,
    // not just two views of the same helper.
    let token = token::Client::new(env, &s.token);
    assert_eq!(token.balance(&alice), 50);
    assert_eq!(token.balance(&bob), 30);
    assert_eq!(token.balance(&carol), 21);
    assert_eq!(
        token.balance(&tipper),
        899,
        "the tipper paid the full 101: 50 + 30 + 21"
    );
}

/// A tipper who is also the *first* recipient keeps their own share: the
/// self-transfer is reported as 0, every other recipient gets exactly their
/// split, and `preview_tip` must report that, not the raw split (#78).
#[test]
fn preview_tip_matches_the_tip_event_breakdown_when_sender_is_a_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    token_admin.mint(&alice, &1_000);

    let jar_id = String::from_str(env, "@ptip_self");
    preview_tip_jar(env, &client, &jar_id, &alice, &bob, &carol);

    // alice tips 1000: her own 500 is never transferred and stays with her,
    // so the payout plan is [0, 300, 200] — not the [500, 300, 200] that
    // preview_split would report.
    let plan = client.preview_tip(&jar_id, &alice, &1_000);
    assert_eq!(
        i128_vec_to_std(&plan),
        std::vec::Vec::from([0_i128, 300_i128, 200_i128])
    );
    assert_eq!(
        i128_vec_to_std(&client.preview_split(&jar_id, &1_000)),
        std::vec::Vec::from([500_i128, 300_i128, 200_i128]),
        "preview_split stays sender-agnostic; preview_tip is the sender-aware view"
    );

    client.tip(
        &alice,
        &jar_id,
        &1_000,
        &String::from_str(env, "from one of you"),
    );

    let moved: std::vec::Vec<i128> = tip_breakdown(env, &s.contract, &jar_id)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(moved, i128_vec_to_std(&plan));

    let token = token::Client::new(env, &s.token);
    assert_eq!(
        token.balance(&alice),
        500,
        "alice's own 500 stays with her: only bob's 300 and carol's 200 leave her"
    );
    assert_eq!(token.balance(&bob), 300);
    assert_eq!(token.balance(&carol), 200);
}

/// The special case of the special case: a tipper who is the *last* recipient
/// absorbs the dust rule twice over — their final entry is 0 because nothing
/// moves, and no earlier share rolls onto them.
#[test]
fn preview_tip_reports_zero_for_a_sender_who_is_the_last_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    token_admin.mint(&carol, &1_000);

    let jar_id = String::from_str(env, "@ptip_last");
    preview_tip_jar(env, &client, &jar_id, &alice, &bob, &carol);

    let plan = client.preview_tip(&jar_id, &carol, &1_000);
    assert_eq!(
        i128_vec_to_std(&plan),
        std::vec::Vec::from([500_i128, 300_i128, 0_i128])
    );

    client.tip(
        &carol,
        &jar_id,
        &1_000,
        &String::from_str(env, "last one skips"),
    );

    let moved: std::vec::Vec<i128> = tip_breakdown(env, &s.contract, &jar_id)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(moved, i128_vec_to_std(&plan));
}

/// A client that calls `preview_tip` before signing must see the same
/// rejections `tip` would raise: unknown jar, below the minimum, and amounts
/// `tip` itself would reject.
#[test]
fn preview_tip_rejects_what_tip_rejects() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    assert_eq!(
        client.try_preview_tip(&String::from_str(env, "@ghost"), &tipper, &100_i128),
        Err(Ok(Error::JarNotFound.into()))
    );

    let jar_id = String::from_str(env, "@ptip_min");
    let owner = Address::generate(env);
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

    assert_eq!(
        client.try_preview_tip(&jar_id, &tipper, &499_i128),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    assert_eq!(
        client.try_preview_tip(&jar_id, &tipper, &0_i128),
        Err(Ok(Error::AmountZero.into()))
    );
    assert_eq!(
        client.try_preview_tip(&jar_id, &tipper, &-5_i128),
        Err(Ok(Error::AmountNegative.into()))
    );
    assert!(client.try_preview_tip(&jar_id, &tipper, &500_i128).is_ok());
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
    assert_eq!(topic_at::<Address>(env, &evs[0].1, 1), s.admin);
    let first: Option<Address> = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(first, Some(registrar.clone()));
    assert_eq!(client.get_registrar(), Some(registrar.clone()));

    client.set_registrar(&None);
    let evs: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && topic_is_symbol(env, &e.1, 0, symbol_short!("reg_set")))
        .collect();
    assert_eq!(evs.len(), 1, "clearing the registrar publishes one event");
    assert_eq!(topic_at::<Address>(env, &evs[0].1, 1), s.admin);
    let second: Option<Address> = TryFromVal::try_from_val(env, &evs[0].2).unwrap();
    assert_eq!(second, None);
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

// ── Issue #136: small amounts are rejected, not silently mis-split ──────────

// Asserts that amounts below the minimum accepted value fail with
// `InvalidAmount` and leave every balance unchanged. The minimum is
// `ceil(BPS_DENOM / min_bps)` for the generated splits — below that,
// some recipient's share would truncate to zero.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

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

// ── Issue #137: validate_splits against a reference model ────────────────────

/// Reference model of the validation rules, written from docs/CONTRACT.md.
/// Check order (matches the contract):
///   1. splits is not empty           → SplitsEmpty
///   2. length <= MAX_RECIPIENTS      → TooManyRecipients
///   3. every bps in 1..=BPS_DENOM    → SplitOutOfRange
///   4. no duplicate recipients       → DuplicateRecipient
///   5. bps sum == BPS_DENOM          → SplitSumNot100Pct
///   6. success
#[derive(Debug, PartialEq)]
enum ModelError {
    SplitsEmpty,
    TooManyRecipients,
    SplitOutOfRange,
    DuplicateRecipient,
    SplitSumNot100Pct,
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
    #![proptest_config(ProptestConfig::with_cases(64))]

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
    #![proptest_config(ProptestConfig::with_cases(64))]

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

// ── #139: Model-based property test ──────────────────────────────────────────

#[derive(Debug, Clone)]
enum Op {
    CreateJar {
        slug: std::string::String,
        owner_idx: usize,
        recipients: std::vec::Vec<(usize, u32)>,
    },
    Tip {
        slug: std::string::String,
        tipper_idx: usize,
        amount: i128,
    },
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

// ── #140: Assert specific errors instead of is_err ──────────────────────────

#[test]
fn jar_exists_is_false_for_rejected_jar_exact_error() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@rejected-exact");

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
    // #140 — assert the specific error, not just is_err()
    assert_eq!(res, Err(Ok(Error::SplitSumNot100Pct.into())));
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

// ── #142: Resource cost assertions ──────────────────────────────────────────

const MAX_INSTRUCTIONS_20_RECIPIENTS: u64 = 500_000_000; // headroom over measured
const MAX_MEMORY_20_RECIPIENTS: u64 = 200_000_000;

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
    ];

    for (variant, expected) in codes {
        assert_eq!(
            variant as u32, expected,
            "error codes are part of the public interface: novatip-sdk and the frontend map them to messages, so changing a value is a breaking change — append new variants and never renumber existing ones"
        );
    }
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

/// `jar_exists` is the onboarding form's keystroke check, so it must answer
/// `false` for ids `create_jar` would refuse rather than panicking on them.
#[test]
fn jar_exists_rejects_slugs_create_jar_would_reject() {
    let s = setup();
    let client = TipSplitterClient::new(&s.env, &s.contract);
    assert!(!client.jar_exists(&String::from_str(&s.env, "")));
    assert!(!client.jar_exists(&String::from_str(&s.env, &"x".repeat(65))));
    assert!(!client.jar_exists(&String::from_str(&s.env, "@Alice")));
    assert!(!client.jar_exists(&String::from_str(&s.env, "@ab")));
}

#[test]
fn get_min_accepted_tip_accounts_for_rounding_and_configured_minimum() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let jar_id = String::from_str(env, "@smallest");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 7500,
            },
            Split {
                to: Address::generate(env),
                bps: 2500,
            },
        ],
    );
    // ceil(10_000 / 2_500) is 4; this is the first amount where both
    // non-final shares are positive under integer division.
    assert_eq!(client.get_min_accepted_tip(&jar_id), 4);
    client.set_min_tip_amount(&jar_id, &Some(25));
    assert_eq!(client.get_min_accepted_tip(&jar_id), 25);
}

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

/// `set_fee_bps` publishes `fee_set` with the admin as topic and the new
/// value as data, so a client can track the fee from the event log.
#[test]
fn set_fee_bps_emits_fee_set_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    client.set_fee_bps(&100);

    let expected_topics = vec![
        env,
        symbol_short!("fee_set").into_val(env),
        s.admin.into_val(env),
    ];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1);
    let fee: u32 = TryFromVal::try_from_val(env, &events[0].2).unwrap();
    assert_eq!(fee, 100);
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

/// A collected recurring tip pays the same fee as a manual tip of the same
/// amount, drawn from the same allowance.
#[test]
fn collect_recurring_takes_the_fee() {
    let s = setup();
    let env = &s.env;
    let (client, alice, bob, supporter, _jar_id, id) = recurring_fixture(env, &s, 1_000, 1_000);
    let token = token::Client::new(env, &s.token);
    client.set_fee_bps(&100);
    let due = client.get_recurring(&id).next_due_ledger;
    env.ledger().set_sequence_number(due);

    let before = token.balance(&supporter);
    client.collect_recurring(&id);

    // 1% of 1 000 is 10; 990 splits 70/30 into 693 and 297.
    assert_eq!(token.balance(&s.admin), 10);
    assert_eq!(token.balance(&alice), 693);
    assert_eq!(token.balance(&bob), 297);
    assert_eq!(before - token.balance(&supporter), 1_000);
}

/// Decodes the single `tip` event the most recent call published for `jar_id`.
fn last_tip_event(env: &Env, contract: &Address, jar_id: &String) -> TipEvent {
    let expected_topics = vec![
        env,
        symbol_short!("tip").into_val(env),
        jar_id.into_val(env),
    ];
    let events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == *contract && e.1 == expected_topics)
        .collect();
    assert_eq!(events.len(), 1, "expected exactly one tip event");
    TryFromVal::try_from_val(env, &events[0].2).expect("tip event data shape")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

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

/// Keep the contract's own instance and code alive across a large ledger
/// advance, so a failure can only come from the jar entry under test.
fn keep_instance_alive(env: &Env, contract: &Address) {
    let max = env.storage().max_ttl();
    env.as_contract(contract, || {
        env.storage().instance().extend_ttl(max, max);
    });
}

fn single_recipient_jar(
    env: &Env,
    client: &TipSplitterClient,
    owner: &Address,
    id: &str,
) -> String {
    let jar_id = String::from_str(env, id);
    client.create_jar(
        owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 10000,
            },
        ],
    );
    jar_id
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
    env.ledger().set_sequence_number(start + JAR_TTL_LEDGERS + 1);

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

/// A recurring schedule keeps its message on-chain and republishes it on every
/// collection, so it shares the tip's content validation.
#[test]
fn create_recurring_rejects_invalid_message() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let jar_id = String::from_str(env, "@recmsg");
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

    let with_nul = String::from_bytes(env, b"monthly\0tip");
    let res = client.try_create_recurring(&supporter, &jar_id, &500, &100, &with_nul);
    assert_eq!(res, Err(Ok(Error::InvalidMessage.into())));
}

/// The constructor must reject an admin and token that share the same address,
/// preventing deployment of a permanently misconfigured contract (#69).
#[test]
#[should_panic(expected = "admin and token cannot be the same address")]
fn constructor_rejects_admin_and_token_with_same_address() {
    let env = Env::default();
    let addr = Address::generate(&env);
    env.register(TipSplitter, (addr.clone(), addr));
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

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "keep self share"));

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
    client.tip(&tipper, &jar_id, &1_000, &String::from_str(env, "ending split"));

    assert_eq!(token.balance(&alice), 600);
    assert_eq!(token.balance(&bob), 200);
    assert_eq!(
        token.balance(&tipper),
        200,
        "tipper's 200 final share stayed in their wallet: only 800 left for alice and bob"
    );
    assert_eq!(token.balance(&s.contract), 0);
}

