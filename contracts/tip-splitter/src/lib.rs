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
//!
//! A worked call for every public function, including how to build the split
//! vector for `create_jar`, is in the `examples` module (`cargo doc` only).

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
/// Most tokens the allowlist may hold, the default token included (#321).
/// Bounds the `Vec` that `get_allowed_tokens` returns.
const MAX_ALLOWED_TOKENS: u32 = 10;
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
const INTERFACE_VERSION: u32 = 14;

/// Storage schema this build expects. Bump it together with a new step in
/// `migrate` whenever stored state needs transforming after an upgrade (#325).
const SCHEMA_VERSION: u32 = 1;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Split {
    pub to: Address,
    pub bps: u32,
}

#[contracttype]
#[derive(Clone)]
pub struct TipRequest {
    pub jar_id: String,
    pub amount: i128,
    pub message: String,
    /// Optional opaque 32-byte client reference echoed in this entry's `tip`
    /// event, as `tip_with_ref` does for a single tip (#318).
    pub reference: Option<BytesN<32>>,
}

// Internal argument bundle for `settle_tip` (#335). Named fields make the two
// booleans/options impossible to swap at a call site, and keep the function
// under clippy's argument limit without an allow.
struct TipParams<'a> {
    jar_id: &'a String,
    amount: i128,
    message: &'a String,
    token_addr: &'a Address,
    enforce_min: bool,
    reference: Option<BytesN<32>>,
}

#[contracttype]
#[derive(Clone)]
pub struct Jar {
    pub owner: Address,
    pub splits: Vec<Split>,
    pub locked: bool,
    pub paused: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewSplit {
    pub to: Address,
    pub amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarStats {
    pub tip_count: u64,
    pub total: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupporterStats {
    pub tip_count: u64,
    pub total: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    pub bps_denom: u32,
    pub max_recipients: u32,
    pub max_message_len: u32,
    pub max_batch_tips: u32,
    pub max_jar_id_len: u32,
    pub min_jar_id_len: u32,
    pub max_fee_bps: u32,
}

/// Everything a tip page needs about one jar, in one simulation (#319).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarInfo {
    pub owner: Address,
    pub splits: Vec<Split>,
    pub locked: bool,
    pub paused: bool,
    pub min_tip_amount: Option<i128>,
    pub min_accepted_tip: i128,
    pub tip_count: u64,
    pub total: i128,
    pub fee_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recurring {
    pub supporter: Address,
    pub jar_id: String,
    pub amount: i128,
    pub period_ledgers: u32,
    pub message: String,
    pub next_due_ledger: u32,
}

/// Contract storage keys. See the [storage-key reference](../../../docs/CONTRACT.md#storage-keys-and-lifetimes)
/// for each key's storage class, access paths, TTL renewal and archive behavior.
#[contracttype(export = false)]
pub enum DataKey {
    /// Contract admin: gates the functions listed under "Admin-only functions"
    /// in `docs/CONTRACT.md`. Written by the constructor and `transfer_admin`.
    Admin,
    /// Address of the USDC Stellar Asset Contract used for all tips.
    Token,
    /// A tip jar keyed by its public slug, e.g. "@alice".
    Jar(String),
    /// Optional minimum tip amount for a jar keyed by its public slug.
    MinTip(String),
    /// A recipient's undeliverable share, held by the contract until they can
    /// receive it again (see #171). Keyed by the recipient address and the
    /// token the share was paid in (#219); value is the amount owed them in
    /// that token.
    Held(Address, Address),
    /// Immutable internal id of the jar currently under a slug (#222). Assigned
    /// by `create_jar`, moved by `rename_jar`, removed by `delete_jar`.
    JarUid(String),
    /// Monotonic counter of issued jar uids. Instance storage.
    JarCount,
    /// The jar uid a recurring schedule was created against (#222).
    RecurringJar(u128),
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
    /// Wasm hash installed by the most recent successful `upgrade` (#334).
    /// Used as the `previous_hash` in the next upgrade event. Absent on a
    /// deployment that has never been upgraded since this key was introduced.
    WasmHash,
    /// Optional maximum tip amount for a jar keyed by its public slug (#323).
    MaxTip(String),
    /// Per-jar flag (#313): when present and `true`, `tip_in_token` rejects
    /// every token other than the default one for this jar. Persistent,
    /// keyed by the public slug like `MinTip`; absence means any allowed token.
    DefaultTokenOnly(String),
    /// Every allowed token in insertion order, default first (#321). Instance
    /// storage like `AllowedToken`; bounded by `MAX_ALLOWED_TOKENS`.
    AllowedTokens,
    /// Storage schema version recorded by `migrate` (#325). Instance storage;
    /// absent on a deployment that has never migrated, read as 0.
    SchemaVersion,
    /// Optional fee recipient set by the admin (#312). Instance storage;
    /// absence means fees go to the admin.
    FeeRecipient,
    /// Pending removal of an unreachable recipient from a jar (#239), keyed by
    /// the jar's public slug. Persistent; at most one per jar.
    PendingRemoval(String),
}

/// A recipient removal announced by the jar owner (#239). It is only
/// executable while the jar's splits still equal `splits`, so any change to
/// the recipients (an update, a departure, a replacement) voids it.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingRemoval {
    pub recipient: Address,
    pub ready_ledger: u32,
    pub splits: Vec<Split>,
}

/// Ledgers between announcing a recipient's removal and being able to execute
/// it (#239): about seven days at 5 seconds per ledger. Long enough for a
/// reachable recipient to notice the `rm_req` event and cancel.
const RECIPIENT_REMOVAL_DELAY_LEDGERS: u32 = 120_960;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    JarExists = 2,
    JarNotFound = 3,
    InvalidSplits = 4,
    /// **Retired; never raised.** Kept so existing codes keep their values.
    /// Amount validation now reports the specific failure: `AmountZero` (25),
    /// `AmountNegative` (26), `SplitAmountTooSmall` (27), `AmountOverflow` (28)
    /// or `InvalidMinimumAmount` (29).
    InvalidAmount = 5,
    TooManyRecipients = 6,
    DuplicateRecipient = 7,
    MessageTooLong = 8,
    InvalidJarId = 9,
    SplitsEmpty = 10,
    SplitOutOfRange = 11,
    SplitSumNot100Pct = 12,
    BelowMinTipAmount = 13,
    NotRecipient = 14,
    SoleRecipient = 15,
    RecurringNotFound = 16,
    InvalidPeriod = 17,
    NothingHeld = 18,
    TokenNotAllowed = 19,
    CannotDisallowDefault = 20,
    StatsOverflow = 21,
    InsufficientBalance = 22,
    NotDue = 23,
    InsufficientAllowance = 24,
    AmountZero = 25,
    AmountNegative = 26,
    SplitAmountTooSmall = 27,
    AmountOverflow = 28,
    InvalidMinimumAmount = 29,
    NoFundsMoved = 30,
    InvalidRecipient = 31,
    AdminExists = 32,
    ContractPaused = 33,
    JarIdInUse = 34,
    FeeTooHigh = 35,
    InvalidMessage = 36,
    BatchEmpty = 37,
    BatchTooLarge = 38,
    JarPaused = 39,
    TooManyAllowedTokens = 40,
    AboveMaxTipAmount = 41,
    InvalidMaximumAmount = 42,
    /// `tip_in_token` was called with a non-default token for a jar that
    /// accepts only the default token (#313). Appended, never renumbered.
    NonDefaultToken = 43,
    NotAuthorized = 44,
    /// The allowlist or constructor was given an address that is not a SAC.
    InvalidToken = 44,
    NotAuthorized = 40,
    /// `cancel_recurring` was called by neither the supporter nor the jar
    /// owner (#324). Appended after 43; it previously collided with 40.
    NotAuthorized = 44,
    /// `transfer_jar_ownership` named the jar's current owner as the new owner,
    /// a no-op handoff. Appended, never renumbered.
    SameOwner = 45,
    /// `execute_recipient_removal` found no pending removal request for the
    /// jar, or the jar's splits changed since the request was announced.
    RemovalNotRequested = 46,
    /// `execute_recipient_removal` was called before the announced delay
    /// (`RECIPIENT_REMOVAL_DELAY_LEDGERS`) elapsed.
    RemovalNotReady = 47,
    /// `collect_recurring` found that the slug the schedule was created for now
    /// names a different jar (deleted and re-created, #222).
    JarChanged = 48,
}

#[contractclient(name = "TipHookClient")]
pub trait TipHook {
    fn on_tip(env: Env, jar_id: String, from: Address, amount: i128, token: Address);
}

#[contract]
pub struct TipSplitter;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarCreatedEvent {
    pub version: u32,
    pub owner: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitsUpdatedEvent {
    pub version: u32,
    pub count: u32,
    pub splits: Vec<Split>,
}

// Data published by `rm_req` (#239). Topics: `("rm_req", jar_id)`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemovalRequestedEvent {
    pub version: u32,
    pub recipient: Address,
    pub ready_ledger: u32,
}

// Data published by `rm_cancel` (#239). Topics: `("rm_cancel", jar_id)`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemovalCancelledEvent {
    pub version: u32,
    pub recipient: Address,
    pub caller: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarTransferredEvent {
    pub version: u32,
    pub previous_owner: Address,
    pub new_owner: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarAdminRecoveredEvent {
    pub version: u32,
    pub admin: Address,
    pub previous_owner: Address,
    pub new_owner: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarDeletedEvent {
    pub version: u32,
    pub jar_id: String,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarLockEvent {
    pub version: u32,
    pub locked: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarPausedEvent {
    pub version: u32,
    pub paused: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeldWithdrawnEvent {
    pub version: u32,
    pub amount: i128,
    pub token: Address,
}

// Data published by `fee_held` (#226) when the protocol fee could not be paid
// and was escrowed for the payee instead; the topics are the symbol and the jar.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeHeldEvent {
    pub version: u32,
    pub payee: Address,
    pub token: Address,
    pub amount: i128,
}

// Data published by `wd_to` (#311); the topic carries the recipient.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeldWithdrawnToEvent {
    pub version: u32,
    pub amount: i128,
    pub to: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarRenamedEvent {
    pub version: u32,
    pub jar_id: String,
    pub new_jar_id: String,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinTipEvent {
    pub version: u32,
    pub amount: Option<i128>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaxTipEvent {
    pub version: u32,
    pub amount: Option<i128>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TipEvent {
    pub version: u32,
    pub from: Address,
    pub amount: i128,
    pub fee: i128,
    pub message: String,
    pub breakdown: Vec<(Address, i128, bool)>,
    pub token: Address,
    pub seq: u64,
    pub reference: Option<BytesN<32>>,
}

// Data published by every admin configuration event.
//
// Admin events share one shape: topics are just the event-name symbol, and the
// data is one of these versioned structs whose first fields are `version` then
// `admin`, followed by the new value. An indexer therefore needs no per-event
// special case for where the admin lives.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenAllowlistEvent {
    pub version: u32,
    pub admin: Address,
    pub token: Address,
    pub allowed: bool,
}

// Data published by `tip_hook`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TipHookEvent {
    pub version: u32,
    pub admin: Address,
    pub hook: Option<Address>,
}

// Data published by `reg_set`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrarEvent {
    pub version: u32,
    pub admin: Address,
    pub registrar: Option<Address>,
}

// Data published by `fee_set`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeEvent {
    pub version: u32,
    pub admin: Address,
    pub fee_bps: u32,
}

// Data published by `fee_rcpt` (#312).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeRecipientEvent {
    pub version: u32,
    pub admin: Address,
    pub recipient: Option<Address>,
}

// Data published by `pause` and `resume`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PauseEvent {
    pub version: u32,
    pub admin: Address,
    pub paused: bool,
}

// Data published by `adm_xfer`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminTransferredEvent {
    pub version: u32,
    pub admin: Address,
    pub new_admin: Address,
}

// Data published by `upgrade` (#102, #334).
//
// `previous_hash` comes from `DataKey::WasmHash`, written by the previous
// upgrade. soroban-sdk 22 exposes no deployer getter for the running code hash,
// so the first upgrade of a deployment that predates this field reports `None`
// and the docs say so.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradedEvent {
    pub version: u32,
    pub admin: Address,
    pub previous_hash: Option<BytesN<32>>,
    pub new_hash: BytesN<32>,
}

