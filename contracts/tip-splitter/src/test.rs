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
