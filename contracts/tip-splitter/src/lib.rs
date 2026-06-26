#![no_std]
//! Novatip — `tip_splitter` contract.
//!
//! A "tip jar" routes a single incoming USDC tip across one or more recipients
//! by basis-point splits. Splitting is atomic: either every recipient is paid in
//! the same transaction or the whole tip reverts.
//!
//! This commit scaffolds an empty contract; data types and logic follow.

use soroban_sdk::{contract, contractimpl, Env};

#[contract]
pub struct TipSplitter;

#[contractimpl]
impl TipSplitter {
    /// Placeholder so the crate compiles; replaced by real entrypoints next.
    pub fn version(_env: Env) -> u32 {
        1
    }
}
