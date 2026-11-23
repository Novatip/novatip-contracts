//! Wasm smoke test (#143).
//!
//! Exercises the deployed wasm through the client generated from its spec, so
//! a difference between the native build and the artifact actually shipped is
//! caught. Build the artifact first (see CONTRIBUTING.md):
//!
//! ```bash
//! cargo build --target wasm32v1-none --release
//! cargo test --features wasm-test
//! ```

use super::*;
use soroban_sdk::vec;

mod wasm {
    soroban_sdk::contractimport!(file = "../../target/wasm32v1-none/release/tip_splitter.wasm");
}

/// The wasm artifact, its default token and the admin both were deployed with.
struct WasmSetup {
    env: Env,
    contract: Address,
    token: Address,
    admin: Address,
}

impl WasmSetup {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let contract = env.register(wasm::WASM, (admin.clone(), token.clone()));
        Self {
            env,
            contract,
            token,
            admin,
        }
    }

    fn client(&self) -> wasm::Client {
        wasm::Client::new(&self.env, &self.contract)
    }
}

#[test]
fn wasm_create_tip_and_read() {
    let s = WasmSetup::new();
    let env = &s.env;
    let client = s.client();

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token::StellarAssetClient::new(env, &s.token).mint(&tipper, &1_000_000);

    let jar_id = String::from_str(env, "@wasmjar");
    client.create_jar(
        &owner,
        &jar_id,
        &Vec::from_array(
            env,
            [
                wasm::Split {
                    to: alice.clone(),
                    bps: 7_000,
                },
                wasm::Split {
                    to: bob.clone(),
                    bps: 3_000,
                },
            ],
        ),
    );
    client.tip(
        &tipper,
        &jar_id,
        &1_000,
        &String::from_str(env, "great work"),
    );

    // The test env keeps only the last invocation's events, so decode the tip
    // event before the balance and view calls below.
    let expected_topics = vec![
        env,
        symbol_short!("tip").into_val(env),
        jar_id.clone().into_val(env),
    ];
    let tip_events: std::vec::Vec<_> = env
        .events()
        .all()
        .iter()
        .filter(|e| e.0 == s.contract && e.1 == expected_topics)
        .collect();
    assert_eq!(tip_events.len(), 1);
    let event: wasm::TipEvent =
        TryFromVal::try_from_val(env, &tip_events[0].2).expect("tip event data shape");
    assert_eq!(event.version, 1);
    assert_eq!(event.from, tipper);
    assert_eq!(event.amount, 1_000);
    assert_eq!(event.message, String::from_str(env, "great work"));
    assert_eq!(event.breakdown.len(), 2);
    assert_eq!(event.breakdown.get(0).unwrap(), (alice.clone(), 700, false));
    assert_eq!(event.breakdown.get(1).unwrap(), (bob.clone(), 300, false));

    // Balances and state read back through the wasm clients.
    let token = token::Client::new(env, &s.token);
    assert_eq!(token.balance(&alice), 700);
    assert_eq!(token.balance(&bob), 300);
    assert_eq!(token.balance(&tipper), 999_000);
    assert_eq!(client.get_jar_owner(&jar_id), owner);
    assert_eq!(client.get_jar_stats(&jar_id).tip_count, 1);
    assert_eq!(client.get_admin(), s.admin);
}

#[test]
fn wasm_validation_error_splits_empty() {
    let s = WasmSetup::new();
    let env = &s.env;
    let client = s.client();

    let result = client.try_create_jar(
        &Address::generate(env),
        &String::from_str(env, "@empty"),
        &Vec::new(env),
    );
    assert_eq!(result, Err(Ok(wasm::Error::SplitsEmpty.into())));
}

#[test]
fn wasm_auth_failure_not_owner() {
    let s = WasmSetup::new();
    let env = &s.env;
    let client = s.client();
    let jar_id = String::from_str(env, "@authjar");
    let splits = Vec::from_array(
        env,
        [wasm::Split {
            to: Address::generate(env),
            bps: 10_000,
        }],
    );
    client.create_jar(&Address::generate(env), &jar_id, &splits);

    // Only the stranger signs; `update_splits` needs the jar owner's auth.
    let stranger = Address::generate(env);
    env.mock_auths(&[MockAuth {
        address: &stranger,
        invoke: &MockAuthInvoke {
            contract: &s.contract,
            fn_name: "update_splits",
            args: (jar_id.clone(), splits.clone()).into_val(env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_update_splits(&jar_id, &splits).is_err());
}
