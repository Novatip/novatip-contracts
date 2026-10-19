// `proptest` (a dev-dependency used by the property tests) needs `std`, so
// `no_std` only applies to the real (wasm) build; the contract logic itself
// never touches `std`, so this doesn't change on-chain behavior.
#![cfg_attr(not(test), no_std)]
//! Novatip — `tip_splitter` contract.
//!
//! A "tip jar" routes a single incoming USDC tip across one or more recipients
//! by basis-point shares. A tipper never pays a share the jar failed to
//! deliver: if a recipient cannot receive the token — a dropped trustline, a
//! deauthorised account — their share is held by the contract under
//! [`DataKey::Held`] and paid out later by `withdraw_held`, while every other
//! recipient is still paid in the same transaction. This replaces the earlier
//! all-or-nothing behaviour, where one collaborator's account state reverted
//! the whole tip and punished the supporter for it.
//!
//! Jar discovery is intentionally event-driven: `create_jar` emits a
//! `jar_crtd` event, and indexers reconstruct the full jar list by scanning
//! those events. This keeps on-chain storage O(1) regardless of how many jars
//! are ever registered.

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error,
    symbol_short, token, Address, BytesN, Env, IntoVal, String, Val, Vec,
};

/// 100% expressed in basis points.
const BPS_DENOM: u32 = 10_000;
/// Safety bound so a single tip can't fan out to an unbounded recipient list.
const MAX_RECIPIENTS: u32 = 20;
/// Most jars one `tip_many` call may tip.
///
/// Each entry pays up to `MAX_RECIPIENTS` recipients, so the batch fan-out is
/// `MAX_BATCH_TIPS * MAX_RECIPIENTS` token transfers plus one `tip` event per
/// jar, all inside one transaction's budget. 10 keeps the worst case at 200
/// transfers, in the same order of magnitude as a single maximum-size jar; a
/// larger batch would abort on budget rather than on a typed error, which is
/// exactly the failure this bound exists to convert into a named one.
const MAX_BATCH_TIPS: u32 = 10;
/// Longest `jar_id`, in bytes, accepted by `create_jar`.
///
/// The id is used as a storage key, an event topic, and a public URL slug, so
/// an unbounded id costs unnecessary rent and can produce jars no frontend can
/// address.
const MAX_JAR_ID_LEN: u32 = 64;
/// Shortest slug body, excluding the leading `@`. Mirrors `novatip-backend`'s
/// `SLUG_REGEX`, so every id a tip page can produce is accepted here.
const MIN_JAR_SLUG_LEN: u32 = 3;
/// Longest slug body, excluding the leading `@`. Mirrors the backend as well;
/// the backend never emits a longer slug, and a jar it cannot link to is dead
/// weight.
const MAX_JAR_SLUG_LEN: u32 = 32;
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
///
/// Length is only half the contract: the bytes must also be valid UTF-8 text
/// with no control characters other than `\n` (see `validate_message`).
const MAX_MESSAGE_LEN: u32 = 280;

/// Highest protocol fee the admin may configure, in basis points (5%).
///
/// The fee is taken from every tip before the split, so an uncapped value
/// would let a compromised or careless admin key swallow a tip outright. 5%
/// matches what Buy Me a Coffee charges and stays well under Patreon's 8–12%;
/// the intended configured value is 100 bps (1%), which keeps cross-border
/// micro-tipping viable. `set_fee_bps` rejects anything above this with
/// `FeeTooHigh`.
const MAX_FEE_BPS: u32 = 500;

/// Width, in bytes, of the optional client reference a tipper may attach.
///
/// A reference is an opaque id the integration uses to match the on-chain
/// `tip` event back to whatever it created off-chain (a pending alert, a bot
/// command, a checkout session). 32 bytes fits the id spaces those clients
/// already use — a UUIDv4 in binary, a `sha256` tag, an 8-byte big-endian
/// primary key — without the contract ascribing any meaning to the contents.
///
/// Written as a literal `BytesN<32>` in the entry-point signatures rather than
/// `BytesN<REFERENCE_LEN>`: `#[contractimpl]` parses the generic arguments of
/// its own parameters and rejects a const with
/// "incorrect number of generic arguments, expect one for BytesN<N>", which
/// aborts expansion of the whole impl — every method then vanishes from the
/// generated client, and the error surfaces far from its cause.
pub const REFERENCE_LEN: usize = 32;

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

/// Extend the contract instance TTL to this many ledgers on use.
///
/// The instance storage holds the admin and token addresses, plus the Wasm code
/// entry itself in soroban-sdk 22. Once archived, every call fails until someone
/// restores it. Reusing the same target as jars keeps the lifetime consistent
/// across all persistent state (~58 days at 5s per ledger).
const INSTANCE_TTL_LEDGERS: u32 = JAR_TTL_LEDGERS;

/// Threshold below which the instance TTL is extended.
///
/// Matches `JAR_TTL_THRESHOLD`: extend when half the lifetime remains so active
/// contracts don't pay the write cost on every call.
const INSTANCE_TTL_THRESHOLD: u32 = JAR_TTL_THRESHOLD;

/// The public interface this deployment speaks.
///
/// `tip` has already changed shape twice (three event elements, then four) and
/// `jar_xfer` went from an address to a tuple, all under the same function
/// names. A client that is only given a contract id had no way to tell which
/// build it was talking to, so it discovered the mismatch when decoding failed
/// — or, worse, silently ignored new data. This view lets it ask first.
///
/// Bump by one whenever the public interface changes: a function is added or
/// removed, an error code is added or renumbered, or an event's shape changes.
/// Do not bump for internal refactors, new storage keys, or doc edits — none of
/// those are visible to a consumer.
///
/// This is a per-contract number for client feature detection. It complements
/// the per-event schema version proposed in #83 rather than replacing it.
const INTERFACE_VERSION: u32 = 4;

/// One recipient and the share of every tip they receive, in basis points.
#[contracttype]
#[derive(Clone)]
pub struct Split {
    /// Recipient that receives this share of every tip.
    pub to: Address,
    /// Share of each tip in basis points, out of 10 000; a jar's splits sum to exactly 10 000.
    pub bps: u32,
}

/// One jar to tip inside a [`TipSplitter::tip_many`] batch: the slug, the
/// amount, and the message that rides along with that jar's `tip` event.
///
/// Fields are named rather than positional so the batch decodes
/// forward-compatibly, the same reason the event payloads name theirs. A
/// `TipRequest` is exactly the per-jar argument list of [`TipSplitter::tip`],
/// so one entry of a batch settles the same way a single call would.
#[contracttype]
#[derive(Clone)]
pub struct TipRequest {
    /// Slug of the jar to tip, e.g. `"@alice"`.
    pub jar_id: String,
    /// Amount of the default token to tip this jar.
    pub amount: i128,
    /// Message published in this jar's `tip` event.
    pub message: String,
}

/// Compact on-chain representation used by storage migrations. Tuple values
/// have no repeated field labels, while the public `Split` remains readable
/// for clients and events.
#[contracttype]
#[derive(Clone)]
pub struct CompactSplit(pub Address, pub u32);

/// A creator's tip jar: who controls it and how tips are split.
#[contracttype]
#[derive(Clone)]
pub struct Jar {
    /// Address that may change the jar — its splits, ownership, and minimum tip amount.
    pub owner: Address,
    /// The recipients a tip is split across, and the share each one receives.
    pub splits: Vec<Split>,
    /// When `true`, `update_splits` may not reduce or remove any current
    /// recipient without that recipient's own authorisation (see #173). Set by
    /// `lock_splits`, cleared by `unlock_splits`; survives
    /// `transfer_jar_ownership`.
    pub locked: bool,
}

/// Versioned storage envelope. New fields must be added as a new variant so
/// older jars remain decodable during a rolling deployment.
#[contracttype]
#[derive(Clone)]
pub enum JarRecord {
    V1(Jar),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewSplit {
    pub to: Address,
    pub amount: i128,
}

/// A jar's settled tip totals, kept on-chain next to the jar.
///
/// Every figure a creator sees normally comes from the backend indexer's
/// database, which has known blind spots: a ledger that returns a full page of
/// events is skipped by `planBatch` in `src/indexer/cursor.ts`, and an event
/// older than the RPC's retention window can never be backfilled. A missed tip
/// silently lowers a creator's totals and nothing on-chain contradicts it.
/// These counters are the authoritative figure any client can read and compare
/// with its own sum, with no dependence on Novatip's backend.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarStats {
    /// How many tips have settled to this jar.
    pub tip_count: u64,
    /// The cumulative amount of those tips.
    ///
    /// A raw, unitless sum, exactly like the minimum in `set_min_tip_amount`:
    /// a tip paid in a non-default token is counted in that token's smallest
    /// unit, so a jar tipped in several assets has a total that mixes units.
    /// A consumer must not render this as a currency figure unless it knows
    /// the jar has only ever been tipped in one asset.
    pub total: i128,
}

/// One supporter's settled tip totals against one jar.
///
/// This is the on-chain data the roadmap's `supporter_badge` contract needs:
/// milestones are phrased as "10 tips or $10 cumulative", and a badge contract
/// cannot read the backend's `Tip` row. Before this existed a badge could only
/// be minted by a trusted off-chain service asserting the milestone.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupporterStats {
    /// How many tips this address has settled to this jar.
    pub tip_count: u64,
    /// The cumulative amount of those tips; same raw-unit caveat as
    /// [`JarStats::total`].
    pub total: i128,
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
    /// Most jars one `tip_many` call may tip.
    pub max_batch_tips: u32,
    /// Longest accepted jar id, in UTF-8 bytes: the leading `@` plus the
    /// 3–32-character canonical body. `create_jar` and `rename_jar` reject a
    /// longer id with `InvalidJarId`. This is the effective input bound; the
    /// looser 64-byte storage cap (`MAX_JAR_ID_LEN`) is unreachable while the
    /// canonical rule holds.
    pub max_jar_id_len: u32,
}

/// A supporter's standing intention to tip a jar on a schedule.
///
/// This is the on-chain *record* of a recurring tip plus the rail that makes it
/// collectable. The supporter approves this contract as a spender for at least
/// `amount` per payment; any keeper may then call [`TipSplitter::collect_recurring`]
/// once the entry's `next_due_ledger` is reached, and the payment splits
/// identically to a manual `tip` of the same amount. The supporter keeps full
/// control: revoking the allowance (or `cancel_recurring`) stops collections
/// dead, and every collection is capped by the allowance the supporter granted.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recurring {
    /// The supporter who registered the schedule, funds each collection from
    /// their own allowance, and may cancel it.
    pub supporter: Address,
    /// The jar every payment is routed to.
    pub jar_id: String,
    /// Amount of each payment, in the jar's token. Validated against the same
    /// checks `tip` applies at registration time.
    pub amount: i128,
    /// Cadence, in ledgers, between payments. Must be non-zero.
    pub period_ledgers: u32,
    /// Message that rides along with each payment.
    pub message: String,
    /// First ledger at which a payment is due: the creation ledger plus
    /// `period_ledgers`.
    pub next_due_ledger: u32,
}

