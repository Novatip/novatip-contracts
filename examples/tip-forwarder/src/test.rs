#![cfg(test)]
use super::*;
use soroban_sdk::testutils::{Address as _, MockAuth, MockAuthInvoke};
use soroban_sdk::{token, Address, Env, IntoVal, String};
use tip_splitter::{TipSplitter, TipSplitterClient};

struct Setup {
    env: Env,
    /// Account that may trigger forwarded tips from the forwarder's balance.
    owner: Address,
    /// The splitter's admin, which receives the protocol fee (#79).
    admin: Address,
    token: Address,
    splitter: Address,
    forwarder: Address,
}

/// Fresh env with the real `TipSplitter`, the forwarder, and a test SAC. The
/// forwarder is funded with 1 000 units of the token.
fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let token = sac.address();

    // The real contract under its real interface — registered side by side,
    // so no built wasm is involved anywhere in these tests.
    let splitter = env.register(TipSplitter, (admin.clone(), token.clone()));

    let owner = Address::generate(&env);
    let forwarder = env.register(TipForwarder, (owner.clone(), splitter.clone()));

    token::StellarAssetClient::new(&env, &token).mint(&forwarder, &1_000);

    Setup {
        env,
        owner,
        admin,
        token,
        splitter,
        forwarder,
    }
}

/// Register `jar_id` on the splitter with the given (recipient, bps) pairs.
fn create_jar(s: &Setup, jar_id: &str, splits: &[(Address, u32)]) {
    let client = TipSplitterClient::new(&s.env, &s.splitter);
    let jar_owner = Address::generate(&s.env);
    let vec = {
        let mut v = Vec::new(&s.env);
        for (to, bps) in splits {
            v.push_back(tip_splitter::Split {
                to: to.clone(),
                bps: *bps,
            });
        }
        v
    };
    client.create_jar(&jar_owner, &String::from_str(&s.env, jar_id), &vec);
}

/// Strict-mode authorization: only `owner` signing a call of `fn_name` on the
/// forwarder. The transfers the forwarder then makes must be authorized by
/// the contract itself (`authorize_as_current_contract`), which is what this
/// example is demonstrating.
fn mock_owner_auth(s: &Setup, fn_name: &str, jar_id: &str, amount: i128, message: &String) {
    s.env.mock_auths(&[MockAuth {
        address: &s.owner,
        invoke: &MockAuthInvoke {
            contract: &s.forwarder,
            fn_name,
            args: (String::from_str(&s.env, jar_id), amount, message.clone()).into_val(&s.env),
            sub_invokes: &[],
        },
    }]);
}

#[test]
fn forwarded_tip_pays_the_preview_amounts() {
    let s = setup();
    let env = &s.env;
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    create_jar(&s, "@band", &[(alice.clone(), 7000), (bob.clone(), 3000)]);

    let message = String::from_str(env, "great show");
    mock_owner_auth(&s, "forward_tip", "@band", 100, &message);

    TipForwarderClient::new(env, &s.forwarder).forward_tip(
        &String::from_str(env, "@band"),
        &100,
        &message,
    );

    // 70/30 is both what preview_split returned and what was authorized.
    let token = token::Client::new(env, &s.token);
    assert_eq!(token.balance(&alice), 70);
    assert_eq!(token.balance(&bob), 30);
    assert_eq!(token.balance(&s.forwarder), 900);
}

#[test]
fn naive_authorization_fails_once_a_protocol_fee_is_set() {
    let s = setup();
    let env = &s.env;
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    create_jar(&s, "@band", &[(alice.clone(), 7000), (bob.clone(), 3000)]);
    // 1%: `tip` now also transfers 10 of the 1 000 to the splitter's admin,
    // a transfer the naive path never authorizes.
    TipSplitterClient::new(env, &s.splitter).set_fee_bps(&100);

    let message = String::from_str(env, "forgot the fee");
    mock_owner_auth(&s, "forward_tip_naive", "@band", 1_000, &message);

    let res = TipForwarderClient::new(env, &s.forwarder).try_forward_tip_naive(
        &String::from_str(env, "@band"),
        &1_000,
        &message,
    );
    assert!(
        res.is_err(),
        "authorizing only the recipients' shares must fail once tip also pays a fee"
    );

    // Nothing moved: the tip reverted with the auth error.
    let token = token::Client::new(env, &s.token);
    assert_eq!(token.balance(&alice), 0);
    assert_eq!(token.balance(&bob), 0);
    assert_eq!(token.balance(&s.forwarder), 1_000);
}

#[test]
fn forwarded_tip_authorizes_the_protocol_fee() {
    let s = setup();
    let env = &s.env;
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    create_jar(&s, "@band", &[(alice.clone(), 7000), (bob.clone(), 3000)]);
    TipSplitterClient::new(env, &s.splitter).set_fee_bps(&100);

    let message = String::from_str(env, "with the fee");
    mock_owner_auth(&s, "forward_tip", "@band", 1_000, &message);

    TipForwarderClient::new(env, &s.forwarder).forward_tip(
        &String::from_str(env, "@band"),
        &1_000,
        &message,
    );

    // 10 to the admin, then 990 split 70/30.
    let token = token::Client::new(env, &s.token);
    assert_eq!(token.balance(&s.admin), 10);
    assert_eq!(token.balance(&alice), 693);
    assert_eq!(token.balance(&bob), 297);
    assert_eq!(token.balance(&s.forwarder), 0);
}