/// Published by `migrate` when it moves stored state to a new schema (#325).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigratedEvent {
    pub version: u32,
    pub admin: Address,
    pub from_version: u32,
    pub to_version: u32,
}

// Data published by `recr_add` (#317). Topics: `("recr_add", supporter, jar_id)`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecurringAddedEvent {
    pub version: u32,
    pub id: u128,
    pub jar_id: String,
    pub amount: i128,
    pub period_ledgers: u32,
    pub next_due_ledger: u32,
}

// Data published by `recr_upd` (#316). Topics: `("recr_upd", supporter)`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecurringUpdatedEvent {
    pub version: u32,
    pub id: u128,
    pub amount: i128,
    pub message: String,
}

#[contractimpl]
impl TipSplitter {
    pub fn __constructor(env: Env, admin: Address, token: Address) {
        // The admin manages privileged operations (fees, pause, hooks, allowlist)
        // while the token address represents the Stellar Asset Contract that all tips
        // settle through. Providing the same address for both is a deployment error.
        // The token address is recorded permanently and can never be changed post-deploy,
        // so catching this misconfiguration at construction prevents deploying a permanently
        // bricked contract requiring complete redeployment and migration of jars.
        if admin == token {
            panic_with_error!(&env, Error::InvalidToken);
        }
        Self::require_token_contract(&env, &token);
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::AllowedToken(token.clone()), &true);
        env.storage()
            .instance()
            .set(&DataKey::AllowedTokens, &Vec::from_array(&env, [token]));
    }

    /// The allowlist as a list, default token first. A deployment created
    /// before #321 has no list yet and reports just the default token.
    fn allowed_tokens(env: &Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::from_array(env, [Self::get_token(env.clone())]))
    }

    /// Ensure an allowlisted address is a Stellar Asset Contract.  Calling
    /// the mandatory SAC `decimals` method through the fallible host API
    /// rejects arbitrary contracts and accounts without trapping the caller.
    fn require_token_contract(env: &Env, token: &Address) {
        if *token == env.current_contract_address() {
            panic_with_error!(env, Error::InvalidToken);
        }
        let args: Vec<Val> = Vec::new(env);
        if env
            .try_invoke_contract::<u32, soroban_sdk::Error>(
                token,
                &symbol_short!("decimals"),
                args,
            )
            .is_err()
        {
            panic_with_error!(env, Error::InvalidToken);
        }
    }

    /// The contract admin, which every admin-gated entry point requires.
    /// Returns the address so the caller can name it in an event.
    fn require_admin(env: &Env) -> Address {
        let admin: Address = Self::get_admin(env.clone());
        admin.require_auth();
        admin
    }

    pub fn allow_token(env: Env, token: Address) {
        let admin = Self::require_admin(&env);
        Self::require_token_contract(&env, &token);
        let mut list = Self::allowed_tokens(&env);
        if !list.contains(&token) {
            if list.len() >= MAX_ALLOWED_TOKENS {
                panic_with_error!(&env, Error::TooManyAllowedTokens);
            }
            list.push_back(token.clone());
            env.storage().instance().set(&DataKey::AllowedTokens, &list);
        }
        env.storage()
            .instance()
            .set(&DataKey::AllowedToken(token.clone()), &true);
        env.events().publish(
            (symbol_short!("tk_allow"),),
            TokenAllowlistEvent {
                version: 1,
                admin,
                token,
                allowed: true,
            },
        );
    }

    pub fn disallow_token(env: Env, token: Address) {
        let admin = Self::require_admin(&env);
        let default: Address = Self::get_token(env.clone());
        if token == default {
            panic_with_error!(&env, Error::CannotDisallowDefault);
        }
        env.storage()
            .instance()
            .remove(&DataKey::AllowedToken(token.clone()));
        let mut list = Self::allowed_tokens(&env);
        if let Some(i) = list.first_index_of(&token) {
            list.remove(i);
            env.storage().instance().set(&DataKey::AllowedTokens, &list);
        }
        env.events().publish(
            (symbol_short!("tk_dis"),),
            TokenAllowlistEvent {
                version: 1,
                admin,
                token,
                allowed: false,
            },
        );
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
        if !(MIN_JAR_SLUG_LEN + 1..=MAX_JAR_SLUG_LEN + 1).contains(&len) {
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

    pub fn create_jar(env: Env, owner: Address, jar_id: String, splits: Vec<Split>) {
        owner.require_auth();
        if !Self::is_valid_jar_id(&jar_id) {
            panic_with_error!(&env, Error::InvalidJarId);
        }
        // Issue #94: reject the contract itself or its token as jar owner.
        let contract_addr = env.current_contract_address();
        let token_addr: Address = Self::get_token(env.clone());
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
        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: owner.clone(),
                splits,
                locked: false,
                paused: false,
            },
        );
        Self::assign_jar_uid(&env, &jar_id);

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

    pub fn update_splits(env: Env, jar_id: String, splits: Vec<Split>) {
        let jar = Self::load_jar(&env, &jar_id);
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
        let new_splits = splits.clone();
        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: jar.owner,
                splits,
                locked: jar.locked,
                paused: jar.paused,
            },
        );

        env.events().publish(
            (symbol_short!("splits"), jar_id),
            SplitsUpdatedEvent {
                version: 1,
                count: split_count,
                splits: new_splits,
            },
        );
    }

    /// Replace `old` with `new` in the jar's splits, keeping its bps and
    /// position (#310). Authorized by `old` only, so it works on a locked jar
    /// and without the owner; the lock is kept.
    pub fn change_recipient(env: Env, jar_id: String, old: Address, new: Address) {
        old.require_auth();
        let jar = Self::load_jar(&env, &jar_id);
        if new == env.current_contract_address() || new == Self::get_token(env.clone()) {
            panic_with_error!(&env, Error::InvalidRecipient);
        }
        let mut splits = jar.splits.clone();
        let mut index: Option<u32> = None;
        for i in 0..splits.len() {
            let s = splits.get(i).unwrap();
            if s.to == new {
                panic_with_error!(&env, Error::DuplicateRecipient);
            }
            if s.to == old {
                index = Some(i);
            }
        }
        let i = index.unwrap_or_else(|| panic_with_error!(&env, Error::NotRecipient));
        let bps = splits.get(i).unwrap().bps;
        splits.set(i, Split { to: new, bps });

        let split_count = splits.len();
        let new_splits = splits.clone();
        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: jar.owner,
                splits,
                locked: jar.locked,
                paused: jar.paused,
            },
        );

        env.events().publish(
            (symbol_short!("splits"), jar_id),
            SplitsUpdatedEvent {
                version: 1,
                count: split_count,
                splits: new_splits,
            },
        );
    }

    pub fn lock_splits(env: Env, jar_id: String) {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();

        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: jar.owner,
                splits: jar.splits,
                locked: true,
                paused: jar.paused,
            },
        );

        env.events().publish(
            (symbol_short!("lock"), jar_id),
            JarLockEvent {
                version: 1,
                locked: true,
            },
        );
    }

    /// Remove the lock. Needs the owner and **every** current recipient, so a
    /// recipient who has lost their key (or is a contract that cannot sign)
    /// blocks it; such a jar recovers through `request_recipient_removal`
    /// instead, which keeps the lock in place (#239).
    pub fn unlock_splits(env: Env, jar_id: String) {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();

        // All current recipients must consent to disarm the protection.
        let splits = jar.splits.clone();
        for i in 0..splits.len() {
            splits.get(i).unwrap().to.require_auth();
        }

        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: jar.owner,
                splits,
                locked: false,
                paused: jar.paused,
            },
        );

        env.events().publish(
            (symbol_short!("lock"), jar_id),
            JarLockEvent {
                version: 1,
                locked: false,
            },
        );
    }

    /// Announce the removal of a recipient who cannot sign (#239). Owner only.
    ///
    /// This is the recovery path for a locked jar. It cannot cut a reachable
    /// recipient unilaterally: the removal only becomes executable
    /// `RECIPIENT_REMOVAL_DELAY_LEDGERS` after this call, the `rm_req` event
    /// announces it, and the recipient (or the owner) can cancel at any time
    /// with `cancel_recipient_removal`. One request per jar; a new one replaces
    /// the old and restarts the delay.
    pub fn request_recipient_removal(env: Env, jar_id: String, recipient: Address) {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();
        if jar.splits.len() == 1 {
            panic_with_error!(&env, Error::SoleRecipient);
        }
        if !jar.splits.iter().any(|s| s.to == recipient) {
            panic_with_error!(&env, Error::NotRecipient);
        }
        let ready_ledger = env
            .ledger()
            .sequence()
            .saturating_add(RECIPIENT_REMOVAL_DELAY_LEDGERS);
        let key = DataKey::PendingRemoval(jar_id.clone());
        env.storage().persistent().set(
            &key,
            &PendingRemoval {
                recipient: recipient.clone(),
                ready_ledger,
                splits: jar.splits,
            },
        );
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        env.events().publish(
            (symbol_short!("rm_req"), jar_id),
            RemovalRequestedEvent {
                version: 1,
                recipient,
                ready_ledger,
            },
        );
    }

    /// Cancel a pending removal (#239). `caller` is the recipient it names, who
    /// vetoes it by signing, or the jar owner.
    pub fn cancel_recipient_removal(env: Env, jar_id: String, caller: Address) {
        let key = DataKey::PendingRemoval(jar_id.clone());
        let pending: PendingRemoval = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::RemovalNotRequested));
        if caller != pending.recipient && caller != Self::load_jar(&env, &jar_id).owner {
            panic_with_error!(&env, Error::NotAuthorized);
        }
        caller.require_auth();
        env.storage().persistent().remove(&key);
        env.events().publish(
            (symbol_short!("rm_cancel"), jar_id),
            RemovalCancelledEvent {
                version: 1,
                recipient: pending.recipient,
                caller,
            },
        );
    }

    /// Execute an announced removal once its delay has elapsed (#239). Owner
    /// only. The departed share goes to the last remaining recipient, as in
    /// `leave_jar`, and the lock stays on. Fails with `RemovalNotRequested` if
    /// the splits changed since the announcement.
    pub fn execute_recipient_removal(env: Env, jar_id: String) {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();
        let key = DataKey::PendingRemoval(jar_id.clone());
        let pending: PendingRemoval = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::RemovalNotRequested));
        if pending.splits != jar.splits {
            panic_with_error!(&env, Error::RemovalNotRequested);
        }
        if env.ledger().sequence() < pending.ready_ledger {
            panic_with_error!(&env, Error::RemovalNotReady);
        }

        let mut departed_bps: u32 = 0;
        let mut remaining: Vec<Split> = Vec::new(&env);
        for split in jar.splits.iter() {
            if split.to == pending.recipient {
                departed_bps = split.bps;
            } else {
                remaining.push_back(split);
            }
        }
        let last = remaining.len() - 1;
        let mut last_split = remaining.get(last).unwrap();
        last_split.bps = last_split
            .bps
            .checked_add(departed_bps)
            .unwrap_or_else(|| panic_with_error!(&env, Error::SplitSumNot100Pct));
        remaining.set(last, last_split);
        Self::validate_splits(&env, &remaining);

        env.storage().persistent().remove(&key);
        let split_count = remaining.len();
        let new_splits = remaining.clone();
        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: jar.owner,
                splits: remaining,
                locked: jar.locked,
                paused: jar.paused,
            },
        );
        env.events().publish(
            (symbol_short!("splits"), jar_id),
            SplitsUpdatedEvent {
                version: 1,
                count: split_count,
                splits: new_splits,
            },
        );
    }

    pub fn is_splits_locked(env: Env, jar_id: String) -> bool {
        let jar = Self::load_jar(&env, &jar_id);
        jar.locked
    }

    pub fn set_jar_paused(env: Env, jar_id: String, paused: bool) {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();

        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: jar.owner,
                splits: jar.splits,
                locked: jar.locked,
                paused,
            },
        );

        env.events().publish(
            (symbol_short!("jar_pause"), jar_id),
            JarPausedEvent { version: 1, paused },
        );
    }

    pub fn is_jar_paused(env: Env, jar_id: String) -> bool {
        let jar = Self::load_jar(&env, &jar_id);
        jar.paused
    }

    pub fn leave_jar(env: Env, jar_id: String, recipient: Address) {
        let jar = Self::load_jar(&env, &jar_id);
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
        let new_splits = remaining.clone();
        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: jar.owner,
                splits: remaining,
                locked: jar.locked,
                paused: jar.paused,
            },
        );

        // Same payload as `update_splits`, so an indexer decodes every
        // `splits` event with one shape.
        env.events().publish(
            (symbol_short!("splits"), jar_id),
            SplitsUpdatedEvent {
                version: 1,
                count: split_count,
                splits: new_splits,
            },
        );
    }

    pub fn transfer_jar_ownership(env: Env, jar_id: String, new_owner: Address) {
        let jar = Self::load_jar(&env, &jar_id);
        // Issue #94: reject the contract itself or its token as new owner.
        let contract_addr = env.current_contract_address();
        let token_addr: Address = Self::get_token(env.clone());
        if new_owner == contract_addr || new_owner == token_addr {
            panic_with_error!(&env, Error::InvalidRecipient);
        }
        if new_owner == jar.owner {
            panic_with_error!(&env, Error::SameOwner);
        }
        jar.owner.require_auth();
        new_owner.require_auth_for_args((jar_id.clone(), new_owner.clone()).into_val(&env));
        let prev_owner = jar.owner;
        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: new_owner.clone(),
                splits: jar.splits,
                // The lock is a promise to the collaborators, not a privilege
                // of the owner, so it survives the transfer (#173).
                locked: jar.locked,
                // Likewise the pause is a property of the jar, so a handover
                // does not silently reopen a jar the previous owner closed (#45).
                paused: jar.paused,
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

    /// Reassign a jar after its owner key is lost. This deliberately trusts
    /// the contract admin; neither the old nor new owner needs to authorize.
    pub fn admin_recover_jar(env: Env, jar_id: String, new_owner: Address) {
        let admin = Self::require_admin(&env);
        let jar = Self::load_jar(&env, &jar_id);
        let contract_addr = env.current_contract_address();
        let token_addr: Address = Self::get_token(env.clone());
        if new_owner == contract_addr || new_owner == token_addr || new_owner == jar.owner {
            panic_with_error!(&env, Error::InvalidRecipient);
        }

        let previous_owner = jar.owner;
        Self::store_jar(
            &env,
            &jar_id,
            &Jar {
                owner: new_owner.clone(),
                splits: jar.splits,
                locked: jar.locked,
                paused: jar.paused,
            },
        );
        env.events().publish(
            (symbol_short!("jar_recov"), jar_id),
            JarAdminRecoveredEvent {
                version: 1,
                admin,
                previous_owner,
                new_owner,
            },
        );
    }

    pub fn delete_jar(env: Env, jar_id: String) {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();

        env.storage()
            .persistent()
            .remove(&DataKey::Jar(jar_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::Stats(jar_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::MinTip(jar_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::MaxTip(jar_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::DefaultTokenOnly(jar_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::PendingRemoval(jar_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::JarUid(jar_id.clone()));

        env.events().publish(
            (symbol_short!("jar_del"), jar_id.clone()),
            JarDeletedEvent { version: 1, jar_id },
        );
    }

    pub fn transfer_admin(env: Env, new_admin: Address) {
        let current_admin = Self::require_admin(&env);
        if current_admin == new_admin {
            panic_with_error!(&env, Error::AdminExists);
        }
        // Require the proposed administrator's signature before changing the
        // control key. A typo or stale address therefore cannot strand admin
        // access or redirect protocol fees (#230).
        new_admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::Admin, &new_admin.clone());
        env.events().publish(
            (symbol_short!("adm_xfer"),),
            AdminTransferredEvent {
                version: 1,
                admin: current_admin,
                new_admin,
            },
        );
    }

    /// Stop tipping contract-wide (admin only): `tip`, `tip_with_ref`,
    /// `tip_in_token` and `tip_many` reject with `ContractPaused` until
    /// `resume`. Other calls, including `withdraw_held` and owner edits, keep
    /// working; `collect_recurring` is not gated yet (#220).
    pub fn pause(env: Env) {
        let admin = Self::require_admin(&env);
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events().publish(
            (symbol_short!("pause"),),
            PauseEvent {
                version: 1,
                admin,
                paused: true,
            },
        );
    }

    pub fn resume(env: Env) {
        let admin = Self::require_admin(&env);
        env.storage().instance().remove(&DataKey::Paused);
        env.events().publish(
            (symbol_short!("resume"),),
            PauseEvent {
                version: 1,
                admin,
                paused: false,
            },
        );
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().has(&DataKey::Paused)
    }

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin = Self::require_admin(&env);

        // soroban-sdk 22 has no deployer getter for the running code hash, so
        // the previous hash is the one recorded by the last upgrade. A
        // deployment that has never upgraded since this key existed reports
        // `None`; docs/CONTRACT.md documents that first-upgrade gap.
        let previous_hash: Option<BytesN<32>> = env.storage().instance().get(&DataKey::WasmHash);

        // Deployer's update_current_contract_wasm replaces the running WASM
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        env.storage()
            .instance()
            .set(&DataKey::WasmHash, &new_wasm_hash);

        env.events().publish(
            (symbol_short!("upgrade"),),
            UpgradedEvent {
                version: 1,
                admin,
                previous_hash,
                new_hash: new_wasm_hash,
            },
        );
    }

    /// Bring stored state up to this build's schema after an `upgrade` (#325).
    /// Admin-only and idempotent: each step runs once, guarded by the stored
    /// schema version, and a call with nothing to do changes nothing.
    pub fn migrate(env: Env) {
        let admin = Self::require_admin(&env);
        let from_version: u32 = env
            .storage()
            .instance()
            .get(&DataKey::SchemaVersion)
            .unwrap_or(0);
        if from_version >= SCHEMA_VERSION {
            return;
        }

        // Per-version steps go here, each guarded by `from_version < N`.
        // Version 1 is the baseline and needs no transform.

        env.storage()
            .instance()
            .set(&DataKey::SchemaVersion, &SCHEMA_VERSION);
        env.events().publish(
            (symbol_short!("migrate"),),
            MigratedEvent {
                version: 1,
                admin,
                from_version,
                to_version: SCHEMA_VERSION,
            },
        );
    }

    pub fn rename_jar(env: Env, jar_id: String, new_jar_id: String) {
        if !Self::is_valid_jar_id(&new_jar_id) {
            panic_with_error!(&env, Error::InvalidJarId);
        }
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();

        // Check if new id is already in use
        let new_key = DataKey::Jar(new_jar_id.clone());
        if env.storage().persistent().has(&new_key) {
            panic_with_error!(&env, Error::JarIdInUse);
        }

        // Copy jar to new id
        Self::store_jar(&env, &new_jar_id, &jar);

        // Delete old id
        env.storage()
            .persistent()
            .remove(&DataKey::Jar(jar_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::PendingRemoval(jar_id.clone()));

        // The jar keeps its uid under the new slug, so a schedule created for the
        // old slug can no longer be mistaken for one that follows the jar (#222).
        let old_uid_key = DataKey::JarUid(jar_id.clone());
        if let Some(uid) = env.storage().persistent().get::<_, u128>(&old_uid_key) {
            let new_uid_key = DataKey::JarUid(new_jar_id.clone());
            env.storage().persistent().set(&new_uid_key, &uid);
            env.storage()
                .persistent()
                .extend_ttl(&new_uid_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
            env.storage().persistent().remove(&old_uid_key);
        }

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
        let old_max_key = DataKey::MaxTip(jar_id.clone());
        if let Some(max_amount) = env.storage().persistent().get::<_, i128>(&old_max_key) {
            let new_max_key = DataKey::MaxTip(new_jar_id.clone());
            env.storage().persistent().set(&new_max_key, &max_amount);
            env.storage()
                .persistent()
                .extend_ttl(&new_max_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
            env.storage().persistent().remove(&old_max_key);
        }
        // Move the default-token-only flag (#313) so it survives a rename.
        let old_flag_key = DataKey::DefaultTokenOnly(jar_id.clone());
        if let Some(flag) = env.storage().persistent().get::<_, bool>(&old_flag_key) {
            let new_flag_key = DataKey::DefaultTokenOnly(new_jar_id.clone());
            env.storage().persistent().set(&new_flag_key, &flag);
            env.storage().persistent().extend_ttl(
                &new_flag_key,
                JAR_TTL_THRESHOLD,
                JAR_TTL_LEDGERS,
            );
            env.storage().persistent().remove(&old_flag_key);
        }

        env.events().publish(
            (symbol_short!("jar_rnm"), jar_id.clone()),
            JarRenamedEvent {
                version: 1,
                jar_id,
                new_jar_id,
            },
        );
    }

    pub fn set_min_tip_amount(env: Env, jar_id: String, min_amount: Option<i128>) {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();

        let min_key = DataKey::MinTip(jar_id.clone());
        match min_amount {
            Some(amount) => {
                if amount <= 0 {
                    panic_with_error!(&env, Error::InvalidMinimumAmount);
                }
                if let Some(max) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, i128>(&DataKey::MaxTip(jar_id.clone()))
                {
                    if amount > max {
                        panic_with_error!(&env, Error::InvalidMinimumAmount);
                    }
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

    /// Set or clear an optional per-jar maximum tip (#323), mirroring
    /// `set_min_tip_amount`. Owner only; the maximum must be positive and no
    /// lower than the jar's minimum. Enforced for the default token only.
    pub fn set_max_tip_amount(env: Env, jar_id: String, max_amount: Option<i128>) {
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

        let max_key = DataKey::MaxTip(jar_id.clone());
        match max_amount {
            Some(amount) => {
                if amount <= 0 {
                    panic_with_error!(&env, Error::InvalidMaximumAmount);
                }
                if let Some(min) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, i128>(&DataKey::MinTip(jar_id.clone()))
                {
                    if amount < min {
                        panic_with_error!(&env, Error::InvalidMaximumAmount);
                    }
                }
                env.storage().persistent().set(&max_key, &amount);
                env.storage()
                    .persistent()
                    .extend_ttl(&max_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
            }
            None => {
                env.storage().persistent().remove(&max_key);
            }
        }

        env.events().publish(
            (symbol_short!("max_tip"), jar_id),
            MaxTipEvent {
                version: 1,
                amount: max_amount,
            },
        );
    }

    /// Set or clear the per-jar default-token-only flag (#313). Owner only.
    /// While `true`, `tip_in_token` rejects any token other than the default
    /// with `NonDefaultToken`, before any transfer. Survives rename and
    /// ownership transfer, since it is keyed by the jar's slug.
    pub fn set_jar_tokens_default_only(env: Env, jar_id: String, default_only: bool) {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner.require_auth();

        let key = DataKey::DefaultTokenOnly(jar_id.clone());
        if default_only {
            env.storage().persistent().set(&key, &true);
            env.storage()
                .persistent()
                .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        } else {
            env.storage().persistent().remove(&key);
        }
    }

    /// Whether the jar accepts only the default token (#313). `false` when unset.
    pub fn get_jar_tokens_default_only(env: Env, jar_id: String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::DefaultTokenOnly(jar_id))
            .unwrap_or(false)
    }

    pub fn tip(env: Env, from: Address, jar_id: String, amount: i128, message: String) -> u64 {
        Self::tip_inner(env, from, jar_id, amount, message, None)
    }

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

    pub fn tip_many(
        env: Env,
        from: Address,
        tips: Vec<TipRequest>,
        token: Option<Address>,
    ) -> Vec<u64> {
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
        // One token per batch (#322), checked like `tip_in_token` before any
        // transfer; the minimum applies only to the default token.
        let default: Address = Self::get_token(env.clone());
        let token_addr = match token {
            Some(t) => {
                if !env
                    .storage()
                    .instance()
                    .has(&DataKey::AllowedToken(t.clone()))
                {
                    panic_with_error!(&env, Error::TokenNotAllowed);
                }
                t
            }
            None => default.clone(),
        };
        let enforce_min = token_addr == default;

        let mut seqs: Vec<u64> = Vec::new(&env);
        for i in 0..tips.len() {
            let req = tips.get(i).unwrap();
            let seq = Self::settle_tip(
                &env,
                &from,
                TipParams {
                    jar_id: &req.jar_id,
                    amount: req.amount,
                    message: &req.message,
                    token_addr: &token_addr,
                    enforce_min,
                    reference: req.reference.clone(),
                },
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
        let token_addr: Address = Self::get_token(env.clone());
        Self::settle_tip(
            &env,
            &from,
            TipParams {
                jar_id: &jar_id,
                amount,
                message: &message,
                token_addr: &token_addr,
                enforce_min: true,
                reference,
            },
        )
    }

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
        let default: Address = Self::get_token(env.clone());
        // #313: a jar that opted into default-token-only refuses other tokens
        // before any transfer is attempted.
        if token != default && Self::get_jar_tokens_default_only(env.clone(), jar_id.clone()) {
            panic_with_error!(&env, Error::NonDefaultToken);
        }
        Self::settle_tip(
            &env,
            &from,
            TipParams {
                jar_id: &jar_id,
                amount,
                message: &message,
                token_addr: &token,
                enforce_min: token == default,
                reference: None,
            },
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
    fn settle_tip(env: &Env, from: &Address, params: TipParams) -> u64 {
        let TipParams {
            jar_id,
            amount,
            message,
            token_addr,
            enforce_min,
            reference,
        } = params;
        if amount == 0 {
            panic_with_error!(env, Error::AmountZero);
        }
        if amount < 0 {
            panic_with_error!(env, Error::AmountNegative);
        }
        Self::validate_message(env, message);

        let jar = Self::load_jar(env, jar_id);

        // Reject tips to a jar its owner has paused (#45), before any funds
        // move. Reads are unaffected; only the tip entry points consult this.
        if jar.paused {
            panic_with_error!(env, Error::JarPaused);
        }

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
            Self::check_max_tip(env, jar_id, amount);
        }

        // Issue #91: extend the contract instance TTL alongside the jar so the
        // contract itself doesn't expire while jars are still active. In
        // soroban-sdk 22 this also extends the Wasm code entry.
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_LEDGERS);

        let client = token::Client::new(env, token_addr);

        // Fail fast with the contract's own code when the sender cannot cover
        // the full tip. Without this, the token's transfer failure propagates
        // with the SAC's own number (10/11/13), which collides with ours —
        // an empty wallet would surface as `SplitsEmpty`. Checked once, before
        // any transfer, so a failed tip never moves partial funds.
        // A tipper who is also a recipient keeps that recipient share, so only
        // the fee and the other shares need to be funded (#229).
        let (fee, net) = Self::split_fee(env, amount);
        let plan = Self::payout_amounts(env, &jar.splits, from, net);
        let mut required = 0i128;
        for i in 0..plan.len() {
            required = required
                .checked_add(plan.get(i).unwrap())
                .unwrap_or_else(|| panic_with_error!(env, Error::AmountOverflow));
        }
        if let Some(payee) = Self::fee_payee(env, from, fee) {
            if payee != *from {
                required = required
                    .checked_add(fee)
                    .unwrap_or_else(|| panic_with_error!(env, Error::AmountOverflow));
            }
        }
        if client.balance(from) < required {
            panic_with_error!(env, Error::InsufficientBalance);
        }

        Self::pay_out(
            env,
            from,
            None,
            &jar.splits,
            TipParams {
                jar_id,
                amount,
                message,
                token_addr,
                enforce_min,
                reference,
            },
        )
    }

    /// The payout shared by `tip` and `collect_recurring` (#326): the fee, the
    /// split with its held-share fallback, the totals, the sequence number, the
    /// `tip` event and the post-tip hook. `spender` picks how funds move:
    /// `None` is a direct `transfer` from `from`; `Some` is a `transfer_from`
    /// against that spender's allowance.
    fn pay_out(
        env: &Env,
        from: &Address,
        spender: Option<&Address>,
        splits: &Vec<Split>,
        params: TipParams,
    ) -> u64 {
        let TipParams {
            jar_id,
            amount,
            message,
            token_addr,
            reference,
            ..
        } = params;
        let client = token::Client::new(env, token_addr);
        let contract_addr = env.current_contract_address();
        let send = |to: &Address, value: &i128| match spender {
            Some(s) => client.transfer_from(s, from, to, value),
            None => client.transfer(from, to, value),
        };
        let try_send = |to: &Address, value: &i128| match spender {
            Some(s) => matches!(client.try_transfer_from(s, from, to, value), Ok(Ok(_))),
            None => matches!(client.try_transfer(from, to, value), Ok(Ok(_))),
        };

        let n = splits.len();
        // The protocol fee comes off the top (#79); the split, its
        // too-small-to-split guard and its dust rule all apply to what is
        // left. One shared calculation with `preview_split` and `preview_tip`,
        // so the number a client shows before signing is the number paid out
        // here.
        let (fee, net) = Self::split_fee(env, amount);
        let plan = Self::payout_amounts(env, splits, from, net);
        let fee_moved = match Self::fee_payee(env, from, fee) {
            Some(payee) => {
                if !try_send(&payee, &fee) {
                    // The payee cannot receive the token (no trustline, or
                    // deauthorised). Escrow the fee for them, as for a recipient's
                    // share, instead of reverting every tip (#226); they collect
                    // it with `withdraw_held`.
                    send(&contract_addr, &fee);
                    let held_key = DataKey::Held(payee.clone(), token_addr.clone());
                    let held: i128 = env.storage().persistent().get(&held_key).unwrap_or(0);
                    env.storage().persistent().set(&held_key, &(held + fee));
                    env.storage().persistent().extend_ttl(
                        &held_key,
                        JAR_TTL_THRESHOLD,
                        JAR_TTL_LEDGERS,
                    );
                    env.events().publish(
                        (symbol_short!("fee_held"), jar_id.clone()),
                        FeeHeldEvent {
                            version: 1,
                            payee,
                            token: token_addr.clone(),
                            amount: fee,
                        },
                    );
                }
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
            let split = splits.get(i).unwrap();
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
            let delivered = try_send(&split.to, &share);
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
                send(&contract_addr, &share);
                total_transferred += share;
                let held_key = DataKey::Held(split.to.clone(), token_addr.clone());
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
        // misleading for indexers, leaderboards, and notifications. The
        // recurring path checks the same plan up front (#221).
        if spender.is_none() && total_transferred == 0 {
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
        Self::call_tip_hook(env, jar_id, from, amount, &token_addr);

        seq
    }

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
        env.events().publish(
            (symbol_short!("tip_hook"),),
            TipHookEvent {
                version: 1,
                admin,
                hook,
            },
        );
    }

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
    fn call_tip_hook(
        env: &Env,
        jar_id: &String,
        from: &Address,
        amount: i128,
        token: &Address,
    ) {
        let Some(hook): Option<Address> = env.storage().instance().get(&DataKey::TipHook) else {
            return;
        };
        let mut args: Vec<Val> = Vec::new(env);
        args.push_back(jar_id.clone().into_val(env));
        args.push_back(from.clone().into_val(env));
        args.push_back(amount.into_val(env));
        args.push_back(token.clone().into_val(env));
        let _ = env.try_invoke_contract::<(), soroban_sdk::Error>(
            &hook,
            &symbol_short!("on_tip"),
            args,
        );
    }

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
        env.events().publish(
            (symbol_short!("reg_set"),),
            RegistrarEvent {
                version: 1,
                admin,
                registrar,
            },
        );
    }

    pub fn get_registrar(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Registrar)
    }

    pub fn set_fee_bps(env: Env, fee_bps: u32) {
        let admin = Self::require_admin(&env);
        if fee_bps > MAX_FEE_BPS {
            panic_with_error!(&env, Error::FeeTooHigh);
        }
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        env.events().publish(
            (symbol_short!("fee_set"),),
            FeeEvent {
                version: 1,
                admin,
                fee_bps,
            },
        );
    }

    pub fn get_fee_bps(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0)
    }

    /// Send protocol fees to `recipient` instead of the admin (#312); `None`
    /// clears it so fees go to the admin again. Admin only.
    pub fn set_fee_recipient(env: Env, recipient: Option<Address>) {
        let admin = Self::require_admin(&env);
        match &recipient {
            Some(r) => {
                if *r == env.current_contract_address() || *r == Self::get_token(env.clone()) {
                    panic_with_error!(&env, Error::InvalidRecipient);
                }
                env.storage().instance().set(&DataKey::FeeRecipient, r);
            }
            None => env.storage().instance().remove(&DataKey::FeeRecipient),
        }
        env.events().publish(
            (symbol_short!("fee_rcpt"),),
            FeeRecipientEvent {
                version: 1,
                admin,
                recipient,
            },
        );
    }

    /// Where protocol fees are paid: the configured recipient, or the admin.
    pub fn get_fee_recipient(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::FeeRecipient)
            .unwrap_or_else(|| Self::get_admin(env.clone()))
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

    /// Whether a tip of `amount` from `from` through `splits` moves any funds:
    /// some recipient other than the tipper gets a share, or a fee is paid to
    /// someone other than the tipper (#221). The same plan `pay_out` executes.
    fn plan_moves_funds(env: &Env, from: &Address, splits: &Vec<Split>, amount: i128) -> bool {
        let (fee, net) = Self::split_fee(env, amount);
        let plan = Self::payout_amounts(env, splits, from, net);
        for i in 0..plan.len() {
            if plan.get(i).unwrap() > 0 {
                return true;
            }
        }
        Self::fee_payee(env, from, fee).is_some()
    }

    /// Who a fee of `fee` should be transferred to, or `None` when nothing
    /// moves: no fee, or the tipper is the fee recipient (#312). That second
    /// case is the self-transfer rule applied to the fee, and the `tip` event
    /// then reports a fee of 0 because nothing moved.
    fn fee_payee(env: &Env, from: &Address, fee: i128) -> Option<Address> {
        if fee == 0 {
            return None;
        }
        let payee = Self::get_fee_recipient(env.clone());
        if payee == *from {
            None
        } else {
            Some(payee)
        }
    }

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

    /// What the contract holds for `recipient` in `token` (#219).
    pub fn get_held(env: Env, recipient: Address, token: Address) -> i128 {
        let held_key = DataKey::Held(recipient, token);
        let held: i128 = env.storage().persistent().get(&held_key).unwrap_or(0);
        if held > 0 {
            env.storage()
                .persistent()
                .extend_ttl(&held_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }
        held
    }

    /// Pay `recipient` what is held for them in `token` only (#219).
    pub fn withdraw_held(env: Env, recipient: Address, token: Address) {
        recipient.require_auth();
        let held_key = DataKey::Held(recipient.clone(), token.clone());
        let held: i128 = env.storage().persistent().get(&held_key).unwrap_or(0);
        if held <= 0 {
            panic_with_error!(&env, Error::NothingHeld);
        }

        let client = token::Client::new(&env, &token);
        let contract_addr = env.current_contract_address();

        // Clear the balance before the transfer so the withdrawal cannot be
        // re-entered against a stale credit; the escrow invariant is that a
        // recipient's held total only ever decreases to zero on withdrawal.
        env.storage().persistent().remove(&held_key);
        client.transfer(&contract_addr, &recipient, &held);

        env.events().publish(
            (symbol_short!("withdraw"), recipient),
            HeldWithdrawnEvent {
                version: 2,
                amount: held,
                token,
            },
        );
    }

    /// Like `withdraw_held`, but pays `to` (#311), for a recipient account
    /// that can no longer receive the token. Authorized by `recipient`. Pays
    /// only the balance held in the default token.
    pub fn withdraw_held_to(env: Env, recipient: Address, to: Address) {
        recipient.require_auth();
        let contract_addr = env.current_contract_address();
        let token_addr: Address = Self::get_token(env.clone());
        if to == contract_addr || to == token_addr {
            panic_with_error!(&env, Error::InvalidRecipient);
        }
        let held_key = DataKey::Held(recipient.clone(), token_addr.clone());
        let held: i128 = env.storage().persistent().get(&held_key).unwrap_or(0);
        if held <= 0 {
            panic_with_error!(&env, Error::NothingHeld);
        }

        env.storage().persistent().remove(&held_key);
        token::Client::new(&env, &token_addr).transfer(&contract_addr, &to, &held);

        env.events().publish(
            (symbol_short!("wd_to"), recipient),
            HeldWithdrawnToEvent {
                version: 1,
                amount: held,
                to,
            },
        );
    }

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

        let jar = Self::load_jar(&env, &jar_id);

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

        // A schedule that would move nothing (the supporter is the jar's only
        // recipient and no fee leaves their account) is never stored (#221);
        // `collect_recurring` repeats the check in case the jar changes later.
        if !Self::plan_moves_funds(&env, &supporter, &jar.splits, amount) {
            panic_with_error!(&env, Error::NoFundsMoved);
        }

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
            jar_id: jar_id.clone(),
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
        // Bind the schedule to this jar, not just to its slug (#222).
        let bound_key = DataKey::RecurringJar(id);
        env.storage()
            .persistent()
            .set(&bound_key, &Self::jar_uid(&env, &jar_id));
        env.storage()
            .persistent()
            .extend_ttl(&bound_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        env.events().publish(
            (symbol_short!("recr_add"), supporter, jar_id.clone()),
            RecurringAddedEvent {
                version: 1,
                id,
                jar_id,
                amount,
                period_ledgers,
                next_due_ledger,
            },
        );
        id
    }

    /// Remove a schedule. `caller` is the supporter, or the owner of the jar the
    /// schedule pays (#324); no funds have moved, so neither loses anything.
    pub fn cancel_recurring(env: Env, caller: Address, id: u128) {
        let rec_key = DataKey::Recurring(id);
        let entry: Recurring = env
            .storage()
            .persistent()
            .get(&rec_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::RecurringNotFound));
        if caller != entry.supporter && caller != Self::load_jar(&env, &entry.jar_id).owner {
            panic_with_error!(&env, Error::NotAuthorized);
        }
        caller.require_auth();

        env.storage().persistent().remove(&rec_key);
        env.storage().persistent().remove(&DataKey::RecurringJar(id));
        env.events()
            .publish((symbol_short!("recr_del"), entry.supporter), (id, caller));
    }

    /// Change a recurring tip's amount and message in place (#316). Keeps the
    /// id and `next_due_ledger`; validated like `create_recurring`.
    pub fn update_recurring(env: Env, id: u128, amount: i128, message: String) {
        let rec_key = DataKey::Recurring(id);
        let mut entry: Recurring = env
            .storage()
            .persistent()
            .get(&rec_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::RecurringNotFound));
        entry.supporter.require_auth();
        if amount == 0 {
            panic_with_error!(&env, Error::AmountZero);
        }
        if amount < 0 {
            panic_with_error!(&env, Error::AmountNegative);
        }
        Self::validate_message(&env, &message);

        let jar: Jar = env
            .storage()
            .persistent()
            .get(&DataKey::Jar(entry.jar_id.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, Error::JarNotFound));
        let min_key = DataKey::MinTip(entry.jar_id.clone());
        if let Some(min_amount) = env.storage().persistent().get::<DataKey, i128>(&min_key) {
            if amount < min_amount {
                panic_with_error!(&env, Error::BelowMinTipAmount);
            }
        }
        let (_, net) = Self::split_fee(&env, amount);
        let _ = Self::compute_shares(&env, &jar.splits, net);

        entry.amount = amount;
        entry.message = message.clone();
        env.storage().persistent().set(&rec_key, &entry);
        env.storage()
            .persistent()
            .extend_ttl(&rec_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);

        env.events().publish(
            (symbol_short!("recr_upd"), entry.supporter),
            RecurringUpdatedEvent {
                version: 1,
                id,
                amount,
                message,
            },
        );
    }

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

        let jar = Self::load_jar(&env, &entry.jar_id);

        // The slug may have been freed and claimed by someone else since the
        // supporter registered this schedule (#222). Fail with a typed error
        // before any state changes rather than paying the new jar. Schedules
        // registered before jar uids existed carry no binding and are not
        // checked.
        let bound_key = DataKey::RecurringJar(id);
        if let Some(bound_uid) = env.storage().persistent().get::<_, u128>(&bound_key) {
            let current: Option<u128> = env
                .storage()
                .persistent()
                .get(&DataKey::JarUid(entry.jar_id.clone()));
            if current != Some(bound_uid) {
                panic_with_error!(&env, Error::JarChanged);
            }
            env.storage()
                .persistent()
                .extend_ttl(&bound_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }

        // Same rule as a direct tip (#93), checked before any state changes:
        // a collection that would move no funds must not publish a `tip`
        // event, bump the totals or consume a sequence number (#221).
        if !Self::plan_moves_funds(&env, &entry.supporter, &jar.splits, entry.amount) {
            panic_with_error!(&env, Error::NoFundsMoved);
        }

        // A recurring schedule is a promise about the *default* token: it was
        // registered against `tip`'s token and minimum, and it is collected
        // with the contract's own allowance, which is per-asset.
        let token_addr: Address = Self::get_token(env.clone());
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

        // Advance by exactly one period, anchored on the old due ledger rather
        // than on "now plus period": a late keeper collects this period and the
        // next becomes due one period after the schedule said, never later.
        // Written before the payout so the post-tip hook already sees the
        // advanced schedule.
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

        // The same payout a manual tip of this amount from this supporter
        // would make, drawn from the allowance (#177, #326). The collected
        // payment carries no client reference.
        Self::pay_out(
            &env,
            &entry.supporter,
            Some(&contract_addr),
            &jar.splits,
            TipParams {
                jar_id: &entry.jar_id,
                amount: entry.amount,
                message: &entry.message,
                token_addr: &token_addr,
                enforce_min: false,
                reference: None,
            },
        );
    }

    pub fn get_jar(env: Env, jar_id: String) -> Jar {
        Self::load_jar(&env, &jar_id)
    }

    pub fn get_jar_owner(env: Env, jar_id: String) -> Address {
        let jar = Self::load_jar(&env, &jar_id);
        jar.owner
    }

    pub fn get_split_count(env: Env, jar_id: String) -> u32 {
        let jar = Self::load_jar(&env, &jar_id);
        jar.splits.len()
    }

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

        let jar = Self::load_jar(&env, &jar_id);
        if jar.paused {
            panic_with_error!(&env, Error::JarPaused);
        }

        let min_key = DataKey::MinTip(jar_id.clone());
        if let Some(min_amount) = env.storage().persistent().get::<DataKey, i128>(&min_key) {
            if amount < min_amount {
                panic_with_error!(&env, Error::BelowMinTipAmount);
            }
            env.storage()
                .persistent()
                .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }
        Self::check_max_tip(&env, &jar_id, amount);

        let (_, net) = Self::split_fee(&env, amount);
        Self::compute_shares(&env, &jar.splits, net)
    }

    pub fn preview_split_details(env: Env, jar_id: String, amount: i128) -> Vec<PreviewSplit> {
        if amount == 0 {
            panic_with_error!(&env, Error::AmountZero);
        }
        if amount < 0 {
            panic_with_error!(&env, Error::AmountNegative);
        }
        let jar = Self::load_jar(&env, &jar_id);
        if jar.paused {
            panic_with_error!(&env, Error::JarPaused);
        }
        let min_key = DataKey::MinTip(jar_id.clone());
        if let Some(min_amount) = env.storage().persistent().get::<DataKey, i128>(&min_key) {
            if amount < min_amount {
                panic_with_error!(&env, Error::BelowMinTipAmount);
            }
            env.storage().persistent().extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }
        Self::check_max_tip(&env, &jar_id, amount);
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
        let jar = Self::load_jar(&env, &jar_id);
        if jar.paused {
            panic_with_error!(&env, Error::JarPaused);
        }

        let min_key = DataKey::MinTip(jar_id.clone());
        if let Some(min_amount) = env.storage().persistent().get::<DataKey, i128>(&min_key) {
            if amount < min_amount {
                panic_with_error!(&env, Error::BelowMinTipAmount);
            }
            env.storage()
                .persistent()
                .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }

        Self::check_max_tip(&env, &jar_id, amount);

        let (_, net) = Self::split_fee(&env, amount);
        Self::payout_amounts(&env, &jar.splits, &from, net)
    }

    /// Reject `amount` above the jar's optional maximum (#323).
    fn check_max_tip(env: &Env, jar_id: &String, amount: i128) {
        if let Some(max) = env
            .storage()
            .persistent()
            .get::<DataKey, i128>(&DataKey::MaxTip(jar_id.clone()))
        {
            if amount > max {
                panic_with_error!(env, Error::AboveMaxTipAmount);
            }
        }
    }

    pub fn get_max_tip_amount(env: Env, jar_id: String) -> Option<i128> {
        if !env
            .storage()
            .persistent()
            .has(&DataKey::Jar(jar_id.clone()))
        {
            panic_with_error!(&env, Error::JarNotFound);
        }
        env.storage().persistent().get(&DataKey::MaxTip(jar_id))
    }

    pub fn get_min_tip_amount(env: Env, jar_id: String) -> Option<i128> {
        Self::load_jar(&env, &jar_id);

        let min_key = DataKey::MinTip(jar_id);
        let min_amount: Option<i128> = env.storage().persistent().get(&min_key);
        if min_amount.is_some() {
            env.storage()
                .persistent()
                .extend_ttl(&min_key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        }
        min_amount
    }

    pub fn get_min_accepted_tip(env: Env, jar_id: String) -> i128 {
        let jar = Self::load_jar(&env, &jar_id);
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

    pub fn get_jar_stats(env: Env, jar_id: String) -> JarStats {
        Self::load_jar(&env, &jar_id);

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

    /// `get_jar`, `get_min_tip_amount`, `get_min_accepted_tip`,
    /// `get_jar_stats` and `get_fee_bps` in one call (#319). Fails with
    /// `JarNotFound` for an unknown slug, like `get_jar`.
    pub fn get_jar_info(env: Env, jar_id: String) -> JarInfo {
        let jar = Self::get_jar(env.clone(), jar_id.clone());
        let stats = Self::get_jar_stats(env.clone(), jar_id.clone());
        JarInfo {
            owner: jar.owner,
            splits: jar.splits,
            locked: jar.locked,
            paused: jar.paused,
            min_tip_amount: Self::get_min_tip_amount(env.clone(), jar_id.clone()),
            min_accepted_tip: Self::get_min_accepted_tip(env.clone(), jar_id),
            tip_count: stats.tip_count,
            total: stats.total,
            fee_bps: Self::get_fee_bps(env),
        }
    }

    pub fn get_supporter_stats(env: Env, jar_id: String, supporter: Address) -> SupporterStats {
        Self::load_jar(&env, &jar_id);

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

    pub fn version(_env: Env) -> u32 {
        INTERFACE_VERSION
    }

    /// Every allowed token, default first (#321).
    pub fn get_allowed_tokens(env: Env) -> Vec<Address> {
        Self::allowed_tokens(&env)
    }

    pub fn is_token_allowed(env: Env, token: Address) -> bool {
        env.storage().instance().has(&DataKey::AllowedToken(token))
    }

    pub fn jar_exists(env: Env, jar_id: String) -> bool {
        if !Self::is_valid_jar_id(&jar_id) {
            return false;
        }
        env.storage().persistent().has(&DataKey::Jar(jar_id))
    }

    pub fn is_recipient(env: Env, jar_id: String, address: Address) -> bool {
        let jar = Self::load_jar(&env, &jar_id);
        for i in 0..jar.splits.len() {
            if jar.splits.get(i).unwrap().to == address {
                return true;
            }
        }
        false
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn get_token(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn get_limits(_env: Env) -> Limits {
        Limits {
            bps_denom: BPS_DENOM,
            max_recipients: MAX_RECIPIENTS,
            max_message_len: MAX_MESSAGE_LEN,
            max_batch_tips: MAX_BATCH_TIPS,
            max_jar_id_len: MAX_JAR_SLUG_LEN + 1,
            min_jar_id_len: MIN_JAR_SLUG_LEN + 1,
            max_fee_bps: MAX_FEE_BPS,
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
    /// Example: `amount = 1_001` over `3_333 / 3_333 / 3_334` bps gives
    /// `333`, `333` and `1_001 - 666 = 335`, which sums to `1_001`. Listing the
    /// `3_334` entry first moves the extra unit to whoever is last, so order
    /// matters. See "Splitting rules" in `docs/CONTRACT.md`.
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

    /// Load a jar or fail with `JarNotFound`, extending the jar and its minimum
    /// tip in lockstep (#92). Every jar read goes through here (#327).
    fn load_jar(env: &Env, jar_id: &String) -> Jar {
        let jar: Jar = env
            .storage()
            .persistent()
            .get(&DataKey::Jar(jar_id.clone()))
            .unwrap_or_else(|| panic_with_error!(env, Error::JarNotFound));
        Self::extend_jar_and_min_ttl(env, jar_id);
        jar
    }

    /// Gives the jar under `jar_id` a fresh uid and returns it (#222).
    fn assign_jar_uid(env: &Env, jar_id: &String) -> u128 {
        let count_key = DataKey::JarCount;
        let uid: u128 = env
            .storage()
            .instance()
            .get::<_, u128>(&count_key)
            .unwrap_or(0)
            + 1;
        env.storage().instance().set(&count_key, &uid);
        let key = DataKey::JarUid(jar_id.clone());
        env.storage().persistent().set(&key, &uid);
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
        uid
    }

    /// The uid of the jar under `jar_id`. A jar created before uids existed gets
    /// one the first time a schedule is registered against it.
    fn jar_uid(env: &Env, jar_id: &String) -> u128 {
        match env
            .storage()
            .persistent()
            .get::<_, u128>(&DataKey::JarUid(jar_id.clone()))
        {
            Some(uid) => uid,
            None => Self::assign_jar_uid(env, jar_id),
        }
    }

    /// Store a jar and extend its TTL. Every jar write goes through here (#327).
    fn store_jar(env: &Env, jar_id: &String, jar: &Jar) {
        let key = DataKey::Jar(jar_id.clone());
        env.storage().persistent().set(&key, jar);
        env.storage()
            .persistent()
            .extend_ttl(&key, JAR_TTL_THRESHOLD, JAR_TTL_LEDGERS);
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
        let token_addr: Address = Self::get_token(env.clone());

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

// Rendered by `cargo doc` only (`cfg(doc)`): the examples never reach the wasm
// build, so they cannot grow `contractspecv0` or trip `check-doc-length.sh`
// and `check-wasm-size.sh`. The blocks are fenced `ignore` and the crate sets
// `doctest = false`, so `cargo test` never compiles them (#59).
#[cfg(doc)]
pub mod examples {
    //! One example per public function of [`TipSplitter`](crate::TipSplitter).
    //!
    //! Every snippet uses the generated `TipSplitterClient` and the names
    //! defined under "Setup". Amounts are `i128` token base units (USDC has 7
    //! decimals, so `10_000_000` is 1 USDC). A snippet that panics on bad input
    //! has a `try_` twin on the client that returns the typed `Error` instead.
    //!
    //! # Setup
    //!
    //! ```ignore
    //! use soroban_sdk::{testutils::Address as _, vec, Address, BytesN, Env, String, Vec};
    //! use tip_splitter::{Split, TipRequest, TipSplitter, TipSplitterClient};
    //!
    //! let env = Env::default();
    //! env.mock_all_auths();
    //! let admin = Address::generate(&env);
    //! let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    //! // The constructor runs at registration: `(admin, default_token)`.
    //! let contract_id = env.register(TipSplitter, (admin.clone(), token.clone()));
    //! let client = TipSplitterClient::new(&env, &contract_id);
    //!
    //! let alice = Address::generate(&env);
    //! let bob = Address::generate(&env);
    //! let supporter = Address::generate(&env);
    //! let jar_id = String::from_str(&env, "@alice");
    //! let note = String::from_str(&env, "thanks!");
    //! ```
    //!
    //! # Building the split vector for `create_jar`
    //!
    //! A split is a `Split { to, bps }`: the recipient and their share in basis
    //! points (`10_000` = 100%). The vector must hold 1 to 20 entries, every
    //! `bps` must be in `1..=10_000`, the entries must sum to exactly `10_000`,
    //! and no address may appear twice. Order is kept and it matters: the last
    //! entry absorbs the rounding remainder of every tip (see "Splitting rules"
    //! in `docs/CONTRACT.md`).
    //!
    //! ```ignore
    //! let splits: Vec<Split> = vec![
    //!     &env,
    //!     Split { to: alice.clone(), bps: 7_000 }, // 70%
    //!     Split { to: bob.clone(), bps: 3_000 },   // 30%, last: takes the remainder
    //! ];
    //! client.create_jar(&alice, &jar_id, &splits);
    //! ```
    //!
    //! A single recipient is `vec![&env, Split { to: alice.clone(), bps: 10_000 }]`.
    //! Outside a test, build the same vector from the caller's own `Env`; the
    //! `Vec` is the SDK type, not `std::vec::Vec`.
    //!
    //! Tipping needs a funded supporter. In a test, mint with the asset admin:
    //!
    //! ```ignore
    //! soroban_sdk::token::StellarAssetClient::new(&env, &token).mint(&supporter, &100_000_000);
    //! ```
    //!
    //! # Setup and administration
    //!
    //! `__constructor(admin, token)` runs once, at deployment. It is not called
    //! through the client; pass its arguments to `register` (or to
    //! `stellar contract deploy -- --admin <ADDR> --token <ADDR>`).
    //!
    //! ```ignore
    //! let contract_id = env.register(TipSplitter, (admin.clone(), token.clone()));
    //! ```
    //!
    //! `allow_token(token)` (admin) adds a token that `tip_in_token` accepts.
    //! `disallow_token(token)` (admin) removes one; the default token cannot be
    //! removed.
    //!
    //! ```ignore
    //! let other = env.register_stellar_asset_contract_v2(admin.clone()).address();
    //! client.allow_token(&other);
    //! assert!(client.is_token_allowed(&other));
    //! client.disallow_token(&other);
    //! ```
    //!
    //! `transfer_admin(new_admin)` (admin) hands the admin role over.
    //!
    //! ```ignore
    //! client.transfer_admin(&bob);
    //! assert_eq!(client.get_admin(), bob);
    //! ```
    //!
    //! `pause()` and `resume()` (admin) stop and restart `tip`, `tip_with_ref`,
    //! `tip_in_token` and `tip_many`;
    //! `is_paused()` reports the flag.
    //!
    //! ```ignore
    //! client.pause();
    //! assert!(client.is_paused());
    //! client.resume();
    //! ```
    //!
    //! `upgrade(new_wasm_hash)` (admin) swaps the contract code for an uploaded
    //! wasm; `migrate()` (admin) then moves stored state to that build's schema
    //! and is safe to call again.
    //!
    //! ```ignore
    //! // `wasm` is the new build's bytes; the hash comes from uploading it.
    //! let new_hash: BytesN<32> = env.deployer().upload_contract_wasm(wasm);
    //! client.upgrade(&new_hash);
    //! client.migrate();
    //! ```
    //!
    //! `set_fee_bps(fee_bps)` (admin) sets the protocol fee in basis points
    //! (`0..=500`); `get_fee_bps()` reads it.
    //!
    //! ```ignore
    //! client.set_fee_bps(&100); // 1%, taken from each tip before the split
    //! assert_eq!(client.get_fee_bps(), 100);
    //! ```
    //!
    //! `set_tip_hook(hook)` and `set_registrar(registrar)` (admin) take an
    //! `Option<Address>`: `Some` installs the contract, `None` clears it.
    //! `get_tip_hook()` and `get_registrar()` read them back.
    //!
    //! ```ignore
    //! client.set_tip_hook(&Some(hook_contract.clone()));
    //! assert_eq!(client.get_tip_hook(), Some(hook_contract));
    //! client.set_registrar(&None);
    //! assert_eq!(client.get_registrar(), None);
    //! ```
    //!
    //! # Creating and managing jars
    //!
    //! `create_jar(owner, jar_id, splits)`: `jar_id` is a slug, `@` followed by
    //! 3 to 32 of `a-z`, `0-9`, `_`, `-`. The owner must authorize, and the
    //! registrar too while one is set.
    //!
    //! ```ignore
    //! let splits = vec![
    //!     &env,
    //!     Split { to: alice.clone(), bps: 5_000 },
    //!     Split { to: bob.clone(), bps: 5_000 },
    //! ];
    //! client.create_jar(&alice, &jar_id, &splits);
    //! ```
    //!
    //! `update_splits(jar_id, splits)` (owner) replaces the whole vector, which
    //! is validated like `create_jar`'s. On a locked jar, every recipient whose
    //! share drops or who is removed must also authorize.
    //!
    //! ```ignore
    //! let new_splits = vec![
    //!     &env,
    //!     Split { to: alice.clone(), bps: 6_000 },
    //!     Split { to: bob.clone(), bps: 4_000 },
    //! ];
    //! client.update_splits(&jar_id, &new_splits);
    //! ```
    //!
    //! `lock_splits(jar_id)` (owner) arms the consent rule above;
    //! `unlock_splits(jar_id)` needs the owner and every current recipient;
    //! `is_splits_locked(jar_id)` reads the flag.
    //!
    //! ```ignore
    //! client.lock_splits(&jar_id);
    //! assert!(client.is_splits_locked(&jar_id));
    //! client.unlock_splits(&jar_id);
    //! ```
    //!
    //! A recipient who cannot sign is removed from a locked jar in two steps:
    //! the owner announces it with `request_recipient_removal(jar_id,
    //! recipient)`, and after `RECIPIENT_REMOVAL_DELAY_LEDGERS` runs
    //! `execute_recipient_removal(jar_id)`. The recipient (or the owner) can
    //! call `cancel_recipient_removal(jar_id, caller)` in between.
    //!
    //! ```ignore
    //! client.request_recipient_removal(&jar_id, &lost_key);
    //! // ... 120_960 ledgers later ...
    //! client.execute_recipient_removal(&jar_id);
    //! ```
    //!
    //! `set_jar_paused(jar_id, paused)` (owner) pauses or resumes tipping into
    //! one jar; `is_jar_paused(jar_id)` reads the flag.
    //!
    //! ```ignore
    //! client.set_jar_paused(&jar_id, &true);
    //! assert!(client.is_jar_paused(&jar_id));
    //! ```
    //!
    //! `leave_jar(jar_id, recipient)`: a recipient removes themselves, and the
    //! last remaining entry absorbs their share. A sole recipient cannot leave.
    //!
    //! ```ignore
    //! client.leave_jar(&jar_id, &bob);
    //! ```
    //!
    //! `transfer_jar_ownership(jar_id, new_owner)` needs the current owner and
    //! the new owner to authorize.
    //!
    //! ```ignore
    //! client.transfer_jar_ownership(&jar_id, &bob);
    //! assert_eq!(client.get_jar_owner(&jar_id), bob);
    //! ```
    //!
    //! `rename_jar(jar_id, new_jar_id)` (owner) moves a jar to a free slug.
    //! `delete_jar(jar_id)` (owner) removes the jar and its settings.
    //!
    //! ```ignore
    //! let renamed = String::from_str(&env, "@alice-music");
    //! client.rename_jar(&jar_id, &renamed);
    //! client.delete_jar(&renamed);
    //! ```
    //!
    //! `set_min_tip_amount(jar_id, min_amount)` and
    //! `set_max_tip_amount(jar_id, max_amount)` (owner) take an `Option<i128>`:
    //! `Some(positive amount)` sets the bound, `None` clears it, and the minimum
    //! may not exceed the maximum.
    //!
    //! ```ignore
    //! client.set_min_tip_amount(&jar_id, &Some(100_000));
    //! client.set_max_tip_amount(&jar_id, &Some(50_000_000));
    //! client.set_min_tip_amount(&jar_id, &None);
    //! ```
    //!
    //! # Tipping
    //!
    //! `tip(from, jar_id, amount, message)` splits `amount` across the jar's
    //! recipients and returns the tip's sequence number. `from` authorizes.
    //!
    //! ```ignore
    //! let seq: u64 = client.tip(&supporter, &jar_id, &10_000_000, &note);
    //! ```
    //!
    //! `tip_with_ref(from, jar_id, amount, message, reference)` also echoes an
    //! opaque 32-byte client reference in the `tip` event.
    //!
    //! ```ignore
    //! let reference: BytesN<32> = BytesN::from_array(&env, &[7u8; 32]);
    //! client.tip_with_ref(&supporter, &jar_id, &10_000_000, &note, &Some(reference));
    //! ```
    //!
    //! `tip_in_token(from, jar_id, token, amount, message)` tips in an allowed
    //! token other than the default.
    //!
    //! ```ignore
    //! client.tip_in_token(&supporter, &jar_id, &other, &10_000_000, &note);
    //! ```
    //!
    //! `tip_many(from, tips, token)` tips several jars in one all-or-nothing
    //! call (1 to 10 entries). `token` is `None` for the default token.
    //!
    //! ```ignore
    //! let tips = vec![
    //!     &env,
    //!     TipRequest {
    //!         jar_id: jar_id.clone(),
    //!         amount: 10_000_000,
    //!         message: note.clone(),
    //!         reference: None,
    //!     },
    //! ];
    //! let seqs: Vec<u64> = client.tip_many(&supporter, &tips, &None);
    //! ```
    //!
    //! `withdraw_held(recipient, token)` pays out a share that could not be
    //! delivered at tip time; `get_held(recipient, token)` reports what is owed.
    //!
    //! ```ignore
    //! if client.get_held(&bob, &token) > 0 {
    //!     client.withdraw_held(&bob, &token);
    //! }
    //! ```
    //!
    //! # Recurring tips
    //!
    //! `create_recurring(supporter, jar_id, amount, period_ledgers, message)`
    //! returns the schedule id. The supporter first approves the contract as a
    //! spender of the token (`approve(from, spender, amount, expiration_ledger)`).
    //!
    //! ```ignore
    //! let tok = soroban_sdk::token::Client::new(&env, &token);
    //! tok.approve(&supporter, &contract_id, &50_000_000, &(env.ledger().sequence() + 100_000));
    //! let id: u128 = client.create_recurring(&supporter, &jar_id, &1_000_000, &17_280, &note);
    //! ```
    //!
    //! `update_recurring(id, amount, message)` (supporter) changes the amount and
    //! message; `get_recurring(id)` reads the schedule; `cancel_recurring(caller,
    //! id)` removes it.
    //!
    //! ```ignore
    //! client.update_recurring(&id, &2_000_000, &note);
    //! let schedule = client.get_recurring(&id); // Recurring { amount, next_due_ledger, .. }
    //! client.cancel_recurring(&supporter, &id);
    //! ```
    //!
    //! `collect_recurring(id)` is callable by anyone once the schedule is due; an
    //! early call fails with `NotDue`.
    //!
    //! ```ignore
    //! client.collect_recurring(&id);
    //! ```
    //!
    //! # Reading jars and previews
    //!
    //! ```ignore
    //! let jar = client.get_jar(&jar_id);                 // Jar { owner, splits, locked, paused }
    //! let owner = client.get_jar_owner(&jar_id);
    //! let n: u32 = client.get_split_count(&jar_id);
    //! let exists: bool = client.jar_exists(&jar_id);
    //! let listed: bool = client.is_recipient(&jar_id, &bob);
    //! let info = client.get_jar_info(&jar_id);           // owner, splits, limits and totals in one call
    //! ```
    //!
    //! `preview_split(jar_id, amount)` returns one share per recipient, in split
    //! order, from the same code `tip` pays with. For a jar created with the 70/30
    //! split vector from the section above:
    //!
    //! ```ignore
    //! let shares: Vec<i128> = client.preview_split(&jar_id, &101);
    //! assert_eq!(shares, vec![&env, 70, 31]); // the last entry takes the remainder
    //! ```
    //!
    //! `preview_split_details(jar_id, amount)` pairs each share with its
    //! recipient. `preview_tip(jar_id, from, amount)` reports what a given
    //! sender actually moves: their own entry is `0`.
    //!
    //! ```ignore
    //! let details = client.preview_split_details(&jar_id, &101); // [PreviewSplit { to, amount }, ..]
    //! let paid = client.preview_tip(&jar_id, &bob, &101);        // bob tips: [70, 0]
    //! ```
    //!
    //! `get_min_tip_amount(jar_id)` and `get_max_tip_amount(jar_id)` read the
    //! owner's bounds (`None` when unset). `get_min_accepted_tip(jar_id)` is the
    //! smallest amount the jar accepts at all, after the fee and the minimum.
    //!
    //! ```ignore
    //! let floor: i128 = client.get_min_accepted_tip(&jar_id); // 4 for that 70/30 jar, no fee
    //! let min: Option<i128> = client.get_min_tip_amount(&jar_id);
    //! let max: Option<i128> = client.get_max_tip_amount(&jar_id);
    //! ```
    //!
    //! # Totals and contract-wide reads
    //!
    //! ```ignore
    //! let stats = client.get_jar_stats(&jar_id);                        // JarStats { tip_count, total }
    //! let mine = client.get_supporter_stats(&jar_id, &supporter);       // SupporterStats { tip_count, total }
    //! let count: u64 = client.get_tip_count();
    //! let v: u32 = client.version();                                    // INTERFACE_VERSION
    //! let tokens: Vec<Address> = client.get_allowed_tokens();           // default token first
    //! let ok: bool = client.is_token_allowed(&token);
    //! let admin_now: Address = client.get_admin();
    //! let default_token: Address = client.get_token();
    //! let limits = client.get_limits();                                 // max_recipients, max_batch_tips, ..
    //! ```
}

mod test;
