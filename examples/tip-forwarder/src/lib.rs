//! Worked example: tipping a Novatip jar **from another contract**.
//!
//! `tip_splitter.tip` calls `from.require_auth()`, which a calling contract
//! satisfies automatically — but the per-recipient `transfer(from, to, share)`
//! calls it then makes on the token are invoked by `tip_splitter`, not by the
//! caller, so they are *not* covered. The caller has to pre-authorize each
//! one with `env.authorize_as_current_contract`, listing every recipient and
//! the exact amount, or the tip fails with an auth error.
//!
//! The amounts to authorize are what `preview_split` returns, with one
//! exception: if the calling contract is itself a recipient, `tip` skips the
//! transfer to it and rolls that share into the last recipient (issue #66),
//! so the amounts to authorize then differ from the preview.
//! [`forward_tip`] does the adjustment; [`forward_tip_naive`] deliberately
//! does not, and the tests in `src/test.rs` show it failing for exactly that
//! reason.
//!
//! The `tip_splitter` interface this contract calls is declared locally with
//! `#[contractclient]` instead of imported with `contractimport!`, so
//! `cargo test` passes from a clean checkout without a built wasm. In
//! production both contracts are deployed and the client calls the real
//! `splitter` address stored at construction.

// `no_std` only applies to the real (wasm) build; the contract logic itself
// stays testable on the host, where `testutils` needs the standard library.
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
    fn get_token(env: Env) -> Address;
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
    /// Authorizes exactly the transfers `tip` will make: the preview's shares,
    /// corrected for the case where this contract is itself a recipient.
    pub fn forward_tip(env: Env, jar_id: String, amount: i128, message: String) {
        Self::forward(env, jar_id, amount, message, true);
    }

    /// Same as [`forward_tip`], but authorizes the raw `preview_split` amounts
    /// without the self-recipient correction. Exists so the tests can show
    /// the failure mode; production callers should use [`forward_tip`].
    pub fn forward_tip_naive(env: Env, jar_id: String, amount: i128, message: String) {
        Self::forward(env, jar_id, amount, message, false);
    }
}

impl TipForwarder {
    fn forward(env: Env, jar_id: String, amount: i128, message: String, adjust: bool) {
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

        // preview_split is the payout `tip` will make, so it is also the list
        // to authorize — except when this contract is itself a recipient, in
        // which case `tip` withholds the self-transfer and rolls it into the
        // last recipient's share.
        let shares = client.preview_split(&jar_id, &amount);
        let jar = client.get_jar(&jar_id);
        let transfers = if adjust {
            Self::actual_transfers(&env, &jar.splits, &shares, &myself)
        } else {
            let mut out: Vec<(Address, i128)> = Vec::new(&env);
            for i in 0..shares.len() {
                let share = shares.get(i).unwrap();
                if share > 0 {
                    out.push_back((jar.splits.get(i).unwrap().to, share));
                }
            }
            out
        };

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

    /// Replay `tip`'s payment loop — skip self-transfers, roll the withheld
    /// share into the last recipient — and report the transfers that actually
    /// happen. These, not the raw preview, are the amounts to authorize when
    /// the forwarder is itself a recipient.
    fn actual_transfers(
        env: &Env,
        splits: &Vec<Split>,
        shares: &Vec<i128>,
        myself: &Address,
    ) -> Vec<(Address, i128)> {
        let n = shares.len();
        let mut out: Vec<(Address, i128)> = Vec::new(env);
        let mut skipped: i128 = 0;
        for i in 0..n {
            let to = splits.get(i).unwrap().to.clone();
            let mut share = shares.get(i).unwrap();
            if i == n - 1 {
                share += skipped;
            }
            if share > 0 && to != *myself {
                out.push_back((to, share));
            } else {
                skipped += share;
            }
        }
        out
    }
}

mod test;
