// `proptest` (a dev-dependency used by the property tests) needs `std`, so
// `no_std` only applies to the real (wasm) build; the contract logic itself
// never touches `std`, so this doesn't change on-chain behavior.
#![cfg_attr(not(test), no_std)]
//! Novatip — `tip_splitter` contract.
//!
//! A "tip jar" routes a single incoming USDC tip across one or more recipients
//! by basis-point splits. Splitting is atomic: either every recipient is paid in
//! the same transaction or the whole tip reverts.
//!
//! Jar discovery is intentionally event-driven: `create_jar` emits a
//! `jar_crtd` event, and indexers reconstruct the full jar list by scanning
//! those events. This keeps on-chain storage O(1) regardless of how many jars
//! are ever registered.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env, String, Vec,
};

/// 100% expressed in basis points.
const BPS_DENOM: u32 = 10_000;
/// Safety bound so a single tip can't fan out to an unbounded recipient list.
const MAX_RECIPIENTS: u32 = 20;
/// Longest `jar_id`, in bytes, accepted by `create_jar`.
///
/// The id is used as a storage key, an event topic, and a public URL slug, so
/// an unbounded id costs unnecessary rent and can produce jars no frontend can
/// address.
const MAX_JAR_ID_LEN: u32 = 64;
/// Longest tip message, in UTF-8 bytes, that may ride along in the `tip` event.
///
/// The message is echoed verbatim into the event payload, so an unbounded
/// string inflates the transaction and every downstream copy the indexer has
/// to store and serve. 280 matches the character budget the tip form implies
/// for plain-ASCII text.
///
/// This is a byte count, not a character count. Accented characters (é, ö)
/// cost 2 bytes each in UTF-8 and most emoji cost 4, so a non-ASCII message
/// hits the limit at well under 280 visible characters. The contract checks
/// `message.len()`, which returns the byte count, so the rejection boundary
/// is 280 bytes regardless of character count.
///
/// Clients should count UTF-8 bytes — not `message.length` in JavaScript or
/// `len(message)` in Rust — to show an accurate remaining-bytes indicator.
const MAX_MESSAGE_LEN: u32 = 280;

/// How long a jar's persistent entry is extended to on access, in ledgers.
///
/// Soroban archives a persistent entry once its time to live runs out, after
/// which `get_jar` and `tip` fail until it is restored. Every read and write
/// path bumps the entry, so a jar that is used at all never expires; this
/// bound only governs how long a completely idle jar survives.
///
/// At the default 5-second close rate, 1 000 000 ledgers is roughly 58 days.
/// That comfortably covers a creator between gigs while staying inside the
/// network's maximum persistent entry lifetime.
const JAR_TTL_LEDGERS: u32 = 1_000_000;

/// Bump a jar's time to live only once it drops below this many ledgers.
///
/// `extend_ttl` takes a threshold as well as a target: below the threshold the
/// entry is extended back up to `JAR_TTL_LEDGERS`, and above it the call is a
/// no-op. Setting the threshold under the target means an actively tipped jar
/// pays for the write occasionally rather than on every single access.
const JAR_TTL_THRESHOLD: u32 = 500_000;

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

/// The contract's hard bounds, returned by `get_limits`.
///
/// Clients that validate input before submitting a transaction need the same
/// numbers the contract enforces. Shipping them as a view rather than as
/// hardcoded client constants means a bound can only be changed in one place.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    /// 100% expressed in basis points, the denominator every `bps` is a share of.
    pub bps_denom: u32,
    /// Most recipients a single jar may split a tip across.
    pub max_recipients: u32,
    /// Longest tip message, in UTF-8 bytes.
    pub max_message_len: u32,
}

#[contracttype]
pub enum DataKey {
    /// Contract admin (deployer); reserved for future migrations.
    Admin,
    /// Address of the USDC Stellar Asset Contract used for all tips.
    Token,
    /// A tip jar keyed by its public slug, e.g. "@alice".
    Jar(String),
    /// Optional minimum tip amount for a jar keyed by its public slug.
    MinTip(String),
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
    DuplicateRecipient = 7,
    MessageTooLong = 8,
    InvalidJarId = 9,
    /// Splits list is empty.
    SplitsEmpty = 10,
    /// A split has a basis-point share that is zero or above 100 %.
    SplitOutOfRange = 11,
    /// Splits sum to something other than 10 000 bps.
    SplitSumNot100Pct = 12,
    /// Tip amount is below the jar's configured minimum tip amount.
    BelowMinTipAmount = 13,
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