#[contracttype(export = false)]
pub enum DataKey {
    /// Contract admin (deployer); reserved for future migrations.
    Admin,
    /// Address of the USDC Stellar Asset Contract used for all tips.
    Token,
    /// A tip jar keyed by its public slug, e.g. "@alice".
    Jar(String),
    /// Optional minimum tip amount for a jar keyed by its public slug.
    MinTip(String),
    /// A recipient's undeliverable share, held by the contract until they can
    /// receive it again (see #171). Keyed by the recipient address; value is
    /// the amount owed them in the jar token.
    Held(Address),
    /// A recurring-tip schedule keyed by its assigned id.
    Recurring(u128),
    /// Monotonic counter of issued recurring-tip ids.
    RecurringCount,
    /// A jar's settled tip totals, under the same slug as the jar itself.
    Stats(String),
    /// One supporter's settled totals, keyed by the `(jar_id, supporter)` pair
    /// the entry describes.
    Supporter(String, Address),
    /// An address in the tip allowlist, keyed by the token itself. Presence is
    /// the whole answer, so `is_token_allowed` is a `has` check.
    ///
    /// Instance rather than persistent on purpose: an allowlist entry is
    /// contract configuration, like the `Admin` and `Token` keys beside it. A
    /// persistent entry nobody bumps is archived silently, which would surface
    /// as "this token suddenly stopped working" with no admin having removed
    /// it — the one failure mode an allowlist must not have.
    AllowedToken(Address),
    /// Contract-wide count of successful tips, used as each tip's sequence
    /// number. Held in instance storage: one `u64`, incremented once per tip,
    /// independent of how many jars exist.
    TipCount,
    /// Optional post-tip hook contract, set and cleared by the admin (#176).
    /// Instance storage, like the other configuration keys beside it.
    TipHook,
    /// Optional registrar contract whose co-signature `create_jar` requires
    /// while it is set (#178). Instance storage; absence means jar creation
    /// stays exactly as it was: owner auth only.
    Registrar,
    /// Contract-wide pause flag. When present in instance storage, tipping is disabled.
    Paused,
    /// Protocol fee taken from every tip before the split, in basis points
    /// (#79). Instance storage, like the other configuration keys; absence
    /// means 0, so an upgraded deployment charges nothing until the admin
    /// opts in with `set_fee_bps`.
    FeeBps,
}

/// Public error codes. The numbers are part of the client interface —
/// `novatip-sdk` and the frontend map them to user-facing messages — so they
/// are pinned by `error_codes_are_pinned_to_the_public_interface` in
/// `src/test.rs`. Append new variants; never renumber an existing one.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// The contract has not been initialized.
    NotInitialized = 1,
    /// The requested jar id is already registered.
    JarExists = 2,
    /// The requested jar id is not registered.
    JarNotFound = 3,
    /// Retired error; use `SplitsEmpty`, `SplitOutOfRange` or `SplitSumNot100Pct`.
    InvalidSplits = 4,
    /// The amount is non-positive, too small to split, or would overflow.
    InvalidAmount = 5,
    /// The split list exceeds the recipient limit.
    TooManyRecipients = 6,
    /// The split list contains the same recipient more than once.
    DuplicateRecipient = 7,
    /// The tip message exceeds the byte-length limit.
    MessageTooLong = 8,
    /// The jar id is not a canonical `@`-prefixed slug: empty, longer than
    /// the limit, or containing bytes outside `[a-z0-9_-]`.
    InvalidJarId = 9,
    /// Splits list is empty.
    SplitsEmpty = 10,
    /// A split has a basis-point share that is zero or above 100 %.
    SplitOutOfRange = 11,
    /// Splits sum to something other than 10 000 bps.
    SplitSumNot100Pct = 12,
    /// Tip amount is below the jar's configured minimum tip amount.
    BelowMinTipAmount = 13,
    /// `leave_jar` was called for an address that is not in the jar's splits.
    NotRecipient = 14,
    /// `leave_jar` would empty the jar: the caller is its only recipient.
    SoleRecipient = 15,
    /// `cancel_recurring`/`get_recurring` named an id that does not exist.
    RecurringNotFound = 16,
    /// A recurring tip was registered with a zero payment period.
    InvalidPeriod = 17,
    /// `withdraw_held` was called by a recipient the contract owes nothing.
    NothingHeld = 18,
    /// `tip_in_token` was given a token the admin has not put on the
    /// allowlist. Checked before any transfer, so a rejection never moves
    /// funds.
    TokenNotAllowed = 19,
    /// `disallow_token` was given the deploy-time default token. `tip` always
    /// pays through that address, so removing it would leave a working `tip`
    /// path for a token the allowlist reports as closed.
    CannotDisallowDefault = 20,
    /// The running tip totals would not fit their fields. Unreachable in
    /// practice — `u64` tip counts and `i128` sums cannot realistically be
    /// exhausted by tips under `MAX_RECIPIENTS` — but the counters are bumped
    /// with checked arithmetic so the failure is typed rather than an opaque
    /// wasm trap, per #167.
    StatsOverflow = 21,
    /// Tip amount exceeds the sender's token balance. Checked before any
    /// transfer so a rejection is a typed error, not a partial payout.
    InsufficientBalance = 22,
    /// `collect_recurring` was called before the schedule's next due ledger.
    /// The entry is left untouched — an early call is a typed no-op, not a
    /// cancellation.
    NotDue = 23,
    /// `collect_recurring` found the supporter's allowance for this contract
    /// too small to cover the scheduled amount. Checked before any transfer.
    InsufficientAllowance = 24,
    /// The caller supplied zero where a positive amount is required.
    AmountZero = 25,
    /// The caller supplied a negative amount.
    AmountNegative = 26,
    /// The amount cannot produce a non-zero share for every recipient.
    SplitAmountTooSmall = 27,
    /// The amount multiplication exceeded the contract's integer range.
    AmountOverflow = 28,
    /// A configured minimum tip must be positive.
    InvalidMinimumAmount = 29,
    /// A tip where every transfer was skipped moved no funds.
    NoFundsMoved = 30,
    /// A split recipient or jar owner is the contract itself or its token.
    InvalidRecipient = 31,
    /// `transfer_admin` was called with the same address as the current admin.
    AdminExists = 32,
    /// An operation was attempted while the contract is paused.
    ContractPaused = 33,
    /// `rename_jar` was given a jar id that is already in use.
    JarIdInUse = 34,
    /// `set_fee_bps` was given a fee above `MAX_FEE_BPS` (500 bps, 5%).
    FeeTooHigh = 35,
    /// The message is not valid UTF-8 or contains a control character other
    /// than newline. Checked before any funds move (#96).
    InvalidMessage = 36,
    /// `tip_many` was given a batch with no entries in it.
    BatchEmpty = 37,
    /// `tip_many` was given more entries than `MAX_BATCH_TIPS`.
    BatchTooLarge = 38,
}

/// The interface a post-tip hook contract must implement (#176).
///
/// `tip`/`tip_in_token`/`collect_recurring` call `on_tip` through
/// `env.try_invoke_contract` *after* every payout and the `tip` event have
/// been finalised, so a hook that panics cannot revert a successful tip: the
/// panic is caught and discarded and the tip still settles. One caveat is
/// inherent to the Soroban budget: if the hook burns the transaction's
/// remaining instructions, the host aborts the whole footprint and the tip
/// fails too — `try_invoke_contract` can catch a `panic!`, not an
/// out-of-gas. Hooks are therefore opt-in, admin-controlled integrations, and
/// an operator should point the hook at a contract they have audited.
#[contractclient(name = "TipHookClient")]
pub trait TipHook {
    /// Called after a tip of `amount` into `jar_id` from `from` has been paid
    /// and announced. Return value is ignored; authorisation is not requested.
    fn on_tip(env: Env, jar_id: String, from: Address, amount: i128);
}

#[contract]
pub struct TipSplitter;

/// Data published by `jar_crtd`; fields are named for forward-compatible decoding.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarCreatedEvent {
    pub version: u32,
    pub owner: Address,
}

/// Data published by `splits`; fields are named for forward-compatible decoding.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitsUpdatedEvent {
    pub version: u32,
    pub count: u32,
}

/// Data published by `jar_xfer`; fields are named for forward-compatible decoding.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarTransferredEvent {
    pub version: u32,
    pub previous_owner: Address,
    pub new_owner: Address,
}

/// Data published by `min_tip`; fields are named for forward-compatible decoding.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinTipEvent {
    pub version: u32,
    pub amount: Option<i128>,
}

/// Data published by `tip`; fields are named for forward-compatible decoding.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TipEvent {
    pub version: u32,
    pub from: Address,
    /// The gross tip, before the protocol fee.
    pub amount: i128,
    /// The protocol fee this tip moved to the admin (#79); `0` when no fee is
    /// configured. `fee` plus the breakdown's `moved` values, plus the share
    /// of a tipper who is also a recipient, equals `amount` exactly.
    pub fee: i128,
    pub message: String,
    pub breakdown: Vec<(Address, i128, bool)>,
    pub token: Address,
    pub seq: u64,
    pub reference: Option<BytesN<32>>,
}

/// Data published when the contract is upgraded (#102).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradedEvent {
    pub admin: Address,
    pub new_hash: BytesN<32>,
}

