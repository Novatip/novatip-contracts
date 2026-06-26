#![no_std]
//! Novatip — `tip_splitter` contract.
//!
//! A "tip jar" routes a single incoming USDC tip across one or more recipients
//! by basis-point splits. Splitting is atomic: either every recipient is paid in
//! the same transaction or the whole tip reverts.
//!
//! This commit adds the constructor and split validation; jar logic follows.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env, String, Vec,
};

/// 100% expressed in basis points.
const BPS_DENOM: u32 = 10_000;
/// Safety bound so a single tip can't fan out to an unbounded recipient list.
const MAX_RECIPIENTS: u32 = 20;

/// One recipient and the share of every tip they receive, in basis points.
#[contracttype]
#[derive(Clone)]
pub struct Split {
    pub to: Address,
    pub bps: u32,
}

/// A creator's tip jar: who controls it and how tips are split.
#[contracttype]
#[derive(Clone)]
pub struct Jar {
    pub owner: Address,
    pub splits: Vec<Split>,
}

#[contracttype]
pub enum DataKey {
    /// Contract admin (deployer); reserved for future migrations.
    Admin,
    /// Address of the USDC Stellar Asset Contract used for all tips.
    Token,
    /// A tip jar keyed by its public slug, e.g. "@alice".
    Jar(String),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    JarExists = 2,
    JarNotFound = 3,
    InvalidSplits = 4,
    InvalidAmount = 5,
    TooManyRecipients = 6,
}

#[contract]
pub struct TipSplitter;

#[contractimpl]
impl TipSplitter {
    /// Runs once at deploy time. `token` is the USDC Stellar Asset Contract id.
    pub fn __constructor(env: Env, admin: Address, token: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
    }

    /// Register a new tip jar. `owner` must authorize. Splits must sum to 100%.
    pub fn create_jar(env: Env, owner: Address, jar_id: String, splits: Vec<Split>) {
        owner.require_auth();
        let key = DataKey::Jar(jar_id);
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::JarExists);
        }
        Self::validate_splits(&env, &splits);
        env.storage().persistent().set(&key, &Jar { owner, splits });
    }

    /// Update an existing jar's splits. Only the jar owner may do this.
    pub fn update_splits(env: Env, jar_id: String, splits: Vec<Split>) {
        let key = DataKey::Jar(jar_id);
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        jar.owner.require_auth();
        Self::validate_splits(&env, &splits);
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: jar.owner,
                splits,
            },
        );
    }

    /// Send a tip. Transfers `amount` of USDC from `from`, split across the jar's
    /// recipients atomically, then emits a `("tip", jar_id)` event.
    pub fn tip(env: Env, from: Address, jar_id: String, amount: i128, message: String) {
        from.require_auth();
        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }

        let jar: Jar = env
            .storage()
            .persistent()
            .get(&DataKey::Jar(jar_id.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let client = token::Client::new(&env, &token_addr);

        let n = jar.splits.len();
        let mut distributed: i128 = 0;
        for i in 0..n {
            let split = jar.splits.get(i).unwrap();
            // Last recipient absorbs any rounding dust so the full amount is sent.
            let share = if i == n - 1 {
                amount - distributed
            } else {
                amount * (split.bps as i128) / (BPS_DENOM as i128)
            };
            if share > 0 {
                client.transfer(&from, &split.to, &share);
                distributed += share;
            }
        }

        env.events()
            .publish((symbol_short!("tip"), jar_id), (from, amount, message));
    }

    /// Read a jar's configuration.
    pub fn get_jar(env: Env, jar_id: String) -> Jar {
        env.storage()
            .persistent()
            .get(&DataKey::Jar(jar_id))
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound))
    }

    /// The USDC token address tips are settled in.
    pub fn get_token(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Validate that splits are non-empty, within bounds, and sum to 100%.
    fn validate_splits(env: &Env, splits: &Vec<Split>) {
        let n = splits.len();
        if n == 0 {
            panic_with_error!(env, Error::InvalidSplits);
        }
        if n > MAX_RECIPIENTS {
            panic_with_error!(env, Error::TooManyRecipients);
        }
        let mut total: u32 = 0;
        for i in 0..n {
            total += splits.get(i).unwrap().bps;
        }
        if total != BPS_DENOM {
            panic_with_error!(env, Error::InvalidSplits);
        }
    }
}
