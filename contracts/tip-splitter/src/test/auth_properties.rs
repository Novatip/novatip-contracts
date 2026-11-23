//! Exact signer trees for locked jars (#258).
use super::*;
use soroban_sdk::vec;

fn auth_fixture() -> (Setup, Address, Address, Address, Address, String) {
    let s = setup();
    let env = &s.env;
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    let jar = String::from_str(env, "@consent");
    let client = TipSplitterClient::new(env, &s.contract);
    client.create_jar(
        &owner,
        &jar,
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
    client.lock_splits(&jar);
    (s, owner, alice, bob, carol, jar)
}

fn assert_exact_signers(env: &Env, wanted: &[Address]) {
    let got = env.auths();
    assert_eq!(
        got.len(),
        wanted.len(),
        "unexpected require_auth calls: {got:?}"
    );
    for signer in wanted {
        assert_eq!(
            got.iter().filter(|entry| &entry.0 == signer).count(),
            1,
            "missing or repeated signer: {signer:?}"
        );
    }
}

#[test]
fn lock_splits_requires_only_owner() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar = String::from_str(env, "@authlock");
    client.create_jar(
        &owner,
        &jar,
        &vec![
            env,
            Split {
                to: alice,
                bps: 10_000,
            },
        ],
    );
    client.lock_splits(&jar);
    assert_exact_signers(env, &[owner]);
}

#[test]
fn locked_splits_increase_and_new_recipient_require_only_owner_and_decreased_holder() {
    let (s, owner, alice, bob, carol, jar) = auth_fixture();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let newcomer = Address::generate(env);
    // Alice loses share; Bob gains; Carol is unchanged; newcomer joins.
    client.update_splits(
        &jar,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 2500,
            },
            Split {
                to: bob.clone(),
                bps: 3500,
            },
            Split {
                to: carol.clone(),
                bps: 3000,
            },
            Split {
                to: newcomer.clone(),
                bps: 1000,
            },
        ],
    );
    assert_exact_signers(env, &[owner, alice]);
    assert_eq!(client.get_jar(&jar).splits.len(), 4);
    assert!(client.is_splits_locked(&jar));
}

#[test]
fn locked_splits_removal_requires_owner_and_exactly_removed_recipient() {
    let (s, owner, alice, bob, carol, jar) = auth_fixture();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    client.update_splits(
        &jar,
        &vec![
            env,
            Split {
                to: alice,
                bps: 4000,
            },
            Split {
                to: carol,
                bps: 6000,
            },
        ],
    );
    assert_exact_signers(env, &[owner, bob]);
    assert!(client.is_splits_locked(&jar));
}

#[test]
fn locked_splits_two_decreases_require_only_owner_and_both_losing_recipients() {
    let (s, owner, alice, bob, carol, jar) = auth_fixture();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    client.update_splits(
        &jar,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 3000,
            },
            Split {
                to: bob.clone(),
                bps: 2000,
            },
            Split {
                to: carol,
                bps: 5000,
            },
        ],
    );
    assert_exact_signers(env, &[owner, alice, bob]);
}

#[test]
fn unlock_splits_requires_owner_and_every_current_recipient() {
    let (s, owner, alice, bob, carol, jar) = auth_fixture();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    client.unlock_splits(&jar);
    assert_exact_signers(env, &[owner, alice, bob, carol]);
    assert!(!client.is_splits_locked(&jar));
}
