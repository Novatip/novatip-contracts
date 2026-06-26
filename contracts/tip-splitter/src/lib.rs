#![no_std]
//! Novatip — `tip_splitter` contract.
//!
//! A "tip jar" routes a single incoming USDC tip across one or more recipients
//! by basis-point splits. Splitting is atomic: either every recipient is paid in
//! the same transaction or the whole tip reverts.
//!
//! This commit adds the data model; constructor and logic follow.

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, String, Vec};

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
    /// Placeholder so the crate compiles; replaced by real entrypoints next.
    pub fn version(_env: Env) -> u32 {
        1
    }
}
