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
    Allowance(Address, Address),
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

    /// A controllable allowance for recurring payout regression tests.
    pub fn approve(env: Env, from: Address, spender: Address, amount: i128, _expiration_ledger: u32) {
        env.storage().persistent().set(&TestTokenKey::Allowance(from, spender), &amount);
    }

    pub fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        env.storage().persistent().get(&TestTokenKey::Allowance(from, spender)).unwrap_or(0)
    }

    pub fn transfer_from(env: Env, spender: Address, from: Address, to: Address, amount: i128) {
        let key = TestTokenKey::Allowance(from.clone(), spender);
        let remaining: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if remaining < amount {
            panic_with_error!(&env, TestTokenError::InsufficientBalance);
        }
        // Reverts atomically if the recipient cannot receive, so allowance
        // cannot be debited for a transfer that never settled.
        Self::transfer(env.clone(), from, to, amount);
        env.storage().persistent().set(&key, &(remaining - amount));
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

/// A `tip_many` batch of `n` entries all naming `jar_id`, each for one stroop
/// with a one-byte message.
fn batch_of(env: &Env, jar_id: &String, n: u32) -> Vec<TipRequest> {
    let mut tips = Vec::new(env);
    for _ in 0..n {
        tips.push_back(TipRequest {
            jar_id: jar_id.clone(),
            amount: 1,
            message: String::from_str(env, "t"),
            reference: None,
        });
    }
    tips
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

// ---------------------------------------------------------------------------
// #177 — preview_tip: the sender-aware preview must agree, number for number,
// with the breakdown the `tip` event publishes for the same tip.
// ---------------------------------------------------------------------------

/// Turns a soroban `Vec<i128>` into a std vec so it can be compared with the
/// decoded event breakdown.
fn i128_vec_to_std(v: &Vec<i128>) -> std::vec::Vec<i128> {
    (0..v.len()).map(|i| v.get(i).unwrap()).collect()
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

// ── #142: Resource cost assertions ──────────────────────────────────────────

const MAX_INSTRUCTIONS_20_RECIPIENTS: u64 = 500_000_000; // headroom over measured
const MAX_MEMORY_20_RECIPIENTS: u64 = 200_000_000;

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

// ---------------------------------------------------------------------------
// #260 — hostile post-tip hooks (panic, error return, re-entrant tip)
// ---------------------------------------------------------------------------

mod erroring_hook {
    use super::*;

    #[contracterror]
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    #[repr(u32)]
    pub enum HookErr {
        Rejected = 1,
    }

    #[contract]
    pub struct ErroringHook;

    #[contractimpl]
    impl ErroringHook {
        pub fn on_tip(
            _env: Env,
            _jar_id: String,
            _from: Address,
            _amount: i128,
        ) -> Result<(), HookErr> {
            Err(HookErr::Rejected)
        }
    }
}

mod reentrant_hook {
    use super::*;

    #[contracttype]
    #[derive(Clone)]
    enum ReentrantKey {
        Target,
    }

    #[contract]
    pub struct ReentrantHook;

    #[contractimpl]
    impl ReentrantHook {
        pub fn __constructor(env: Env, target: Address) {
            env.storage().instance().set(&ReentrantKey::Target, &target);
        }

        pub fn on_tip(env: Env, jar_id: String, from: Address, amount: i128) {
            let target: Address = env
                .storage()
                .instance()
                .get(&ReentrantKey::Target)
                .expect("target set");
            let splitter = TipSplitterClient::new(&env, &target);
            splitter.tip(&from, &jar_id, &amount, &String::from_str(&env, "reenter"));
        }
    }
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

mod admin;
mod escrow;
mod fees;
mod jars;
mod interaction_regressions;
mod lifecycle_regressions;
mod properties;
mod recurring;
mod splits;
mod tips;
mod views;