#[test]
fn forwarder_keeps_its_own_share_when_it_is_a_recipient() {
    let s = setup();
    let env = &s.env;
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    create_jar(
        &s,
        "@selfjar",
        &[
            (alice.clone(), 6000),
            (s.forwarder.clone(), 2000),
            (bob.clone(), 2000),
        ],
    );

    let message = String::from_str(env, "skimming myself");
    mock_owner_auth(&s, "forward_tip", "@selfjar", 1_000, &message);

    TipForwarderClient::new(env, &s.forwarder).forward_tip(
        &String::from_str(env, "@selfjar"),
        &1_000,
        &message,
    );

    // preview_tip reports [600, 0, 200]: `tip` makes no transfer to the
    // forwarder, so its 200 never leaves it, and alice and bob receive
    // exactly their shares.
    let token = token::Client::new(env, &s.token);
    assert_eq!(token.balance(&alice), 600);
    assert_eq!(token.balance(&bob), 200);
    assert_eq!(token.balance(&s.forwarder), 200);
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum HostileTokenError {
    NotAuthorized = 1,
    InsufficientBalance = 2,
}

#[contracttype]
#[derive(Clone)]
enum HostileTokenKey {
    Balance(Address),
    Blocked,
}

/// Minimal token contract whose `block` entry point makes transfers to one
/// recipient fail, triggering `tip_splitter`'s held-share escrow fallback
/// (`transfer(from, splitter, share)`).
#[contract]
pub struct HostileToken;

#[contractimpl]
impl HostileToken {
    pub fn mint(env: Env, to: Address, amount: i128) {
        let k = HostileTokenKey::Balance(to);
        let b: i128 = env.storage().persistent().get(&k).unwrap_or(0);
        env.storage().persistent().set(&k, &(b + amount));
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&HostileTokenKey::Balance(id))
            .unwrap_or(0)
    }

    pub fn block(env: Env, to: Address) {
        env.storage()
            .persistent()
            .set(&HostileTokenKey::Blocked, &to);
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        if let Some(b) = env
            .storage()
            .persistent()
            .get::<HostileTokenKey, Address>(&HostileTokenKey::Blocked)
        {
            if b == to {
                panic_with_error!(&env, HostileTokenError::NotAuthorized);
            }
        }
        let fk = HostileTokenKey::Balance(from.clone());
        let fb: i128 = env.storage().persistent().get(&fk).unwrap_or(0);
        if fb < amount {
            panic_with_error!(&env, HostileTokenError::InsufficientBalance);
        }
        env.storage().persistent().set(&fk, &(fb - amount));
        let tk = HostileTokenKey::Balance(to.clone());
        let tb: i128 = env.storage().persistent().get(&tk).unwrap_or(0);
        env.storage().persistent().set(&tk, &(tb + amount));
    }
}

#[test]
fn forwarded_tip_fails_when_a_recipients_share_must_be_held() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token = env.register(HostileToken, ());
    let splitter = env.register(TipSplitter, (admin.clone(), token.clone()));
    let owner = Address::generate(&env);
    let forwarder = env.register(TipForwarder, (owner.clone(), splitter.clone()));

    let token_client = HostileTokenClient::new(&env, &token);
    token_client.mint(&forwarder, &1_000);

    let s = Setup {
        env,
        owner,
        admin,
        token,
        splitter,
        forwarder,
    };
    let env = &s.env;
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    create_jar(&s, "@band", &[(alice.clone(), 7000), (bob.clone(), 3000)]);

    // Deauthorize Bob so `pay_out` falls back to `transfer(forwarder, splitter, 30)`
    // to escrow his share (#171). Because `forward_tip` only pre-authorized
    // `transfer(forwarder, bob, 30)`, the escrow transfer lacks contract auth
    // and the forwarded tip fails cleanly with no funds moved.
    HostileTokenClient::new(env, &s.token).block(&bob);

    let message = String::from_str(env, "bob is frozen");
    mock_owner_auth(&s, "forward_tip", "@band", 100, &message);

    let res = TipForwarderClient::new(env, &s.forwarder).try_forward_tip(
        &String::from_str(env, "@band"),
        &100,
        &message,
    );
    assert!(
        res.is_err(),
        "forward_tip must fail when a share falls back to an un-authorized escrow transfer to the splitter"
    );

    let token_client = HostileTokenClient::new(env, &s.token);
    assert_eq!(token_client.balance(&alice), 0);
    assert_eq!(token_client.balance(&bob), 0);
    assert_eq!(token_client.balance(&s.splitter), 0);
    assert_eq!(token_client.balance(&s.forwarder), 1_000);
    assert_eq!(TipSplitterClient::new(env, &s.splitter).get_held(&bob, &s.token), 0);
}