    /// Register a new tip jar. `owner` must authorize. `jar_id` must be
    /// non-empty and at most `MAX_JAR_ID_LEN` bytes. Splits must sum to 100%
    /// and may not name the same recipient twice.
    /// Emits a `jar_crtd` event so indexers can discover all jars from the
    /// event log without any on-chain list.
    pub fn create_jar(env: Env, owner: Address, jar_id: String, splits: Vec<Split>) {
        owner.require_auth();
        if jar_id.is_empty() || jar_id.len() > MAX_JAR_ID_LEN {
            panic_with_error!(&env, Error::InvalidJarId);
        }
        let key = DataKey::Jar(jar_id.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::JarExists);
        }
        Self::validate_splits(&env, &splits);
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: owner.clone(),
                splits,
            },
        );

        env.events()
            .publish((symbol_short!("jar_crtd"), jar_id), owner);
    }

    /// Update an existing jar's splits. Only the jar owner may do this.
    /// Emits a `splits` event so indexers caching a jar's splits know to
    /// refetch them.
    pub fn update_splits(env: Env, jar_id: String, splits: Vec<Split>) {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        jar.owner.require_auth();
        Self::validate_splits(&env, &splits);
        let split_count = splits.len();
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: jar.owner,
                splits,
            },
        );

        env.events()
            .publish((symbol_short!("splits"), jar_id), split_count);
    }

    /// Transfer control of a jar to a new owner. Only the current owner may do
    /// this; the new owner does not need to authorize. Splits are unchanged.
    /// Emits a `jar_xfer` event carrying both the outgoing and the incoming
    /// owner, so indexers can update who controls the jar.
    pub fn transfer_jar_ownership(env: Env, jar_id: String, new_owner: Address) {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        jar.owner.require_auth();
        let prev_owner = jar.owner;
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: new_owner.clone(),
                splits: jar.splits,
            },
        );

        // Both ends of the move, so the event is meaningful on its own. With
        // only the new owner, an indexer starting from a partial history can
        // see where a jar went but not where it came from, unless it has
        // already replayed every prior event for that jar.
        env.events()
            .publish((symbol_short!("jar_xfer"), jar_id), (prev_owner, new_owner));
    }

    /// Set an optional minimum tip amount for a jar. Only the jar owner may do this.
    ///
    /// If `min_amount` is `Some(amount)`, `amount` must be strictly positive (`> 0`).
    /// Passing `None` clears any previously set minimum tip amount.
    ///
    /// Emits a `min_tip` event carrying the jar slug and the configured minimum.
    pub fn set_min_tip_amount(env: Env, jar_id: String, min_amount: Option<i128>) {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        jar.owner.require_auth();

        let min_key = DataKey::MinTip(jar_id.clone());
        match min_amount {
            Some(amount) => {
                if amount <= 0 {
                    panic_with_error!(&env, Error::InvalidAmount);
                }
                env.storage().persistent().set(&min_key, &amount);
                env.storage()
                    .persistent()
                    .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
            }
            None => {
                env.storage().persistent().remove(&min_key);
            }
        }

        env.events()
            .publish((symbol_short!("min_tip"), jar_id), min_amount);
    }

    /// Send a tip. Transfers `amount` of USDC from `from`, split across the jar's
    /// recipients atomically, then emits a `("tip", jar_id)` event carrying the
    /// sender, the total, the message, and the per-recipient breakdown in split
    /// order.
    ///
    /// `message` may be at most `MAX_MESSAGE_LEN` bytes; it is rejected before
    /// any funds move.
    pub fn tip(env: Env, from: Address, jar_id: String, amount: i128, message: String) {
        from.require_auth();
        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        if message.len() > MAX_MESSAGE_LEN {
            panic_with_error!(&env, Error::MessageTooLong);
        }

        let jar_key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&jar_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));

        // Enforce optional per-jar minimum tip amount if configured
        let min_key = DataKey::MinTip(jar_id.clone());
        if let Some(min_amount) = env.storage().persistent().get::<DataKey, i128>(&min_key) {
            if amount < min_amount {
                panic_with_error!(&env, Error::BelowMinTipAmount);
            }
            env.storage()
                .persistent()
                .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }

        // Bump the jar's storage entry so a jar that is tipped regularly
        // is never archived, and a jar idle for a few years still works.
        env.storage()
            .persistent()
            .extend_ttl(&jar_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let client = token::Client::new(&env, &token_addr);

        let n = jar.splits.len();
        // One shared calculation with `preview_split`, so the number a client
        // shows before signing is the number paid out here.
        let shares = Self::compute_shares(&env, &jar.splits, amount);

        let mut skipped: i128 = 0;
        // The per-recipient breakdown published in the `tip` event, in split
        // order. Recording it as the transfers happen is what makes the event
        // self-describing: an indexer that recomputed the split arithmetic
        // from `get_jar` could get a different answer, because the jar's
        // splits may have changed between the tip and the read.
        let mut breakdown: Vec<(Address, i128)> = Vec::new(&env);
        for i in 0..n {
            let split = jar.splits.get(i).unwrap();
            let mut share = shares.get(i).unwrap();
            if i == n - 1 {
                // A share withheld from a self-transfer above was never sent,
                // so it stays with the tipper by rolling into the last
                // recipient's remainder — exactly as it did when this loop
                // accumulated only the amounts it actually transferred.
                share += skipped;
            }
            let paid = if share > 0 && split.to != from {
                // Skip self-transfers: a tipper who is also a recipient would
                // otherwise pay themselves with a no-op transfer that burns gas
                // and emits a confusing token event.
                client.transfer(&from, &split.to, &share);
                share
            } else {
                skipped += share;
                // Nothing moved, so the breakdown reports 0 rather than the
                // notional share. The event is a record of the transfers this
                // call actually made, which is what a balance-tracking indexer
                // needs to stay in step with the ledger.
                0
            };
            breakdown.push_back((split.to, paid));
        }

        env.events().publish(
            (symbol_short!("tip"), jar_id),
            (from, amount, message, breakdown),
        );
    }

    /// Read a jar's configuration.
    pub fn get_jar(env: Env, jar_id: String) -> Jar {
        let key = DataKey::Jar(jar_id);
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        jar
    }

    /// Read a jar's owner without pulling its splits across the wire.
    ///
    /// The alternative — `get_jar(jar_id).owner` — deserializes the whole
    /// recipient vector to read one address, which for a jar near the
    /// `MAX_RECIPIENTS` cap is a lot of data moved to answer "do I control
    /// this jar?". A dashboard badge or a client deciding whether the
    /// connected wallet may call `update_splits` only needs the address.
    ///
    /// Panics with `JarNotFound` if the slug is unregistered, matching
    /// `get_jar`.
    pub fn get_jar_owner(env: Env, jar_id: String) -> Address {
        let key = DataKey::Jar(jar_id);
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        jar.owner
    }

    /// How many recipients a jar pays, without reading the recipients.
    ///
    /// A tip page showing a "3 collaborators" badge needs one integer, and
    /// `get_jar(jar_id).splits.len()` makes it pay for the whole recipient
    /// vector to get it. This also gives a client a way to decide whether
    /// fetching the full split list is worth it.
    ///
    /// The count is always between 1 and `MAX_RECIPIENTS`: `validate_splits`
    /// rejects an empty list, so a stored jar always has at least one
    /// recipient.
    ///
    /// Panics with `JarNotFound` if the slug is unregistered, matching
    /// `get_jar`.
    pub fn get_split_count(env: Env, jar_id: String) -> u32 {
        let key = DataKey::Jar(jar_id);
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        jar.splits.len()
    }

    /// What each recipient would receive from a tip of `amount`, in split
    /// order.
    ///
    /// Clients previously recomputed the split themselves to show "Alice gets
    /// 7.00, Bob gets 3.00" before the supporter signs, which put the rounding
    /// rule in two places and let the two drift apart. This runs the same
    /// arithmetic `tip` does, so the preview is the payout — dust included.
    ///
    /// The returned shares always sum to exactly `amount`: the final recipient
    /// absorbs whatever the integer divisions truncated.
    ///
    /// Rejects the same amounts `tip` rejects — non-positive, or small enough
    /// that some recipient's share would truncate to zero — so a preview that
    /// returns at all describes a tip that can go through. Panics with
    /// `JarNotFound` if the slug is unregistered.
    ///
    /// One caveat: `tip` withholds a recipient's share when that recipient is
    /// the tipper, rather than making them pay themselves. This view does not
    /// know who is tipping, so it reports every recipient's full share. A
    /// client whose connected wallet appears in the splits should account for
    /// that itself.
    pub fn preview_split(env: Env, jar_id: String, amount: i128) -> Vec<i128> {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        let min_key = DataKey::MinTip(jar_id);
        if let Some(min_amount) = env.storage().persistent().get::<DataKey, i128>(&min_key) {
            if amount < min_amount {
                panic_with_error!(&env, Error::BelowMinTipAmount);
            }
            env.storage()
                .persistent()
                .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }

        Self::compute_shares(&env, &jar.splits, amount)
    }

    /// Read a jar's optional minimum tip amount.
    ///
    /// Returns `Some(amount)` if a minimum is set, or `None` if no minimum is configured.
    /// Panics with `JarNotFound` if the slug is not registered.
    pub fn get_min_tip_amount(env: Env, jar_id: String) -> Option<i128> {
        let key = DataKey::Jar(jar_id.clone());
        if !env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::JarNotFound);
        }
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        let min_key = DataKey::MinTip(jar_id);
        let min_amount: Option<i128> = env.storage().persistent().get(&min_key);
        if min_amount.is_some() {
            env.storage()
                .persistent()
                .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }
        min_amount
    }

    /// Whether `jar_id` is already registered.
    ///
    /// A slug-availability check would otherwise have to call `get_jar` and
    /// catch the `JarNotFound` panic, which is awkward from the SDK. This
    /// returns a plain `bool` and reads one storage key, so the onboarding form
    /// can run it on every (debounced) keystroke.
    pub fn jar_exists(env: Env, jar_id: String) -> bool {
        env.storage().persistent().has(&DataKey::Jar(jar_id))
    }

    /// Whether `address` appears in the jar's splits.
    ///
    /// A collaborator added to someone else's jar would otherwise have to
    /// fetch the whole jar and scan the split vector for their own address,
    /// which is awkward from a wallet or a one-line script. This reads the
    /// same single storage key and answers the question directly.
    ///
    /// Ownership is a separate thing: a jar owner who is not also a recipient
    /// gets `false`, because they receive no share of a tip. Use
    /// `get_jar_owner` to ask who controls a jar.
    ///
    /// Panics with `JarNotFound` if the slug is not registered, matching
    /// `get_jar`. A missing jar is not the same answer as "not a recipient",
    /// and conflating the two would hide a typo'd slug; use `jar_exists` to
    /// test registration.
    pub fn is_recipient(env: Env, jar_id: String, address: Address) -> bool {
        let key = DataKey::Jar(jar_id);
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        for i in 0..jar.splits.len() {
            if jar.splits.get(i).unwrap().to == address {
                return true;
            }
        }
        false
    }

    /// The contract admin recorded at deploy time.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// The USDC token address tips are settled in.
    pub fn get_token(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// The bounds this contract enforces: the basis-point denominator, the
    /// maximum recipient count, and the maximum tip-message length in bytes.
    ///
    /// These are compile-time constants, so the view reads no storage. It
    /// exists so a client can discover the limits of the contract it is
    /// actually talking to instead of hardcoding its own copy, which would
    /// silently desynchronise the moment a bound here changed.
    pub fn get_limits(_env: Env) -> Limits {
        Limits {
            bps_denom: BPS_DENOM,
            max_recipients: MAX_RECIPIENTS,
            max_message_len: MAX_MESSAGE_LEN,
        }
    }

    /// Split `amount` across `splits` in basis points, returning one share per
    /// recipient in split order.
    ///
    /// This is the single source of the rounding rule: each non-final recipient
    /// gets `amount * bps / 10_000` (truncating integer division) and the final
    /// recipient gets the remainder, so the shares always sum to exactly
    /// `amount` and no dust is lost. `tip` pays these out and `preview_split`
    /// reports them, which is what keeps the quoted split and the paid split
    /// identical.
    ///
    /// Panics with `InvalidAmount` if `amount` is not positive, if it is small
    /// enough that some recipient's share would truncate to zero, or if it is
    /// so large that `amount * bps` overflows `i128`.
    fn compute_shares(env: &Env, splits: &Vec<Split>, amount: i128) -> Vec<i128> {
        if amount <= 0 {
            panic_with_error!(env, Error::InvalidAmount);
        }
        let n = splits.len();
        let denom = BPS_DENOM as i128;

        // Reject amounts too small to pay every recipient a non-zero share.
        // With integer division, a recipient's share of `amount * bps / 10_000`
        // truncates to zero when `amount < 10_000 / bps`. If that happens, the
        // recipient is silently skipped and the final recipient absorbs the
        // dust — the tip succeeds but the collaborator never sees it.
        // Better to fail loudly with InvalidAmount than to pay nobody.
        for i in 0..n {
            let bps = splits.get(i).unwrap().bps as i128;
            // `checked_mul`: `amount * bps` can overflow i128 before the
            // division brings it back into range. That is a caller error — the
            // amount is too large for the contract to split safely — so it
            // surfaces as a typed error rather than an opaque wasm trap.
            let product = amount
                .checked_mul(bps)
                .unwrap_or_else(|| panic_with_error!(env, Error::InvalidAmount));
            if product < denom {
                panic_with_error!(env, Error::InvalidAmount);
            }
        }

        let mut shares = Vec::new(env);
        let mut distributed: i128 = 0;
        for i in 0..n {
            // Last recipient absorbs any rounding dust so the full amount is
            // accounted for.
            let share = if i == n - 1 {
                amount - distributed
            } else {
                amount
                    .checked_mul(splits.get(i).unwrap().bps as i128)
                    .unwrap_or_else(|| panic_with_error!(env, Error::InvalidAmount))
                    / denom
            };
            distributed += share;
            shares.push_back(share);
        }
        shares
    }

    /// Validate that splits are non-empty, within bounds, carry a share that is
    /// neither zero nor above 100% each, and sum to exactly 100%.
    ///
    /// A `bps == 0` entry would never be paid — `tip` skips zero shares — so it
    /// is dead weight that still consumes a slot against `MAX_RECIPIENTS` and
    /// misleads clients into showing a collaborator who never receives funds.
    ///
    /// A `bps > BPS_DENOM` entry claims more than the whole tip, so it can never
    /// belong to a set summing to 100%. Rejecting it per entry also bounds the
    /// running total at `MAX_RECIPIENTS * BPS_DENOM` (200_000), which keeps the
    /// accumulator far below `u32::MAX` by construction.
    fn validate_splits(env: &Env, splits: &Vec<Split>) {
        let n = splits.len();
        if n == 0 {
            panic_with_error!(env, Error::SplitsEmpty);
        }
        if n > MAX_RECIPIENTS {
            panic_with_error!(env, Error::TooManyRecipients);
        }
        let mut total: u32 = 0;
        for i in 0..n {
            let split = splits.get(i).unwrap();
            let bps = split.bps;
            if bps == 0 || bps > BPS_DENOM {
                panic_with_error!(env, Error::SplitOutOfRange);
            }
            // `checked_add` rather than `+=`: `bps` is caller-supplied, and an
            // overflow must surface as the same typed `InvalidSplits` every
            // other rejection returns, not as an opaque wasm trap. The bound
            // above already makes overflow unreachable, so this is belt and
            // braces — but it puts the invariant in the code rather than
            // resting on `overflow-checks = true` in the release profile.
            total = total
                .checked_add(bps)
                .unwrap_or_else(|| panic_with_error!(env, Error::SplitSumNot100Pct));
            // Pairwise comparison rather than a set: `n` is capped at
            // MAX_RECIPIENTS (20), so this is at most 190 comparisons, and a hash
            // set would need an allocator we don't have under `no_std`.
            for j in (i + 1)..n {
                if splits.get(j).unwrap().to == split.to {
                    panic_with_error!(env, Error::DuplicateRecipient);
                }
            }
        }
        if total != BPS_DENOM {
            panic_with_error!(env, Error::SplitSumNot100Pct);
        }
    }
}

mod test;