#[contractimpl]
impl TipSplitter {
    /// Runs once at deploy time. `token` is the USDC Stellar Asset Contract id.
    ///
    /// The deploy-time token is also the first entry in the tip allowlist, so a
    /// deployment is usable through `tip_in_token` immediately and
    /// `get_token`/`is_token_allowed` can never disagree about it.
    pub fn __constructor(env: Env, admin: Address, token: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::AllowedToken(token), &true);
    }

    /// The contract admin, which every admin-gated entry point requires.
    /// Returns the address so the caller can name it in an event.
    fn require_admin(env: &Env) -> Address {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
        admin.require_auth();
        admin
    }

    /// Add `token` to the allowlist `tip_in_token` accepts. Admin-only.
    ///
    /// Idempotent: allowing an already-allowed token succeeds and re-emits,
    /// because the operation is a config assertion rather than a state
    /// transition, and a deployment script that retries should not fail.
    /// Emits a `tk_allow` event carrying the token and the admin who authorised
    /// it, so an index of accepted assets can show provenance.
    pub fn allow_token(env: Env, token: Address) {
        let admin = Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&DataKey::AllowedToken(token.clone()), &true);
        env.events()
            .publish((symbol_short!("tk_allow"), token), admin);
    }

    /// Remove `token` from the allowlist. Admin-only.
    ///
    /// The deploy-time default token cannot be removed: `tip` pays through it
    /// unconditionally, so allowing its removal would leave a functioning tip
    /// path for a token the allowlist reports as closed, and `get_token` would
    /// contradict `is_token_allowed`. Rejected with `CannotDisallowDefault`.
    ///
    /// Emits a `tk_dis` event carrying the token and the admin. Existing tips
    /// are untouched — this only stops future tips in that asset.
    pub fn disallow_token(env: Env, token: Address) {
        let admin = Self::require_admin(&env);
        let default: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if token == default {
            panic_with_error!(&env, Error::CannotDisallowDefault);
        }
        env.storage()
            .instance()
            .remove(&DataKey::AllowedToken(token.clone()));
        env.events()
            .publish((symbol_short!("tk_dis"), token), admin);
    }

    /// Whether `jar_id` is a canonical slug: a leading `@` followed by
    /// 3–32 lower-case ASCII letters, digits, `_` or `-`.
    ///
    /// The contract is the only place that can enforce one shape for every
    /// client. Without it, `@alice`, `@Alice` and `@alice ` are three distinct
    /// jars, and the lookalikes are indistinguishable wherever a raw id is
    /// rendered. The backend's `SLUG_REGEX` is the same rule, so rejecting
    /// anything else cannot lock out a jar a tip page could create.
    ///
    /// Copies at most 33 bytes into a stack buffer; no allocation, so it is
    /// cheap enough to run before the storage lookup.
    fn is_valid_jar_id(jar_id: &String) -> bool {
        let len = jar_id.len();
        if len > MAX_JAR_ID_LEN || !(MIN_JAR_SLUG_LEN + 1..=MAX_JAR_SLUG_LEN + 1).contains(&len) {
            return false;
        }
        let mut buf = [0u8; (MAX_JAR_SLUG_LEN + 1) as usize];
        jar_id.copy_into_slice(&mut buf[..len as usize]);
        if buf[0] != b'@' {
            return false;
        }
        for byte in &buf[1..len as usize] {
            if !(byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || *byte == b'_'
                || *byte == b'-')
            {
                return false;
            }
        }
        true
    }

    /// Register a new tip jar. `owner` must authorize. `jar_id` must be a
    /// canonical slug — a leading `@` followed by 3–32 lower-case ASCII
    /// letters, digits, `_` or `-` — and at most `MAX_JAR_ID_LEN` bytes.
    /// Splits must sum to 100% and may not name the same recipient twice.
    ///
    /// When the admin has named a registrar (#178), creating a jar
    /// additionally requires that contract to co-sign the exact
    /// `(jar_id, owner)` pair being registered — see [`TipSplitter::set_registrar`]
    /// for what that gate is for and what it deliberately does not promise.
    ///
    /// Emits a `jar_crtd` event so indexers can discover all jars from the
    /// event log without any on-chain list.
    pub fn create_jar(env: Env, owner: Address, jar_id: String, splits: Vec<Split>) {
        owner.require_auth();
        if !Self::is_valid_jar_id(&jar_id) {
            panic_with_error!(&env, Error::InvalidJarId);
        }
        // Issue #94: reject the contract itself or its token as jar owner.
        let contract_addr = env.current_contract_address();
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if owner == contract_addr || owner == token_addr {
            panic_with_error!(&env, Error::InvalidRecipient);
        }
        let key = DataKey::Jar(jar_id.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::JarExists);
        }
        Self::validate_splits(&env, &splits);
        // Runs after the validation above: a malformed request never burns a
        // co-signature, and the args co-signed are exactly the values stored.
        if let Some(registrar) = Self::get_registrar(env.clone()) {
            registrar.require_auth_for_args((jar_id.clone(), owner.clone()).into_val(&env));
        }
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: owner.clone(),
                splits,
                locked: false,
            },
        );

        // Issue #91: extend instance TTL on jar creation so the contract stays
        // alive alongside its jars.
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        env.events().publish(
            (symbol_short!("jar_crtd"), jar_id),
            JarCreatedEvent { version: 1, owner },
        );
    }

    /// Update an existing jar's splits. Only the jar owner may do this.
    ///
    /// On a **locked** jar (#173) the owner's signature alone is not enough:
    /// every current recipient whose share would shrink or who would be dropped
    /// by the new list must also authorize the call. Recipients whose share is
    /// unchanged or increased, and brand-new recipients, are not required to
    /// sign — the lock only protects against a collaborator being cut out or
    /// reduced without their consent.
    ///
    /// Emits a `splits` event so indexers caching a jar's splits know to
    /// refetch them.
    pub fn update_splits(env: Env, jar_id: String, splits: Vec<Split>) {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        // Issue #92: extend both jar and min-tip in lockstep.
        Self::extend_jar_and_min_ttl(&env, &jar_id);
        jar.owner.require_auth();
        Self::validate_splits(&env, &splits);

        // Skip if the splits haven't changed (issue #100).
        if jar.splits == splits {
            return;
        }

        if jar.locked {
            // Consent rule: a current recipient must sign iff their share
            // decreases or they are removed. Compare each old entry against its
            // new share (0 when the address is absent from the new list).
            let old = jar.splits.clone();
            for i in 0..old.len() {
                let prev = old.get(i).unwrap();
                let mut new_bps = 0u32;
                for j in 0..splits.len() {
                    let cand = splits.get(j).unwrap();
                    if cand.to == prev.to {
                        new_bps = cand.bps;
                        break;
                    }
                }
                if new_bps < prev.bps {
                    prev.to.require_auth();
                }
            }
        }

        let split_count = splits.len();
        let key = DataKey::Jar(jar_id.clone());
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: jar.owner,
                splits,
                locked: jar.locked,
            },
        );

        env.events().publish(
            (symbol_short!("splits"), jar_id),
            SplitsUpdatedEvent {
                version: 1,
                count: split_count,
            },
        );
    }

    /// Lock a jar's splits so the owner can no longer reduce or remove a
    /// recipient without that recipient's consent (#173). Owner-only.
    ///
    /// A band member or co-host promoting a shared tip link gets an on-chain
    /// assurance that their share cannot be cut behind their back. The lock is
    /// a property of the jar, not the owner, so it survives
    /// `transfer_jar_ownership`: handing the jar to a new owner does not
    /// silently disarm the protection the collaborators agreed to.
    ///
    /// Publishing on a jar that is already locked is a no-op on state but still
    /// emits a `lock` event, so an indexer stays in sync with the intent.
    pub fn lock_splits(env: Env, jar_id: String) {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        // Issue #92: extend both jar and min-tip in lockstep.
        Self::extend_jar_and_min_ttl(&env, &jar_id);
        jar.owner.require_auth();

        let key = DataKey::Jar(jar_id.clone());
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: jar.owner,
                splits: jar.splits,
                locked: true,
            },
        );

        env.events().publish((symbol_short!("lock"), jar_id), true);
    }

    /// Unlock a jar's splits, restoring the owner's unilateral control.
    ///
    /// Consent mirrors a reduction: because unlocking lets the owner then cut
    /// any collaborator, it requires authorization from **every** current
    /// recipient, not just the owner. Otherwise an owner could disarm the lock
    /// first and reduce shares a moment later, which is exactly the bypass the
    /// lock exists to prevent.
    ///
    /// Owner-only if there are no consent-gating recipients is not possible: a
    /// stored jar always has at least one recipient, so the owner's own
    /// signature alone never unlocks a locked jar that lists other collaborators.
    pub fn unlock_splits(env: Env, jar_id: String) {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        // Issue #92: extend both jar and min-tip in lockstep.
        Self::extend_jar_and_min_ttl(&env, &jar_id);
        jar.owner.require_auth();

        // All current recipients must consent to disarm the protection.
        let splits = jar.splits.clone();
        for i in 0..splits.len() {
            splits.get(i).unwrap().to.require_auth();
        }

        env.storage().persistent().set(
            &key,
            &Jar {
                owner: jar.owner,
                splits,
                locked: false,
            },
        );

        env.events().publish((symbol_short!("lock"), jar_id), false);
    }

    /// Whether a jar's splits are locked against unilateral owner edits (#173).
    ///
    /// Panics with `JarNotFound` if the slug is not registered, matching
    /// `get_jar`.
    pub fn is_splits_locked(env: Env, jar_id: String) -> bool {
        let key = DataKey::Jar(jar_id);
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        jar.locked
    }

    /// Let a recipient remove themselves from a jar (#172), authorized by that
    /// recipient rather than the owner.
    ///
    /// A collaborator listed in someone else's jar previously had no say: they
    /// could not leave a project they had quit, stop receiving payments they
    /// did not want attributed to them, or escape the case where their own
    /// account can no longer receive the token. Their basis points are
    /// reassigned by a documented rule so the remaining splits still sum to
    /// 10 000: **the departing share is added to the last remaining entry**,
    /// which already absorbs rounding dust elsewhere in the contract.
    ///
    /// Rejections:
    /// - the caller is not in the splits — `NotRecipient`;
    /// - the caller is the only recipient (leaving would empty the jar) —
    ///   `SoleRecipient`.
    ///
    /// Ownership is untouched: the owner stays the owner. Emits the same
    /// `splits` event as `update_splits` so indexers refresh their copy.
    pub fn leave_jar(env: Env, jar_id: String, recipient: Address) {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        recipient.require_auth();

        let old = jar.splits.clone();
        let n = old.len();
        if n == 1 {
            panic_with_error!(&env, Error::SoleRecipient);
        }
        let mut found = false;
        let mut departed_bps: u32 = 0;
        let mut remaining: Vec<Split> = Vec::new(&env);
        for i in 0..n {
            let split = old.get(i).unwrap();
            if split.to == recipient {
                found = true;
                departed_bps = split.bps;
            } else {
                remaining.push_back(split);
            }
        }
        if !found {
            panic_with_error!(&env, Error::NotRecipient);
        }

        // Documented rule: the last remaining entry absorbs the departed share.
        let last = remaining.len() - 1;
        let mut last_split = remaining.get(last).unwrap();
        // `remaining` was built from validated splits (each bps within bounds
        // and summing to 10 000); after removing one entry and adding its `bps`
        // to the last, the total is unchanged at 10 000 and no single entry can
        // exceed 10 000 (all others are >= 1). `checked_add` keeps any
        // unforeseen overflow a typed error rather than a trap.
        last_split.bps = last_split
            .bps
            .checked_add(departed_bps)
            .unwrap_or_else(|| panic_with_error!(&env, Error::SplitSumNot100Pct));
        remaining.set(last, last_split);

        Self::validate_splits(&env, &remaining);
        let split_count = remaining.len();
        let key = DataKey::Jar(jar_id.clone());
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: jar.owner,
                splits: remaining,
                locked: jar.locked,
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
        // Issue #92: extend both jar and min-tip in lockstep.
        Self::extend_jar_and_min_ttl(&env, &jar_id);
        jar.owner.require_auth();
        // Issue #94: reject the contract itself or its token as new owner.
        let contract_addr = env.current_contract_address();
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if new_owner == contract_addr || new_owner == token_addr {
            panic_with_error!(&env, Error::InvalidRecipient);
        }
        let prev_owner = jar.owner;
        env.storage().persistent().set(
            &key,
            &Jar {
                owner: new_owner.clone(),
                splits: jar.splits,
                // The lock is a promise to the collaborators, not a privilege
                // of the owner, so it survives the transfer (#173).
                locked: jar.locked,
            },
        );

        // Both ends of the move, so the event is meaningful on its own. With
        // only the new owner, an indexer starting from a partial history can
        // see where a jar went but not where it came from, unless it has
        // already replayed every prior event for that jar.
        env.events().publish(
            (symbol_short!("jar_xfer"), jar_id),
            JarTransferredEvent {
                version: 1,
                previous_owner: prev_owner,
                new_owner,
            },
        );
    }

    /// Transfer admin control to a new address. Only the current admin may do this.
    /// Rejects if the new admin is the same as the current one. Emits an event
    /// carrying both addresses so indexers can track the handover.
    pub fn transfer_admin(env: Env, new_admin: Address) {
        let current_admin = Self::require_admin(&env);
        if current_admin == new_admin {
            panic_with_error!(&env, Error::AdminExists);
        }
        env.storage()
            .instance()
            .set(&DataKey::Admin, &new_admin.clone());
        env.events()
            .publish((symbol_short!("adm_xfer"),), (current_admin, new_admin));
    }

    /// Pause all tipping operations. Only the admin may call this.
    /// Once paused, `tip`, `tip_with_ref`, and `tip_in_token` will reject with
    /// `ContractPaused` until the admin calls `resume`. Emits a pause event.
    pub fn pause(env: Env) {
        let admin = Self::require_admin(&env);
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events().publish((symbol_short!("pause"),), admin);
    }

    /// Resume tipping operations after a pause. Only the admin may call this.
    /// Removes the pause flag from storage and emits a resume event.
    pub fn resume(env: Env) {
        let admin = Self::require_admin(&env);
        env.storage().instance().remove(&DataKey::Paused);
        env.events().publish((symbol_short!("resume"),), admin);
    }

    /// Check whether the contract is currently paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().has(&DataKey::Paused)
    }

    /// Upgrade the contract to a new WASM hash. Admin-only (#102).
    ///
    /// This allows the deployer to replace the contract code without changing
    /// its address or losing any stored state (jars, tips, recurring schedules).
    /// Only the current admin may authorize this call. Emits an `upgrade` event
    /// carrying the new hash so indexers can track which code version handled
    /// each tip.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin = Self::require_admin(&env);

        // Deployer's update_current_contract_wasm replaces the running WASM
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());

        env.events().publish(
            (symbol_short!("upgrade"),),
            UpgradedEvent {
                admin,
                new_hash: new_wasm_hash,
            },
        );
    }

    /// Rename a jar by moving it to a new id. Only the jar owner may do this.
    /// The old id stops resolving immediately, and the new id must not already be in use.
    /// Emits a `jar_rnm` event carrying both ids for indexer tracking.
    pub fn rename_jar(env: Env, jar_id: String, new_jar_id: String) {
        if !Self::is_valid_jar_id(&new_jar_id) {
            panic_with_error!(&env, Error::InvalidJarId);
        }
        let old_key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&old_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&old_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        jar.owner.require_auth();

        // Check if new id is already in use
        let new_key = DataKey::Jar(new_jar_id.clone());
        if env.storage().persistent().has(&new_key) {
            panic_with_error!(&env, Error::JarIdInUse);
        }

        // Copy jar to new id
        env.storage().persistent().set(&new_key, &jar);
        env.storage()
            .persistent()
            .extend_ttl(&new_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        // Delete old id
        env.storage().persistent().remove(&old_key);

        // Move stats too
        let old_stats_key = DataKey::Stats(jar_id.clone());
        let new_stats_key = DataKey::Stats(new_jar_id.clone());
        if let Some(stats) = env
            .storage()
            .persistent()
            .get::<_, JarStats>(&old_stats_key)
        {
            env.storage().persistent().set(&new_stats_key, &stats);
            env.storage().persistent().extend_ttl(
                &new_stats_key,
                JAR_TTL_THRESHOLD,
                JAR_TTL_LEDGERS,
            );
            env.storage().persistent().remove(&old_stats_key);
        }

        // Move min tip if present
        let old_min_key = DataKey::MinTip(jar_id.clone());
        let new_min_key = DataKey::MinTip(new_jar_id.clone());
        if let Some(min_amount) = env.storage().persistent().get::<_, i128>(&old_min_key) {
            env.storage().persistent().set(&new_min_key, &min_amount);
            env.storage()
                .persistent()
                .extend_ttl(&new_min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
            env.storage().persistent().remove(&old_min_key);
        }

        env.events()
            .publish((symbol_short!("jar_rnm"),), (jar_id, new_jar_id));
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
                    panic_with_error!(&env, Error::InvalidMinimumAmount);
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

        env.events().publish(
            (symbol_short!("min_tip"), jar_id),
            MinTipEvent {
                version: 1,
                amount: min_amount,
            },
        );
    }

    /// Send a tip of `amount` of the deploy-time default token from `from`,
    /// split across the jar's recipients, and emit a `("tip", jar_id)` event.
    ///
    /// A recipient who cannot receive the token no longer reverts the whole tip:
    /// their share is escrowed in the contract (credit under `DataKey::Held`)
    /// and marked `held` in the breakdown, while everyone else is paid normally.
    /// `withdraw_held` releases it once the recipient can receive again (#171).
    ///
    /// `message` may be at most `MAX_MESSAGE_LEN` bytes, must be valid UTF-8,
    /// and may not contain control characters other than newline; it is
    /// rejected before any funds move.
    ///
    /// Returns the tip's sequence number, so the supporter's own client learns
    /// which tip it just sent without waiting for the event log. See
    /// [`Self::tip_with_ref`] for attaching a client reference, and
    /// [`Self::get_tip_count`] to read the running total.
    pub fn tip(env: Env, from: Address, jar_id: String, amount: i128, message: String) -> u64 {
        Self::tip_inner(env, from, jar_id, amount, message, None)
    }

    /// Send a tip carrying an optional 32-byte client `reference`.
    ///
    /// An integration that starts a tip off-chain (a pending alert, a bot
    /// command, a checkout session) needs to recognise that tip when it comes
    /// back through the indexer or a webhook. Sender, amount, and the free-text
    /// message are none of them unique, and the message is visible to the
    /// creator so it cannot carry an opaque id. `reference` is that id: the
    /// contract publishes it verbatim in the `tip` event and stores nothing.
    ///
    /// The value is opaque by design — not persisted, not checked for
    /// uniqueness, not interpreted. Passing `None` costs nothing, and a
    /// collision between two integrations' references is the consumers' problem
    /// to resolve, not the contract's.
    ///
    /// Identical to [`tip`] in every other respect: same transfers, same
    /// validation, same sequence numbering. Both entry points call one internal
    /// implementation, so they cannot drift apart.
    pub fn tip_with_ref(
        env: Env,
        from: Address,
        jar_id: String,
        amount: i128,
        message: String,
        reference: Option<BytesN<32>>,
    ) -> u64 {
        Self::tip_inner(env, from, jar_id, amount, message, reference)
    }

    /// Tip several jars in one transaction, from a single authorisation.
    ///
    /// Tipping two creators otherwise costs two transactions, two wallet
    /// approvals and two network fees — for micro-tips, a large fraction of the
    /// whole interaction. This batches them: `from` authorises once, and every
    /// jar in `tips` is settled in order, each exactly as a single
    /// [`Self::tip`] call would settle it.
    ///
    /// "Exactly" is literal. Every entry goes through the same private
    /// `settle_tip` the single-jar entry points share, so a batched jar gets the
    /// same validation, the same minimum-tip check, the same protocol fee, the
    /// same self-transfer skip, the same held-funds fallback, and its own `tip`
    /// event carrying its own sequence number. An indexer that already decodes a
    /// `tip` event needs no change whatsoever to follow a batch.
    ///
    /// The batch is bounded by `MAX_BATCH_TIPS` (`BatchTooLarge`) and an empty
    /// batch is `BatchEmpty`. It is **all-or-nothing**: Soroban rolls the whole
    /// transaction back if any entry panics, so a batch whose last jar is
    /// rejected leaves the earlier jars' balances and totals untouched, and no
    /// sequence number is consumed by the entries that would have settled.
    ///
    /// Returns one sequence number per entry, in the same order as `tips`.
    pub fn tip_many(env: Env, from: Address, tips: Vec<TipRequest>) -> Vec<u64> {
        // One signature covers the whole batch; no entry re-requires it.
        from.require_auth();
        if env.storage().instance().has(&DataKey::Paused) {
            panic_with_error!(&env, Error::ContractPaused);
        }
        if tips.is_empty() {
            panic_with_error!(&env, Error::BatchEmpty);
        }
        if tips.len() > MAX_BATCH_TIPS {
            panic_with_error!(&env, Error::BatchTooLarge);
        }
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        let mut seqs: Vec<u64> = Vec::new(&env);
        for i in 0..tips.len() {
            let req = tips.get(i).unwrap();
            let seq = Self::settle_tip(
                &env,
                &from,
                &req.jar_id,
                req.amount,
                &req.message,
                &token_addr,
                true,
                None,
            );
            seqs.push_back(seq);
        }
        seqs
    }

    /// The single transfer loop behind `tip` and `tip_with_ref`.
    ///
    /// Kept private and non-exposed so the two public entry points cannot
    /// disagree about validation, the order of transfers, the self-transfer
    /// skip, or the event shape.
    fn tip_inner(
        env: Env,
        from: Address,
        jar_id: String,
        amount: i128,
        message: String,
        reference: Option<BytesN<32>>,
    ) -> u64 {
        from.require_auth();
        if env.storage().instance().has(&DataKey::Paused) {
            panic_with_error!(&env, Error::ContractPaused);
        }
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        Self::settle_tip(
            &env,
            &from,
            &jar_id,
            amount,
            &message,
            &token_addr,
            true,
            reference,
        )
    }

    /// Send a tip of `amount` of `token`, for any token the admin has put on
    /// the allowlist.
    ///
    /// A supporter holding EURC previously had to swap into the default token
    /// before tipping; this lets them pay in the asset they hold. The
    /// allowlist is checked before any funds move, so an unlisted token is
    /// rejected with `TokenNotAllowed` rather than partially paying.
    ///
    /// The jar's minimum tip is **not** enforced here unless `token` is the
    /// default token. `set_min_tip_amount` stores a raw number with no unit,
    /// and one number cannot be a floor for several assets with different
    /// decimal counts — 500 is $0.005 of a 7-decimal asset and $0.0005 of a
    /// 6-decimal one. #170 allowed either "default token only" or "per-token";
    /// this takes the first, because per-token minimums would change
    /// `set_min_tip_amount`'s signature and the storage key, breaking jars that
    /// already configured one. The consequence is explicit: a jar can be tipped
    /// below its floor in another allowed asset, and a creator who cares
    /// should keep the allowlist to the token they set the minimum in.
    ///
    /// `compute_shares` needs no equivalent decision: it splits by basis points
    /// and never interprets the magnitude, so the same arithmetic is correct
    /// for any asset.
    ///
    /// Returns the tip's sequence number, exactly like [`Self::tip`]; the token
    /// the tip was paid in is carried in the `tip` event itself.
    pub fn tip_in_token(
        env: Env,
        from: Address,
        jar_id: String,
        token: Address,
        amount: i128,
        message: String,
    ) -> u64 {
        from.require_auth();
        if env.storage().instance().has(&DataKey::Paused) {
            panic_with_error!(&env, Error::ContractPaused);
        }
        if !env
            .storage()
            .instance()
            .has(&DataKey::AllowedToken(token.clone()))
        {
            panic_with_error!(&env, Error::TokenNotAllowed);
        }
        let default: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        Self::settle_tip(
            &env,
            &from,
            &jar_id,
            amount,
            &message,
            &token,
            token == default,
            None,
        )
    }

    /// The body every tip entry point shares: validate, split, transfer, bump
    /// the running totals, assign the sequence number, and emit the `tip`
    /// event.
    ///
    /// `from` must already have authorized. `enforce_min` says whether this
    /// call is in the token the jar's minimum tip is denominated in.
    ///
    /// Stats are written after the transfer loop. A transfer that traps (for
    /// instance a sender with an insufficient balance) reverts the whole call,
    /// so the counters never record a tip that did not settle — which is what
    /// makes them usable as an authoritative check on the indexer.
    ///
    /// The per-recipient amounts transferred here and the ones
    /// [`Self::preview_tip`] reports for the same sender come from one helper,
    /// [`Self::payout_amounts`], so the two can never drift apart (#177).
    #[allow(clippy::too_many_arguments)]
    fn settle_tip(
        env: &Env,
        from: &Address,
        jar_id: &String,
        amount: i128,
        message: &String,
        token_addr: &Address,
        enforce_min: bool,
        reference: Option<BytesN<32>>,
    ) -> u64 {
        if amount == 0 {
            panic_with_error!(env, Error::AmountZero);
        }
        if amount < 0 {
            panic_with_error!(env, Error::AmountNegative);
        }
        Self::validate_message(env, message);

        let jar_key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&jar_key)
            .unwrap_or_else(|| panic_with_error!(env, Error::JarNotFound));

        // Enforce optional per-jar minimum tip amount if configured
        let min_key = DataKey::MinTip(jar_id.clone());
        if enforce_min {
            if let Some(min_amount) = env.storage().persistent().get::<DataKey, i128>(&min_key) {
                if amount < min_amount {
                    panic_with_error!(env, Error::BelowMinTipAmount);
                }
                env.storage()
                    .persistent()
                    .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
            }
        }

        // Bump the jar's storage entry so a jar that is tipped regularly
        // is never archived. An idle jar survives roughly 58 days at the default
        // 5-second close rate (JAR_TTL_LEDGERS = 1_000_000 ledgers).
        env.storage()
            .persistent()
            .extend_ttl(&jar_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        // Issue #91: extend the contract instance TTL alongside the jar so the
        // contract itself doesn't expire while jars are still active. In
        // soroban-sdk 22 this also extends the Wasm code entry.
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        let client = token::Client::new(env, token_addr);
        let contract_addr = env.current_contract_address();

        // Fail fast with the contract's own code when the sender cannot cover
        // the full tip. Without this, the token's transfer failure propagates
        // with the SAC's own number (10/11/13), which collides with ours —
        // an empty wallet would surface as `SplitsEmpty`. Checked once, before
        // any transfer, so a failed tip never moves partial funds.
        if client.balance(from) < amount {
            panic_with_error!(env, Error::InsufficientBalance);
        }

        let n = jar.splits.len();
        // The protocol fee comes off the top (#79); the split, its
        // too-small-to-split guard and its dust rule all apply to what is
        // left. One shared calculation with `preview_split` and `preview_tip`,
        // so the number a client shows before signing is the number paid out
        // here.
        let (fee, net) = Self::split_fee(env, amount);
        let plan = Self::payout_amounts(env, &jar.splits, from, net);
        let fee_moved = match Self::fee_payee(env, from, fee) {
            Some(admin) => {
                client.transfer(from, &admin, &fee);
                fee
            }
            None => 0,
        };

        // The per-recipient breakdown published in the `tip` event, in split
        // order. Each entry is `(recipient, moved, held)`: `moved` is what left
        // the tipper for that recipient this call (0 for a tipper who is also
        // a recipient and keeps their share), and `held` marks a share the
        // recipient could not receive and the contract is now escrowing (#171).
        // Recording it as the transfers happen is what makes the event
        // self-describing: an indexer that recomputed the split arithmetic from
        // `get_jar` could get a different answer, because the jar's splits may
        // have changed between the tip and the read.
        let mut breakdown: Vec<(Address, i128, bool)> = Vec::new(env);
        let mut total_transferred: i128 = 0;
        for i in 0..n {
            let split = jar.splits.get(i).unwrap();
            let share = plan.get(i).unwrap();
            if share == 0 {
                // Nothing moved (the recipient is the tipper), so the breakdown
                // reports 0 rather than the notional share. The event is a
                // record of the transfers this call actually made, which is
                // what a balance-tracking indexer needs to stay in step with
                // the ledger.
                breakdown.push_back((split.to, 0, false));
                continue;
            }
            let delivered = matches!(client.try_transfer(from, &split.to, &share), Ok(Ok(_)));
            if delivered {
                total_transferred += share;
                breakdown.push_back((split.to, share, false));
            } else {
                // The recipient cannot receive the token (dropped trustline,
                // deauthorised account). Hold their share in the contract so
                // one collaborator's account state no longer reverts the whole
                // tip: the funds leave the tipper exactly as if the transfer
                // had succeeded, and `withdraw_held` pays them once the
                // recipient can receive again (#171).
                client.transfer(from, &contract_addr, &share);
                total_transferred += share;
                let held_key = DataKey::Held(split.to.clone());
                let held: i128 = env.storage().persistent().get(&held_key).unwrap_or(0);
                env.storage().persistent().set(&held_key, &(held + share));
                env.storage().persistent().extend_ttl(
                    &held_key,
                    JAR_TTL_THRESHOLD,
                    JAR_TTL_LEDGERS,
                );
                breakdown.push_back((split.to, share, true));
            }
        }

        // Issue #93: reject a tip where no funds actually moved. When every
        // recipient is the tipper themselves, all shares are skipped and the
        // event would claim a tip occurred at zero cost — forgeable and
        // misleading for indexers, leaderboards, and notifications.
        if total_transferred == 0 {
            panic_with_error!(env, Error::NoFundsMoved);
        }

        Self::bump_totals(env, jar_id, from, amount);

        // Assign the sequence number only now that every transfer has
        // succeeded. Soroban rolls the whole transaction back on a panic, so a
        // reverted tip consumes no storage either; incrementing after the loop
        // additionally means a tip that traps mid-split leaves the counter
        // where it was, so no number is ever skipped.
        //
        // A tip is the only writer of this key, so on a contract that has taken
        // at least one tip the entry is present. `unwrap_or(0)` covers the very
        // first tip, whose increment turns it into 1.
        let seq: u64 = env
            .storage()
            .instance()
            .get(&DataKey::TipCount)
            .unwrap_or(0u64)
            + 1;
        env.storage().instance().set(&DataKey::TipCount, &seq);

        env.events().publish(
            (symbol_short!("tip"), jar_id.clone()),
            TipEvent {
                version: 1,
                from: from.clone(),
                amount,
                fee: fee_moved,
                message: message.clone(),
                breakdown,
                token: token_addr.clone(),
                seq,
                reference,
            },
        );

        // Post-tip hook (#176): only now, after the funds and the event are
        // finalised, so a hook can never observe — or prevent — a tip that did
        // not settle.
        Self::call_tip_hook(env, jar_id, from, amount);

        seq
    }

    /// Register or clear the post-tip hook contract (#176). Admin-only.
    ///
    /// When set, every settled tip — `tip`, `tip_with_ref`, `tip_in_token` and
    /// `collect_recurring` — calls `on_tip(jar_id, from, amount)` on `hook`
    /// *after* the transfers and the `tip` event, through
    /// `env.try_invoke_contract`, so a hook that panics or errors cannot revert
    /// the tip: the failure is caught and discarded. The caveat that cannot be
    /// engineered away is the transaction budget: a hook that exhausts it
    /// aborts the whole transaction, tip included — `try_invoke_contract`
    /// catches a contract error, not an out-of-gas. See `docs/CONTRACT.md`.
    ///
    /// Passing `None` clears the hook and restores the pre-hook behaviour
    /// exactly. Every change emits a `tip_hook` event carrying the admin, so
    /// the configuration change is attributable in the event log.
    pub fn set_tip_hook(env: Env, hook: Option<Address>) {
        let admin = Self::require_admin(&env);
        match &hook {
            Some(h) => {
                env.storage().instance().set(&DataKey::TipHook, h);
            }
            None => {
                env.storage().instance().remove(&DataKey::TipHook);
            }
        }
        env.events()
            .publish((symbol_short!("tip_hook"), admin), hook);
    }

    /// The currently registered post-tip hook, or `None` when tips settle
    /// without one (#176).
    pub fn get_tip_hook(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::TipHook)
    }

    /// Fire the post-tip hook if one is registered (#176).
    ///
    /// Deliberately last in every settle path, and deliberately result-blind:
    /// whatever the hook does to its own state, the tip has already moved the
    /// funds and published its event. A failed invocation is discarded rather
    /// than propagated — the whole point is that a broken badge contract cannot
    /// stop a creator being paid.
    fn call_tip_hook(env: &Env, jar_id: &String, from: &Address, amount: i128) {
        let Some(hook): Option<Address> = env.storage().instance().get(&DataKey::TipHook) else {
            return;
        };
        let mut args: Vec<Val> = Vec::new(env);
        args.push_back(jar_id.clone().into_val(env));
        args.push_back(from.clone().into_val(env));
        args.push_back(amount.into_val(env));
        let _ = env.try_invoke_contract::<(), soroban_sdk::Error>(
            &hook,
            &symbol_short!("on_tip"),
            args,
        );
    }

    /// Set or clear the registrar whose co-signature `create_jar` requires
    /// (#178). Admin-only.
    ///
    /// Why: Novatip derives a jar's on-chain id from the claimed slug
    /// (`@alice` for `/alice`) and the backend guarantees uniqueness in *its*
    /// database, but the chain does not know about the claim — until Alice's
    /// onboarding finishes, anyone can call `create_jar("@alice", ...)` and
    /// strand her behind an "already registered on-chain" error. While a
    /// registrar is set, a slug can only be created for the owner the registrar
    /// approved: the backend signs the authorisation entry when a slug claim
    /// succeeds, and the creator's wallet submits the transaction as today.
    ///
    /// The trust model is deliberately narrow, and it is *not* solved here by
    /// code: the registrar is the same backend, so this gate is only as
    /// trustworthy as the backend's key management — it prevents race-style
    /// squatting by outsiders, while a compromised registrar key could register
    /// slugs for anyone (though *only* ones not yet registered; existing jars
    /// are untouched). The backend holding no keys by design is the open
    /// question #178 flags; this entry point makes the mechanism available and
    /// reversible — `None` restores today's permissionless behaviour exactly,
    /// so existing deployments and third-party users are unaffected until the
    /// admin opts in.
    ///
    /// Emits a `reg_set` event carrying the admin who made the change, so the
    /// gate's state is attributable in the event log.
    pub fn set_registrar(env: Env, registrar: Option<Address>) {
        let admin = Self::require_admin(&env);
        match &registrar {
            Some(r) => {
                env.storage().instance().set(&DataKey::Registrar, r);
            }
            None => {
                env.storage().instance().remove(&DataKey::Registrar);
            }
        }
        env.events()
            .publish((symbol_short!("reg_set"), admin), registrar);
    }

    /// The registrar whose co-signature `create_jar` currently requires, or
    /// `None` when jar creation is permissionless as before (#178).
    pub fn get_registrar(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Registrar)
    }

    /// Set the protocol fee taken from every tip, in basis points (#79).
    /// Admin-only.
    ///
    /// Every tip used to go entirely to the jar's recipients, leaving the
    /// platform no on-chain way to fund itself. A fee taken here is more
    /// honest than one taken off-chain: a tipper can read it with
    /// `get_fee_bps` before signing.
    ///
    /// The fee is `amount * fee_bps / 10_000`, truncated, and comes off the tip
    /// *before* the split. The too-small-to-split guard and the last
    /// recipient's dust rule therefore both apply to the post-fee amount, and
    /// the recipients' total plus the fee equals the tip exactly. The fee is
    /// paid to the contract admin.
    ///
    /// Defaults to 0, so no existing deployment starts charging on upgrade.
    /// Values above `MAX_FEE_BPS` (500, i.e. 5%) are rejected with
    /// `FeeTooHigh`, so the fee can never be set to swallow a tip. `0` turns
    /// the fee off again.
    ///
    /// Emits a `fee_set` event carrying the admin and the new value.
    pub fn set_fee_bps(env: Env, fee_bps: u32) {
        let admin = Self::require_admin(&env);
        if fee_bps > MAX_FEE_BPS {
            panic_with_error!(&env, Error::FeeTooHigh);
        }
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        env.events()
            .publish((symbol_short!("fee_set"), admin), fee_bps);
    }

    /// The protocol fee currently taken from every tip, in basis points (#79).
    /// `0` until the admin sets one.
    ///
    /// A tip form should show this before the supporter signs. The exact fee
    /// on a given amount is `amount - sum(preview_split(jar_id, amount))`,
    /// because every preview already reports post-fee shares.
    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0)
    }

    /// Split a gross tip into `(fee, net)` under the configured fee (#79).
    ///
    /// The fee truncates, so any rounding favours the tipper's recipients, and
    /// `fee + net == amount` always. With `fee_bps <= MAX_FEE_BPS` the net is
    /// at least 95% of the tip, so a positive amount always leaves a positive
    /// net for `compute_shares` to split.
    fn split_fee(env: &Env, amount: i128) -> (i128, i128) {
        let fee_bps = Self::get_fee_bps(env.clone());
        if fee_bps == 0 {
            return (0, amount);
        }
        let fee = amount
            .checked_mul(fee_bps as i128)
            .unwrap_or_else(|| panic_with_error!(env, Error::AmountOverflow))
            / BPS_DENOM as i128;
        (fee, amount - fee)
    }

    /// Who a fee of `fee` should be transferred to, or `None` when nothing
    /// moves: no fee, or the tipper is the admin. That second case is the
    /// self-transfer rule applied to the fee, and the `tip` event then
    /// reports a fee of 0 because nothing moved.
    fn fee_payee(env: &Env, from: &Address, fee: i128) -> Option<Address> {
        if fee == 0 {
            return None;
        }
        let admin = Self::get_admin(env.clone());
        if admin == *from {
            None
        } else {
            Some(admin)
        }
    }

    /// The number of tips this contract has taken, and the sequence number the
    /// next tip will receive.
    ///
    /// Every successful tip increments a contract-wide `u64` by one and the
    /// `tip` event carries that value, so a consumer can order tips and, more
    /// importantly, tell that it has seen all of them: an indexer that has
    /// stored numbers 1..N and reads N here has missed nothing. A gap means the
    /// event scan is incomplete, which the transaction hash cannot reveal
    /// because two tips in one batched transaction share it.
    ///
    /// Returns `0` on a fresh deploy. The counter is contract-wide, not
    /// per-jar.
    pub fn get_tip_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::TipCount)
            .unwrap_or(0u64)
    }

    /// Bump both running totals for a settled tip: the jar's aggregate and this
    /// supporter's against that jar.
    ///
    /// Two extra persistent writes per tip, which is real rent — see the
    /// measured budget delta in `docs/CONTRACT.md`. It buys a figure the
    /// indexer can be checked against, and the reason #167 and #168 share one
    /// helper is that they need the identical guarantee: both are updated only
    /// after the transfers succeeded, in the same call that paid them out.
    fn bump_totals(env: &Env, jar_id: &String, from: &Address, amount: i128) {
        let stats_key = DataKey::Stats(jar_id.clone());
        let jar_stats = env
            .storage()
            .persistent()
            .get::<DataKey, JarStats>(&stats_key)
            .map(|s| JarStats {
                tip_count: s
                    .tip_count
                    .checked_add(1)
                    .unwrap_or_else(|| panic_with_error!(env, Error::StatsOverflow)),
                total: s
                    .total
                    .checked_add(amount)
                    .unwrap_or_else(|| panic_with_error!(env, Error::StatsOverflow)),
            })
            .unwrap_or(JarStats {
                tip_count: 1,
                total: amount,
            });
        env.storage().persistent().set(&stats_key, &jar_stats);
        // Same TTL policy as the jar entry, so the totals an indexer compares
        // against outlive the jar they describe rather than archiving first.
        env.storage()
            .persistent()
            .extend_ttl(&stats_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        let supporter_key = DataKey::Supporter(jar_id.clone(), from.clone());
        let supporter_stats = env
            .storage()
            .persistent()
            .get::<DataKey, SupporterStats>(&supporter_key)
            .map(|s| SupporterStats {
                tip_count: s
                    .tip_count
                    .checked_add(1)
                    .unwrap_or_else(|| panic_with_error!(env, Error::StatsOverflow)),
                total: s
                    .total
                    .checked_add(amount)
                    .unwrap_or_else(|| panic_with_error!(env, Error::StatsOverflow)),
            })
            .unwrap_or(SupporterStats {
                tip_count: 1,
                total: amount,
            });
        env.storage()
            .persistent()
            .set(&supporter_key, &supporter_stats);
        // An archived entry here simply means the supporter has to tip again
        // (or the entry is restored) — the history is not lost from the event
        // log, and a badge check that reads zeros asks for a restore rather
        // than minting on a wrong count.
        env.storage()
            .persistent()
            .extend_ttl(&supporter_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
    }

    /// Read how much the contract is holding for `recipient` because a past tip
    /// could not deliver their share (#171). Returns 0 when nothing is held.
    pub fn get_held(env: Env, recipient: Address) -> i128 {
        let held_key = DataKey::Held(recipient);
        let held: i128 = env.storage().persistent().get(&held_key).unwrap_or(0);
        if held > 0 {
            env.storage()
                .persistent()
                .extend_ttl(&held_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }
        held
    }

    /// Pay out a recipient's held balance to them, once their account can
    /// receive the token again. Only `recipient` may authorize this (#171).
    ///
    /// Rejected with `NothingHeld` when the contract holds nothing for them, so
    /// a caller cannot mistake a no-op for a successful withdrawal.
    pub fn withdraw_held(env: Env, recipient: Address) {
        recipient.require_auth();
        let held_key = DataKey::Held(recipient.clone());
        let held: i128 = env.storage().persistent().get(&held_key).unwrap_or(0);
        if held <= 0 {
            panic_with_error!(&env, Error::NothingHeld);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let client = token::Client::new(&env, &token_addr);
        let contract_addr = env.current_contract_address();

        // Clear the balance before the transfer so the withdrawal cannot be
        // re-entered against a stale credit; the escrow invariant is that a
        // recipient's held total only ever decreases to zero on withdrawal.
        env.storage().persistent().remove(&held_key);
        client.transfer(&contract_addr, &recipient, &held);

        env.events()
            .publish((symbol_short!("withdraw"), recipient), held);
    }

    /// Register a recurring tip to a jar (#174), authorized by the supporter.
    ///
    /// This stores the on-chain schedule; it moves no funds itself. The
    /// payment rail is [`Self::collect_recurring`] (#175): the supporter
    /// approves this contract as a token spender (`approve` with
    /// `spender = tip_splitter`, at least `amount` per period) and any keeper
    /// collects once `next_due_ledger` is reached. Revoking the allowance — or
    /// `cancel_recurring` — stops collections dead, so the supporter never
    /// loses control of the money.
    ///
    /// Validated against the same checks `tip` applies so a stored schedule can
    /// actually be paid: the jar must exist, `amount` must be positive and pass
    /// `compute_shares` (no zero-truncated shares, no `i128` overflow) and clear
    /// any per-jar minimum tip, the message must be valid UTF-8 text with no
    /// control characters other than newline and fit `MAX_MESSAGE_LEN`, and
    /// `period_ledgers` must be non-zero. The new entry records its first due
    /// ledger (`current + period_ledgers`) and is stored under a fresh id.
    ///
    /// Emits a `recr_add` event carrying the assigned id.
    pub fn create_recurring(
        env: Env,
        supporter: Address,
        jar_id: String,
        amount: i128,
        period_ledgers: u32,
        message: String,
    ) -> u128 {
        supporter.require_auth();
        if period_ledgers == 0 {
            panic_with_error!(&env, Error::InvalidPeriod);
        }
        if amount == 0 {
            panic_with_error!(&env, Error::AmountZero);
        }
        if amount < 0 {
            panic_with_error!(&env, Error::AmountNegative);
        }
        Self::validate_message(&env, &message);

        let jar_key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&jar_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));

        // Same minimum-tip gate `tip` applies.
        let min_key = DataKey::MinTip(jar_id.clone());
        if let Some(min_amount) = env.storage().persistent().get::<DataKey, i128>(&min_key) {
            if amount < min_amount {
                panic_with_error!(&env, Error::BelowMinTipAmount);
            }
        }

        // Runs the tip's split arithmetic so a schedule whose amount could never
        // be paid (dust-truncation or overflow) is rejected at registration.
        let (_, net) = Self::split_fee(&env, amount);
        let _ = Self::compute_shares(&env, &jar.splits, net);

        let count_key = DataKey::RecurringCount;
        let count: u128 = env.storage().persistent().get(&count_key).unwrap_or(0);
        let id = count + 1;
        env.storage().persistent().set(&count_key, &id);

        let next_due_ledger = env
            .ledger()
            .sequence()
            .checked_add(period_ledgers)
            .unwrap_or_else(|| panic_with_error!(&env, Error::InvalidPeriod));

        let entry = Recurring {
            supporter: supporter.clone(),
            jar_id,
            amount,
            period_ledgers,
            message,
            next_due_ledger,
        };
        let rec_key = DataKey::Recurring(id);
        env.storage().persistent().set(&rec_key, &entry);
        env.storage()
            .persistent()
            .extend_ttl(&rec_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        env.storage()
            .persistent()
            .extend_ttl(&count_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        env.events()
            .publish((symbol_short!("recr_add"), supporter), id);
        id
    }

    /// Cancel a recurring tip (#174). Only the supporter who created it may
    /// authorize. Removes the stored entry and emits a `recr_del` event.
    pub fn cancel_recurring(env: Env, id: u128) {
        let rec_key = DataKey::Recurring(id);
        let entry: Recurring = env
            .storage()
            .persistent()
            .get(&rec_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::RecurringNotFound));
        entry.supporter.require_auth();

        env.storage().persistent().remove(&rec_key);
        env.events()
            .publish((symbol_short!("recr_del"), entry.supporter), id);
    }

    /// Read a stored recurring-tip entry, including its `next_due_ledger`
    /// (#174). Panics with `RecurringNotFound` for an unknown id.
    pub fn get_recurring(env: Env, id: u128) -> Recurring {
        let rec_key = DataKey::Recurring(id);
        let entry: Recurring = env
            .storage()
            .persistent()
            .get(&rec_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::RecurringNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&rec_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        entry
    }

    /// Collect one due payment of a recurring tip (#175). Callable by anyone —
    /// typically the keeper (the backend or the creator) — with **no**
    /// authorisation from the supporter.
    ///
    /// The payment rail is the token allowance the supporter granted this
    /// contract (`approve` on the token with `spender = tip_splitter`, at least
    /// `amount` per period). Each recipient's share is moved with
    /// `transfer_from` with the contract itself as spender, split by the same
    /// [`Self::payout_amounts`] arithmetic as a manual `tip` — including the
    /// protocol fee (#79), the tipper-is-recipient skip, and the held-escrow fallback
    /// (#171). The `tip` event is published in the identical shape with the
    /// supporter as `from` and an absent reference, so the indexer, webhooks
    /// and analytics pick a collected tip up exactly like a manual one. The
    /// jar and supporter stats are bumped as well.
    ///
    /// Rules, per the issue:
    /// - A call before `next_due_ledger` fails with `NotDue` and changes
    ///   nothing; the entry is *not* cancelled, so the same call succeeds once
    ///   the ledger is reached.
    /// - At most **one period is collected per call**, and `next_due_ledger`
    ///   advances by exactly `period_ledgers` each time: a keeper that was
    ///   offline for months cannot drain months at once, and the schedule keeps
    ///   its original cadence anchor.
    /// - An exhausted or expired allowance fails with the typed
    ///   `InsufficientAllowance` before any funds move (an expired allowance
    ///   reads as zero); a supporter whose balance cannot cover the period
    ///   fails with the typed `InsufficientBalance`. Both checks run before
    ///   any transfer, so a rejected collection changes nothing.
    /// - The per-jar minimum tip is *not* re-enforced here: it was enforced at
    ///   registration, and a minimum raised later must not silently strand
    ///   already-registered schedules. A supporter who wants the new floor
    ///   honoured cancels and re-registers.
    ///
    /// Reverts (leaving state untouched) with `RecurringNotFound` for an
    /// unknown id and `JarNotFound` if the jar's entry was archived. Emits the
    /// `tip` event and fires the post-tip hook (#176) like any settled tip.
    pub fn collect_recurring(env: Env, id: u128) {
        let rec_key = DataKey::Recurring(id);
        let entry: Recurring = env
            .storage()
            .persistent()
            .get(&rec_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::RecurringNotFound));

        // Due check first: an early call is a typed no-op, no funds move, no
        // TTL bump that would mask the rejection.
        if env.ledger().sequence() < entry.next_due_ledger {
            panic_with_error!(&env, Error::NotDue);
        }

        let jar_key = DataKey::Jar(entry.jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&jar_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));

        // A recurring schedule is a promise about the *default* token: it was
        // registered against `tip`'s token and minimum, and it is collected
        // with the contract's own allowance, which is per-asset.
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let client = token::Client::new(&env, &token_addr);
        let contract_addr = env.current_contract_address();

        // Allowance gate, checked before any transfer so an exhausted or
        // expired allowance is this contract's typed error rather than the
        // token's own: the supporter approves at least one period's `amount`,
        // and the transfers below consume at most `amount` in total (the plan
        // never moves more than the tip). An expired allowance reads as 0 and
        // fails the same check.
        if client.allowance(&entry.supporter, &contract_addr) < entry.amount {
            panic_with_error!(&env, Error::InsufficientAllowance);
        }

        // Same fail-fast rationale as `settle_tip`: an empty supporter wallet
        // must surface as this contract's typed `InsufficientBalance`, not the
        // SAC's own code, and the check runs before any transfer so a rejected
        // collection moves nothing.
        if client.balance(&entry.supporter) < entry.amount {
            panic_with_error!(&env, Error::InsufficientBalance);
        }

        env.storage()
            .persistent()
            .extend_ttl(&jar_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        // The same split a manual tip of this amount from this supporter would
        // produce — one shared helper, so collection can never drift from the
        // preview or the tip (#177).
        let (fee, net) = Self::split_fee(&env, entry.amount);
        let plan = Self::payout_amounts(&env, &jar.splits, &entry.supporter, net);
        // Same fee rule as `settle_tip`, drawn from the same allowance: the
        // fee plus the plan never exceeds `amount`, so the gate above covers
        // both.
        let fee_moved = match Self::fee_payee(&env, &entry.supporter, fee) {
            Some(admin) => {
                client.transfer_from(&contract_addr, &entry.supporter, &admin, &fee);
                fee
            }
            None => 0,
        };

        let mut breakdown: Vec<(Address, i128, bool)> = Vec::new(&env);
        for i in 0..jar.splits.len() {
            let split = jar.splits.get(i).unwrap();
            let share = plan.get(i).unwrap();
            if share == 0 {
                // The supporter is this recipient; the share stays with them
                // exactly as a manual tip would leave it.
                breakdown.push_back((split.to, 0, false));
                continue;
            }
            let delivered = matches!(
                client.try_transfer_from(&contract_addr, &entry.supporter, &split.to, &share),
                Ok(Ok(_))
            );
            if delivered {
                breakdown.push_back((split.to, share, false));
            } else {
                // Same escrow fallback as `settle_tip` (#171): the allowance is
                // still charged and the share waits in the contract for the
                // recipient to become payable again.
                client.transfer_from(&contract_addr, &entry.supporter, &contract_addr, &share);
                let held_key = DataKey::Held(split.to.clone());
                let held: i128 = env.storage().persistent().get(&held_key).unwrap_or(0);
                env.storage().persistent().set(&held_key, &(held + share));
                env.storage().persistent().extend_ttl(
                    &held_key,
                    JAR_TTL_THRESHOLD,
                    JAR_TTL_LEDGERS,
                );
                breakdown.push_back((split.to, share, true));
            }
        }

        // Advance by exactly one period, anchored on the old due ledger rather
        // than on "now plus period": a late keeper collects this period and the
        // next becomes due one period after the schedule said, never later.
        let next_due_ledger = entry
            .next_due_ledger
            .checked_add(entry.period_ledgers)
            .unwrap_or_else(|| panic_with_error!(&env, Error::InvalidPeriod));
        env.storage().persistent().set(
            &rec_key,
            &Recurring {
                next_due_ledger,
                ..entry.clone()
            },
        );
        env.storage()
            .persistent()
            .extend_ttl(&rec_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        Self::bump_totals(&env, &entry.jar_id, &entry.supporter, entry.amount);

        let seq: u64 = env
            .storage()
            .instance()
            .get(&DataKey::TipCount)
            .unwrap_or(0u64)
            + 1;
        env.storage().instance().set(&DataKey::TipCount, &seq);

        // Identical shape to `settle_tip`'s event; the collected payment
        // carries no client reference, published as an explicit `None`.
        env.events().publish(
            (symbol_short!("tip"), entry.jar_id.clone()),
            TipEvent {
                version: 1,
                from: entry.supporter.clone(),
                amount: entry.amount,
                fee: fee_moved,
                message: entry.message.clone(),
                breakdown,
                token: token_addr.clone(),
                seq,
                reference: None,
            },
        );

        Self::call_tip_hook(&env, &entry.jar_id, &entry.supporter, entry.amount);
    }

    /// Read a jar's configuration.
    pub fn get_jar(env: Env, jar_id: String) -> Jar {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        // Issue #92: extend both jar and min-tip in lockstep.
        Self::extend_jar_and_min_ttl(&env, &jar_id);
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
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        // Issue #92: extend both jar and min-tip in lockstep.
        Self::extend_jar_and_min_ttl(&env, &jar_id);
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
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        // Issue #92: extend both jar and min-tip in lockstep.
        Self::extend_jar_and_min_ttl(&env, &jar_id);
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
    /// The shares are of the post-fee amount (#79): they always sum to exactly
    /// `amount` minus the protocol fee, and the final recipient absorbs
    /// whatever the integer divisions truncated. With no fee configured that
    /// is `amount` itself.
    ///
    /// Rejects the same amounts `tip` rejects — non-positive, or small enough
    /// that some recipient's share would truncate to zero — so a preview that
    /// returns at all describes a tip that can go through. The sign is checked
    /// first, before the jar lookup and the jar's minimum, matching `tip`'s
    /// order, so a client that previews and then tips sees the same error
    /// (#97). Panics with `JarNotFound` if the slug is unregistered.
    ///
    /// One caveat: `tip` withholds a recipient's share when that recipient is
    /// the tipper, rather than making them pay themselves. This view does not
    /// know who is tipping, so it reports every recipient's full share. A
    /// client whose connected wallet appears in the splits should use
    /// [`Self::preview_tip`] instead, which takes the sender and matches the
    /// actual payout (#177).
    pub fn preview_split(env: Env, jar_id: String, amount: i128) -> Vec<i128> {
        // Same precedence as `tip`: an unusable amount is refused before the
        // jar lookup and before the minimum, so preview and payout report the
        // identical error for the same amount (#97).
        if amount == 0 {
            panic_with_error!(&env, Error::AmountZero);
        }
        if amount < 0 {
            panic_with_error!(&env, Error::AmountNegative);
        }

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

        let (_, net) = Self::split_fee(&env, amount);
        Self::compute_shares(&env, &jar.splits, net)
    }

    /// Return the preview together with each recipient, avoiding a second
    /// `get_jar` round trip for clients that need to render the breakdown.
    pub fn preview_split_details(env: Env, jar_id: String, amount: i128) -> Vec<PreviewSplit> {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        let (_, net) = Self::split_fee(&env, amount);
        let amounts = Self::compute_shares(&env, &jar.splits, net);
        let mut result = Vec::new(&env);
        for i in 0..amounts.len() {
            result.push_back(PreviewSplit {
                to: jar.splits.get(i).unwrap().to,
                amount: amounts.get(i).unwrap(),
            });
        }
        result
    }

    /// What each recipient would *actually receive* from a `tip` of `amount`
    /// sent by `from`, in split order — the sender-aware version of
    /// [`Self::preview_split`] (#177).
    ///
    /// `preview_split` does not know who is tipping, and `tip` treats a
    /// recipient who is also the sender differently: their share is not
    /// transferred and stays with them. For a collaborator
    /// tipping their own jar the two previews used to disagree, and the old
    /// advice to "account for that itself" reintroduced exactly the duplicated
    /// arithmetic `preview_split` exists to remove. This view closes the gap.
    ///
    /// The numbers are produced by the same private helper
    /// ([`Self::payout_amounts`]) that `tip` itself uses to build its transfer
    /// loop, so whatever the payout rule ends up being — including any change
    /// from #66 — `tip` and this view follow it together. A recipient who is
    /// the sender is reported as `0`, matching the `tip` event's breakdown
    /// entry for them.
    ///
    /// Same validation as `preview_split`: rejects unknown jars (`JarNotFound`),
    /// amounts below the jar's minimum (`BelowMinTipAmount`), and amounts
    /// `tip` would reject in `compute_shares`. Like every view it reads no
    /// balance, so it does *not* say whether `from` can actually pay; and a
    /// share that ends up escrowed as `held` at transfer time (#171) is still
    /// reported here, because the amount does leave the tipper either way —
    /// the `tip` event's breakdown carries the `held` flag.
    pub fn preview_tip(env: Env, jar_id: String, from: Address, amount: i128) -> Vec<i128> {
        // Same precedence as `settle_tip`: an unusable amount is refused before
        // the jar lookup, so a client gets the identical error from preview
        // and from the tip itself.
        if amount == 0 {
            panic_with_error!(&env, Error::AmountZero);
        }
        if amount < 0 {
            panic_with_error!(&env, Error::AmountNegative);
        }
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

        let (_, net) = Self::split_fee(&env, amount);
        Self::payout_amounts(&env, &jar.splits, &from, net)
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

    /// Return the smallest positive tip that can pay every configured split.
    /// The result also respects the jar's optional minimum tip amount and the
    /// protocol fee, which comes off before the split (#79).
    pub fn get_min_accepted_tip(env: Env, jar_id: String) -> i128 {
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        let denom = BPS_DENOM as i128;
        // The smallest post-fee amount that pays every recipient.
        let mut net = 1i128;
        for i in 0..jar.splits.len() {
            let bps = jar.splits.get(i).unwrap().bps as i128;
            let required = (denom + bps - 1) / bps;
            if required > net {
                net = required;
            }
        }
        // The smallest gross amount `a` whose net `a - floor(a * fee / D)`,
        // i.e. `ceil(a * (D - fee) / D)`, reaches `net`. That holds exactly
        // when `a * (D - fee) > (net - 1) * D`. With no fee this is `net`.
        let fee_bps = Self::get_fee_bps(env.clone()) as i128;
        let mut smallest = (net - 1) * denom / (denom - fee_bps) + 1;
        if let Some(minimum) = env
            .storage()
            .persistent()
            .get::<DataKey, i128>(&DataKey::MinTip(jar_id))
        {
            if minimum > smallest {
                smallest = minimum;
            }
        }
        smallest
    }

    /// A jar's running tip totals, as settled on-chain.
    ///
    /// A creator can compare this against the sum their own indexer recorded
    /// and tell whether the indexer missed tips, which the backend's blind
    /// spots (paged-out ledgers, events past the RPC's retention window) make
    /// otherwise invisible. A third-party page or embeddable button gets a
    /// total that does not depend on Novatip's backend at all.
    ///
    /// Returns zeros for a registered jar that has never been tipped — the
    /// entry is created by the first tip, not by `create_jar`. Panics with
    /// `JarNotFound` for an unregistered slug, matching `get_jar`: an untyped
    /// zero would make a typo'd slug look like a jar nobody has tipped.
    pub fn get_jar_stats(env: Env, jar_id: String) -> JarStats {
        let jar_key = DataKey::Jar(jar_id.clone());
        if !env.storage().persistent().has(&jar_key) {
            panic_with_error!(&env, Error::JarNotFound);
        }
        env.storage()
            .persistent()
            .extend_ttl(&jar_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        let key = DataKey::Stats(jar_id);
        // Bump only when the entry exists: `extend_ttl` loads the entry, so
        // bumping a key that has never been written fails with a storage error
        // rather than returning zeros. Same shape as `get_min_tip_amount`.
        match env.storage().persistent().get::<DataKey, JarStats>(&key) {
            Some(stats) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
                stats
            }
            None => JarStats {
                tip_count: 0,
                total: 0,
            },
        }
    }

    /// One supporter's running tip totals against one jar.
    ///
    /// This is what lets the roadmap's `supporter_badge` contract verify a
    /// milestone ("10 tips or $10 cumulative") on-chain instead of trusting an
    /// off-chain service to assert it.
    ///
    /// Returns zeros for an address that has never tipped the jar. Panics with
    /// `JarNotFound` for an unregistered slug, for the same reason
    /// `get_jar_stats` does.
    pub fn get_supporter_stats(env: Env, jar_id: String, supporter: Address) -> SupporterStats {
        let jar_key = DataKey::Jar(jar_id.clone());
        if !env.storage().persistent().has(&jar_key) {
            panic_with_error!(&env, Error::JarNotFound);
        }
        env.storage()
            .persistent()
            .extend_ttl(&jar_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        let key = DataKey::Supporter(jar_id, supporter);
        match env
            .storage()
            .persistent()
            .get::<DataKey, SupporterStats>(&key)
        {
            Some(stats) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
                stats
            }
            None => SupporterStats {
                tip_count: 0,
                total: 0,
            },
        }
    }

    /// Which interface shape this deployment speaks.
    ///
    /// Reads no storage — it is a compile-time constant, like `get_limits`.
    ///
    /// `tip` has already changed event shape twice and `jar_xfer` once without
    /// the name moving, and the SDK's `decodeTipEvent` still read three event
    /// elements after the fourth was added. A client that knows only a contract
    /// id can now ask before decoding, instead of discovering the mismatch when
    /// parsing fails or silently dropping the new field.
    ///
    /// Treat a number you have not seen before as "newer than I was built
    /// for": the fields this version documents still exist, so the safe
    /// response is to read what you know and ignore the rest, not to refuse.
    ///
    /// Bump it whenever a function is added or removed, an error code is added
    /// or renumbered, or an event's shape changes — see `INTERFACE_VERSION`.
    /// This is per-contract feature detection and complements, rather than
    /// replacing, the per-event schema version proposed in #83.
    pub fn version(_env: Env) -> u32 {
        INTERFACE_VERSION
    }

    /// Whether the admin has put `token` on the tip allowlist.
    ///
    /// A tip form needs to build its asset dropdown from what this contract
    /// actually accepts, rather than assuming the deploy-time token is the only
    /// option. The deploy-time token is always allowed; `tip` pays through it
    /// regardless of this list.
    pub fn is_token_allowed(env: Env, token: Address) -> bool {
        env.storage().instance().has(&DataKey::AllowedToken(token))
    }

    /// Whether `jar_id` is already registered.
    ///
    /// A slug-availability check would otherwise have to call `get_jar` and
    /// catch the `JarNotFound` panic, which is awkward from the SDK. This
    /// returns a plain `bool` and reads one storage key, so the onboarding form
    /// can run it on every (debounced) keystroke.
    pub fn jar_exists(env: Env, jar_id: String) -> bool {
        if !Self::is_valid_jar_id(&jar_id) {
            return false;
        }
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
        let key = DataKey::Jar(jar_id.clone());
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        // Issue #92: extend both jar and min-tip in lockstep.
        Self::extend_jar_and_min_ttl(&env, &jar_id);
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
    /// maximum recipient count, the maximum tip-message length in bytes, and
    /// the maximum accepted jar-id length in bytes.
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
            max_batch_tips: MAX_BATCH_TIPS,
            max_jar_id_len: MAX_JAR_SLUG_LEN + 1,
        }
    }

    /// The amounts a settled tip of `amount` from `from` actually transfers,
    /// one per recipient in split order (#177).
    ///
    /// This is `compute_shares` plus the one sender-dependent rule: a
    /// recipient who *is* the sender is reported as `0` — no self-transfer,
    /// and their share stays with them. Every other recipient gets exactly
    /// their `compute_shares` entry, so what leaves the tipper is what the
    /// other recipients receive, and that is `amount` minus the tipper's own
    /// share.
    ///
    /// An earlier version rolled the withheld share into the last recipient,
    /// which made a tipper who held a share pay it out to someone else (#78).
    ///
    /// `settle_tip` pays exactly these numbers out and
    /// [`Self::preview_tip`] reports them, which is what keeps a tip's event
    /// breakdown and its preview from ever diverging; `collect_recurring`
    /// builds its `transfer_from` plan from here too.
    ///
    /// Panics with the same errors as [`Self::compute_shares`] for amounts a
    /// tip of this jar could not pay.
    fn payout_amounts(env: &Env, splits: &Vec<Split>, from: &Address, amount: i128) -> Vec<i128> {
        let shares = Self::compute_shares(env, splits, amount);
        let mut plan: Vec<i128> = Vec::new(env);
        for i in 0..splits.len() {
            if splits.get(i).unwrap().to == *from {
                plan.push_back(0);
            } else {
                plan.push_back(shares.get(i).unwrap());
            }
        }
        plan
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
    /// Panics with `AmountZero` / `AmountNegative` if `amount` is not positive,
    /// `SplitAmountTooSmall` if it is small enough that some recipient's share
    /// would truncate to zero, and `AmountOverflow` if `amount * bps` overflows
    /// `i128`.
    fn compute_shares(env: &Env, splits: &Vec<Split>, amount: i128) -> Vec<i128> {
        if amount == 0 {
            panic_with_error!(env, Error::AmountZero);
        }
        if amount < 0 {
            panic_with_error!(env, Error::AmountNegative);
        }
        let n = splits.len();
        let denom = BPS_DENOM as i128;

        // Reject amounts too small to pay every recipient a non-zero share.
        // With integer division, a recipient's share of `amount * bps / 10_000`
        // truncates to zero when `amount < 10_000 / bps`. If that happens, the
        // recipient is silently skipped and the final recipient absorbs the
        // dust — the tip succeeds but the collaborator never sees it.
        // Better to fail loudly with SplitAmountTooSmall than to pay nobody.
        for i in 0..n {
            let bps = splits.get(i).unwrap().bps as i128;
            // `checked_mul`: `amount * bps` can overflow i128 before the
            // division brings it back into range. That is a caller error — the
            // amount is too large for the contract to split safely — so it
            // surfaces as a typed error rather than an opaque wasm trap.
            let product = amount
                .checked_mul(bps)
                .unwrap_or_else(|| panic_with_error!(env, Error::AmountOverflow));
            if product < denom {
                panic_with_error!(env, Error::SplitAmountTooSmall);
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
                    .unwrap_or_else(|| panic_with_error!(env, Error::AmountOverflow))
                    / denom
            };
            distributed += share;
            shares.push_back(share);
        }
        shares
    }

    /// Extend a jar's TTL and its minimum-tip entry in lockstep (issue #92).
    ///
    /// The min-tip lives in a separate persistent key with its own TTL. If only
    /// the jar is extended, an idle jar can still tip: `get_jar` succeeds but
    /// `tip` fails once the min-tip entry expires. This helper keeps them
    /// aligned so a jar is only as available as its least-recently-touched entry.
    fn extend_jar_and_min_ttl(env: &Env, jar_id: &String) {
        let key = DataKey::Jar(jar_id.clone());
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        let min_key = DataKey::MinTip(jar_id.clone());
        if env.storage().persistent().has(&min_key) {
            env.storage()
                .persistent()
                .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }
    }

    /// Validate a message's length and content (#96).
    ///
    /// A Soroban `String` is a byte buffer with no encoding guarantee, so
    /// without this check a tipper can attach invalid UTF-8 or embedded
    /// control bytes and the contract echoes them verbatim into the `tip`
    /// event. Consumers assume text: the backend writes the message into a
    /// Postgres text column, which rejects NUL, so such a tip is silently
    /// dropped by the indexer, and invalid UTF-8 renders differently in every
    /// client.
    ///
    /// Newline is allowed because the tip form offers multi-line input; every
    /// other control character — including NUL, tab, carriage return and DEL —
    /// is rejected. `char::is_control` also covers the C1 range, which is only
    /// reachable through a valid multi-byte sequence.
    ///
    /// The check runs before any transfer, so a rejected tip moves no funds.
    fn validate_message(env: &Env, message: &String) {
        let len = message.len();
        if len > MAX_MESSAGE_LEN {
            panic_with_error!(env, Error::MessageTooLong);
        }
        let mut buf = [0u8; MAX_MESSAGE_LEN as usize];
        message.copy_into_slice(&mut buf[..len as usize]);
        let text = core::str::from_utf8(&buf[..len as usize])
            .unwrap_or_else(|_| panic_with_error!(env, Error::InvalidMessage));
        if text.chars().any(|c| c.is_control() && c != '\n') {
            panic_with_error!(env, Error::InvalidMessage);
        }
    }

    /// Validate splits in one pass: range, duplicate recipient, then total sum.
    ///
    /// A zero-bps entry is dead weight that consumes a slot and misleads clients.
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

        // Issue #94: reject the contract itself or its token as a recipient.
        // Funds sent to either address are locked forever — the contract has no
        // withdrawal function for tips, and the token contract cannot receive
        // transfers meant for end users.
        let contract_addr = env.current_contract_address();
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));

        let mut total: u32 = 0;
        let mut seen: Vec<Address> = Vec::new(env);
        for i in 0..n {
            let split = splits.get(i).unwrap();
            let bps = split.bps;
            if bps == 0 || bps > BPS_DENOM {
                panic_with_error!(env, Error::SplitOutOfRange);
            }
            // Reject contract or token address as recipient (#94).
            if split.to == contract_addr || split.to == token_addr {
                panic_with_error!(env, Error::InvalidRecipient);
            }
            // `checked_add` rather than `+=`: `bps` is caller-supplied, and an
            // overflow must surface as the same typed `SplitSumNot100Pct` every
            // other rejection returns, not as an opaque wasm trap. The bound
            // above already makes overflow unreachable, so this is belt and
            // braces — but it puts the invariant in the code rather than
            // resting on `overflow-checks = true` in the release profile.
            total = total
                .checked_add(bps)
                .unwrap_or_else(|| panic_with_error!(env, Error::SplitSumNot100Pct));
            if seen.contains(&split.to) {
                panic_with_error!(env, Error::DuplicateRecipient);
            }
            seen.push_back(split.to);
        }
        if total != BPS_DENOM {
            panic_with_error!(env, Error::SplitSumNot100Pct);
        }
    }
}

mod test;
