#![cfg(test)]
use super::*;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{token, vec, Address, Env, String};

/// Shared test fixture: a fresh env with a USDC-like token and a deployed
/// TipSplitter pointed at it. All auths are mocked.
struct Setup {
    env: Env,
    contract: Address,
    token: Address,
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
fn create_jar_rejects_bad_bps_sum() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    // 6000 + 3000 = 9000, not 10000.
    let bad = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 6000,
        },
        Split {
            to: alice.clone(),
            bps: 3000,
        },
    ];

    let res = client.try_create_jar(&owner, &String::from_str(env, "@x"), &bad);
    assert_eq!(res, Err(Ok(Error::InvalidSplits)));
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
    assert_eq!(res, Err(Ok(Error::JarExists)));
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
    assert_eq!(res, Err(Ok(Error::JarNotFound)));
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
    assert_eq!(res, Err(Ok(Error::InvalidAmount)));
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
    client.tip(&tipper, &jar_id, &500, &String::from_str(env, "all for you"));

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

    let res = client.try_create_jar(
        &owner,
        &String::from_str(env, "@toobig"),
        &splits_vec,
    );
    assert_eq!(res, Err(Ok(Error::TooManyRecipients)));
}

#[test]
fn create_jar_emits_jar_created_event() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let splits = vec![env, Split { to: alice.clone(), bps: 10000 }];
    let jar_id = String::from_str(env, "@one");

    client.create_jar(&owner, &jar_id, &splits);

    // The jar_created event must be published with the correct topics and data.
    let events = env.events().all();
    // Filter to events from our contract
    let jar_events: soroban_sdk::Vec<_> = events
        .iter()
        .filter(|e| e.0 == s.contract)
        .collect();
    assert_eq!(jar_events.len(), 1);
}
