//! Worked example: tipping a Novatip jar **from another contract**.
//!
//! `tip_splitter.tip` calls `from.require_auth()`, which a calling contract
//! satisfies automatically — but the per-recipient `transfer(from, to, share)`
//! calls it then makes on the token are invoked by `tip_splitter`, not by the
//! caller, so they are *not* covered. The caller has to pre-authorize each
//! one with `env.authorize_as_current_contract`, listing every recipient and
//! the exact amount, or the tip fails with an auth error.
//!
//! The amounts to authorize are what `preview_tip(jar_id, myself, amount)`
//! returns — the sender-aware preview, which reports `0` for a recipient who
//! is the caller itself, because `tip` makes no transfer to it and the share
//! stays with the caller — plus, once the admin has set a protocol fee (#79),
//! one more transfer: the fee, paid to the contract admin. Every preview
//! reports post-fee shares, so the fee is `amount - sum(preview_split)`.
//! [`forward_tip`] authorizes both; [`forward_tip_naive`] deliberately
//! forgets the fee, and the tests in `src/test.rs` show it failing for
//! exactly that reason.
//!
//! The `tip_splitter` interface this contract calls is declared locally with
//! `#[contractclient]` instead of imported with `contractimport!`, so
//! `cargo test` passes from a clean checkout without a built wasm. In
//! production both contracts are deployed and the client calls the real
//! `splitter` address stored at construction.

#![cfg_attr(not(test), no_std)]

use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error,
    symbol_short, vec, Address, Env, IntoVal, String, Vec,
};

/// The tip_splitter surface this contract uses. Signatures must match
/// `contracts/tip-splitter/src/lib.rs`; they are local declarations, not
/// imports, so tests never need the deployed contract's wasm.
#[contractclient(name = "SplitterClient")]
pub trait SplitterInterface {
    fn get_jar(env: Env, jar_id: String) -> Jar;
    fn preview_split(env: Env, jar_id: String, amount: i128) -> Vec<i128>;
    fn preview_tip(env: Env, jar_id: String, from: Address, amount: i128) -> Vec<i128>;
    fn get_token(env: Env) -> Address;
    fn get_admin(env: Env) -> Address;
    fn tip(env: Env, from: Address, jar_id: String, amount: i128, message: String) -> u64;
}

/// Mirror of `tip_splitter::Split`. The XDR is a map keyed by field names, so
/// an identical layout decodes the real jar.
#[contracttype]
#[derive(Clone)]
pub struct Split {
    pub to: Address,
    pub bps: u32,
}

/// Mirror of `tip_splitter::Jar`.
#[contracttype]
#[derive(Clone)]
pub struct Jar {
    pub owner: Address,
    pub splits: Vec<Split>,
    /// Lock flag added by #173; the XDR is a name-keyed map, so the mirror
    /// must carry every field the real `Jar` stores.
    pub locked: bool,
}

#[contracttype]
pub enum DataKey {
    /// Account allowed to trigger forwarded tips.
    Owner,
    /// The `tip_splitter` contract tips are forwarded to.
    Splitter,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
}

#[contract]
pub struct TipForwarder;

#[contractimpl]
impl TipForwarder {
    /// `owner` gates `forward_tip`; `splitter` is the deployed `tip_splitter`.
    pub fn __constructor(env: Env, owner: Address, splitter: Address) {
        env.storage().instance().set(&DataKey::Owner, &owner);
        env.storage().instance().set(&DataKey::Splitter, &splitter);
    }

    /// Tip `jar_id` `amount` from this contract's own balance. The owner must
    /// authorize.
    ///
    /// Authorizes exactly the transfers `tip` will make: the sender-aware
    /// preview's shares, plus the protocol fee when one is configured.
    pub fn forward_tip(env: Env, jar_id: String, amount: i128, message: String) {
        Self::forward(env, jar_id, amount, message, true);
    }

    /// Same as [`forward_tip`], but authorizes only the recipients' shares and
    /// forgets the protocol fee transfer. Exists so the tests can show the
    /// failure mode; production callers should use [`forward_tip`].
    pub fn forward_tip_naive(env: Env, jar_id: String, amount: i128, message: String) {
        Self::forward(env, jar_id, amount, message, false);
    }
}

impl TipForwarder {
    fn forward(env: Env, jar_id: String, amount: i128, message: String, with_fee: bool) {
        let owner: Address = env
            .storage()
            .instance()
            .get(&DataKey::Owner)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        owner.require_auth();
        let splitter: Address = env
            .storage()
            .instance()
            .get(&DataKey::Splitter)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        let client = SplitterClient::new(&env, &splitter);
        let myself = env.current_contract_address();

        // preview_tip is the payout `tip` will make for this sender: a share
        // that would go to this contract itself is reported as 0 and never
        // transferred, so it needs no authorization.
        let plan = client.preview_tip(&jar_id, &myself, &amount);
        let jar = client.get_jar(&jar_id);
        let mut transfers: Vec<(Address, i128)> = Vec::new(&env);
        for i in 0..plan.len() {
            let share = plan.get(i).unwrap();
            if share > 0 {
                transfers.push_back((jar.splits.get(i).unwrap().to, share));
            }
        }
        if with_fee {
            if let Some(fee) = Self::fee_transfer(&client, &jar_id, amount, &myself) {
                transfers.push_back(fee);
            }
        }

        // Each transfer `tip` will invoke on the token, authorized by *this*
        // contract: (from = ourselves, to = recipient, amount).
        let token = client.get_token();
        let mut entries: Vec<InvokerContractAuthEntry> = Vec::new(&env);
        for i in 0..transfers.len() {
            let (to, share) = transfers.get(i).unwrap();
            let args = vec![
                &env,
                myself.clone().into_val(&env),
                to.into_val(&env),
                share.into_val(&env),
            ];
            entries.push_back(InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: token.clone(),
                    fn_name: symbol_short!("transfer"),
                    args,
                },
                sub_invocations: vec![&env],
            }));
        }
        env.authorize_as_current_contract(entries);

        client.tip(&myself, &jar_id, &amount, &message);
    }

    /// The protocol-fee transfer `tip` will make, if any (#79): `tip` takes
    /// the fee off the top and pays it to the admin, so the previews' shares
    /// sum to `amount - fee`. `None` when no fee is configured, or when this
    /// contract is the admin and so keeps the fee rather than paying itself.
    fn fee_transfer(
        client: &SplitterClient,
        jar_id: &String,
        amount: i128,
        myself: &Address,
    ) -> Option<(Address, i128)> {
        let shares = client.preview_split(jar_id, &amount);
        let mut net: i128 = 0;
        for i in 0..shares.len() {
            net += shares.get(i).unwrap();
        }
        let fee = amount - net;
        let admin = client.get_admin();
        if fee > 0 && admin != *myself {
            Some((admin, fee))
        } else {
            None
        }
    }
}

mod test;
