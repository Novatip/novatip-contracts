//! Read-only views and previews.

use super::*;
use soroban_sdk::vec;

/// `jar_exists` flips from false to true on registration, and matches the slug
/// exactly — a prefix or a different slug must not read as taken.
#[test]
fn jar_exists_tracks_registration() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@alice");

    assert!(
        !client.jar_exists(&jar_id),
        "slug must be free before registration"
    );

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

    assert!(
        client.jar_exists(&jar_id),
        "slug must be taken after registration"
    );
    assert!(
        !client.jar_exists(&String::from_str(env, "@bob")),
        "an unrelated slug must still be free"
    );
    // The onboarding check relies on exact matching: "@ali" is its own slug.
    assert!(
        !client.jar_exists(&String::from_str(env, "@ali")),
        "a prefix of a taken slug must still be free"
    );
}

/// A `create_jar` that fails validation must leave the slug free — otherwise
/// the availability check would report a name as taken that nobody owns.
#[test]
fn jar_exists_is_false_for_rejected_jar() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@rejected");

    // 6000 + 3000 = 9000, so the jar is never stored.
    let res = client.try_create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 6000,
            },
            Split {
                to: bob.clone(),
                bps: 3000,
            },
        ],
    );
    assert!(res.is_err());

    assert!(!client.jar_exists(&jar_id));
}

/// `update_splits` rewrites the same key, so the slug must stay registered.
#[test]
fn jar_exists_stays_true_after_update_splits() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@steady");

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
    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 5000,
            },
            Split {
                to: bob.clone(),
                bps: 5000,
            },
        ],
    );

    assert!(client.jar_exists(&jar_id));
}

/// `get_jar_owner` must agree with `get_jar(...).owner` for a registered jar,
/// which is the whole point: the cheap read and the full read never disagree.
#[test]
fn get_jar_owner_returns_owner_of_registered_jar() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@duo");

    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 6000,
            },
            Split {
                to: bob.clone(),
                bps: 4000,
            },
        ],
    );

    assert_eq!(client.get_jar_owner(&jar_id), owner);
    assert_eq!(client.get_jar_owner(&jar_id), client.get_jar(&jar_id).owner);
    // The owner is distinct from the recipients — a jar can pay addresses that
    // do not control it, so returning a recipient would pass a weaker test.
    assert_ne!(client.get_jar_owner(&jar_id), alice);
}

/// An unregistered slug is a `JarNotFound` panic, not a default address — a
/// zero or placeholder address here would read to a client as "someone owns
/// this jar".
#[test]
fn get_jar_owner_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_get_jar_owner(&String::from_str(env, "@ghost"));
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// `transfer_jar_ownership` rewrites the stored owner, so the cheap read must
/// follow it rather than serving a stale address.
#[test]
fn get_jar_owner_follows_ownership_transfer() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let new_owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@moved");

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
    assert_eq!(client.get_jar_owner(&jar_id), owner);

    client.transfer_jar_ownership(&jar_id, &new_owner);

    assert_eq!(client.get_jar_owner(&jar_id), new_owner);
    assert_ne!(client.get_jar_owner(&jar_id), owner);
}

/// A single-recipient jar counts 1 — the degenerate case a badge still has to
/// render, and the one an off-by-one would most likely get wrong.
#[test]
fn get_split_count_counts_single_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@solo");

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

    assert_eq!(client.get_split_count(&jar_id), 1);
}

/// A multi-recipient jar counts every entry, and agrees with the length of the
/// vector `get_jar` returns — the cheap read must not drift from the full one.
#[test]
fn get_split_count_counts_every_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let jar_id = String::from_str(env, "@collective");

    // Four recipients at 2500 bps each.
    let mut splits = vec![env];
    for _ in 0..4 {
        splits.push_back(Split {
            to: Address::generate(env),
            bps: 2500,
        });
    }
    client.create_jar(&owner, &jar_id, &splits);

    assert_eq!(client.get_split_count(&jar_id), 4);
    assert_eq!(
        client.get_split_count(&jar_id),
        client.get_jar(&jar_id).splits.len()
    );
}

/// `update_splits` replaces the stored vector, so the count must follow it both
/// upwards and back down rather than serving a cached length.
#[test]
fn get_split_count_follows_update_splits() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    let jar_id = String::from_str(env, "@growing");

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
    assert_eq!(client.get_split_count(&jar_id), 1);

    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 4000,
            },
            Split {
                to: bob.clone(),
                bps: 4000,
            },
            Split {
                to: carol.clone(),
                bps: 2000,
            },
        ],
    );
    assert_eq!(client.get_split_count(&jar_id), 3);

    // Shrinking the list must lower the count too.
    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 5000,
            },
            Split {
                to: bob.clone(),
                bps: 5000,
            },
        ],
    );
    assert_eq!(client.get_split_count(&jar_id), 2);
}

/// An unregistered slug is a `JarNotFound` panic, not `0` — a zero count would
/// be indistinguishable from a jar, and no stored jar can have zero splits.
#[test]
fn get_split_count_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_get_split_count(&String::from_str(env, "@ghost"));
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// The preview is the payout: the numbers a client shows before signing must
/// be the balances the tip actually produces.
#[test]
fn preview_split_matches_what_tip_pays() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token = token::Client::new(env, &s.token);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@preview");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 7000,
            },
            Split {
                to: bob.clone(),
                bps: 3000,
            },
        ],
    );

    let preview = client.preview_split(&jar_id, &101);
    assert_eq!(preview, vec![env, 70_i128, 31_i128]);

    client.tip(
        &tipper,
        &jar_id,
        &101,
        &String::from_str(env, "here you go"),
    );

    assert_eq!(token.balance(&alice), preview.get(0).unwrap());
    assert_eq!(token.balance(&bob), preview.get(1).unwrap());
}

/// A single-recipient jar previews the whole amount, with no dust to place.
#[test]
fn preview_split_single_recipient_gets_everything() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@onlyme");

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

    assert_eq!(client.preview_split(&jar_id, &7), vec![env, 7_i128]);
}

/// Thirds over amounts that do not divide cleanly: the shares must still sum to
/// exactly the amount, with the truncated remainder landing on the last
/// recipient. These are the amounts where a client's own arithmetic would
/// most easily drift from the contract's.
#[test]
fn preview_split_shares_sum_to_amount_for_awkward_amounts() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let jar_id = String::from_str(env, "@thirds");

    // 3333 / 3333 / 3334 — the classic non-terminating split.
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 3333,
            },
            Split {
                to: Address::generate(env),
                bps: 3333,
            },
            Split {
                to: Address::generate(env),
                bps: 3334,
            },
        ],
    );

    for amount in [10_i128, 11, 99, 100, 101, 7_777, 1_000_003] {
        let shares = client.preview_split(&jar_id, &amount);
        assert_eq!(shares.len(), 3, "one share per recipient");

        let mut total: i128 = 0;
        for i in 0..shares.len() {
            let share = shares.get(i).unwrap();
            assert!(share > 0, "every recipient must get something at {amount}");
            total += share;
        }
        assert_eq!(total, amount, "shares must sum to exactly {amount}");
    }

    // Spot-check the dust placement rather than only the invariant: at 100,
    // truncation leaves 1 over and the final recipient absorbs it.
    assert_eq!(
        client.preview_split(&jar_id, &100),
        vec![env, 33_i128, 33_i128, 34_i128]
    );
}

/// The preview must follow `update_splits`, or a client would quote shares for
/// a line-up the contract no longer has.
#[test]
fn preview_split_follows_update_splits() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@requoted");

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
    assert_eq!(client.preview_split(&jar_id, &100), vec![env, 100_i128]);

    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 2500,
            },
            Split {
                to: bob.clone(),
                bps: 7500,
            },
        ],
    );
    assert_eq!(
        client.preview_split(&jar_id, &100),
        vec![env, 25_i128, 75_i128]
    );
}

/// A preview that returns at all must describe a tip that can go through, so
/// it rejects exactly the amounts `tip` rejects.
#[test]
fn preview_split_rejects_amounts_tip_would_reject() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@strict");

    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 9900,
            },
            Split {
                to: bob.clone(),
                bps: 100,
            },
        ],
    );

    assert_eq!(
        client.try_preview_split(&jar_id, &0),
        Err(Ok(Error::AmountZero.into()))
    );
    assert_eq!(
        client.try_preview_split(&jar_id, &-5),
        Err(Ok(Error::AmountNegative.into()))
    );
    // bps 100 truncates to zero below 100, which `tip` refuses to pay.
    assert_eq!(
        client.try_preview_split(&jar_id, &99),
        Err(Ok(Error::SplitAmountTooSmall.into()))
    );
    assert!(client
        .try_tip(
            &Address::generate(env),
            &jar_id,
            &99,
            &String::from_str(env, "too small")
        )
        .is_err());
    // 100 is the first amount that pays everyone.
    assert_eq!(
        client.preview_split(&jar_id, &100),
        vec![env, 99_i128, 1_i128]
    );
}

/// An unregistered slug is a `JarNotFound` panic, matching `get_jar` — an
/// empty vector would read as a jar that pays nobody.
#[test]
fn preview_split_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_preview_split(&String::from_str(env, "@ghost"), &100);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// A collaborator's own membership check: a listed recipient gets `true`, an
/// address nowhere in the splits gets `false`, and the jar owner gets `false`
/// when they take no share — owning a jar and being paid by it are separate
/// things.
#[test]
fn is_recipient_distinguishes_recipients_owner_and_strangers() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let stranger = Address::generate(env);

    let jar_id = String::from_str(env, "@collab");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 6000,
            },
            Split {
                to: bob.clone(),
                bps: 4000,
            },
        ],
    );

    // Listed recipients, in both the first and a later position.
    assert!(client.is_recipient(&jar_id, &alice));
    assert!(client.is_recipient(&jar_id, &bob));

    // An address that is nowhere in the jar.
    assert!(!client.is_recipient(&jar_id, &stranger));

    // The owner, who takes no share of a tip here.
    assert!(
        !client.is_recipient(&jar_id, &owner),
        "owning a jar is not the same as receiving a share of its tips"
    );
    assert_eq!(
        client.get_jar(&jar_id).owner,
        owner,
        "the owner check lives on get_jar, not is_recipient"
    );
}

/// An owner who is also listed in the splits gets `true` — the previous test
/// asserts a `false` for an owner, so this pins down that the answer tracks the
/// split vector rather than ownership.
#[test]
fn is_recipient_is_true_for_an_owner_who_is_also_a_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let jar_id = String::from_str(env, "@solo");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: owner.clone(),
                bps: 10000,
            },
        ],
    );

    assert!(client.is_recipient(&jar_id, &owner));
}

/// `update_splits` replaces the recipient list, so membership must follow it —
/// a dropped collaborator stops being a recipient and a new one starts.
#[test]
fn is_recipient_follows_update_splits() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);

    let jar_id = String::from_str(env, "@reshuffle");
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
    assert!(client.is_recipient(&jar_id, &alice));
    assert!(!client.is_recipient(&jar_id, &bob));

    client.update_splits(
        &jar_id,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );
    assert!(!client.is_recipient(&jar_id, &alice));
    assert!(client.is_recipient(&jar_id, &bob));
}

/// An unregistered slug is `JarNotFound`, not a quiet `false` — otherwise a
/// typo'd jar id would look indistinguishable from a genuine non-membership.
#[test]
fn is_recipient_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let alice = Address::generate(env);
    let res = client.try_is_recipient(&String::from_str(env, "@nope"), &alice);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// `get_limits` exists so clients stop hardcoding the contract's bounds. This
/// asserts the view and the constants cannot drift apart: changing one without
/// the other fails here.
#[test]
fn get_limits_matches_contract_constants() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let limits = client.get_limits();

    assert_eq!(limits.bps_denom, BPS_DENOM);
    assert_eq!(limits.max_recipients, MAX_RECIPIENTS);
    assert_eq!(limits.max_message_len, MAX_MESSAGE_LEN);
    assert_eq!(limits.max_batch_tips, MAX_BATCH_TIPS);
    assert_eq!(limits.max_jar_id_len, MAX_JAR_SLUG_LEN + 1);
    assert_eq!(limits.min_jar_id_len, MIN_JAR_SLUG_LEN + 1);
    assert_eq!(limits.max_fee_bps, MAX_FEE_BPS);
    assert_eq!(
        limits,
        Limits {
            bps_denom: 10_000,
            max_recipients: 20,
            max_message_len: 280,
            max_batch_tips: 10,
            max_jar_id_len: 33,
            min_jar_id_len: 4,
            max_fee_bps: 500,
        },
        "the published limits are part of the public interface; changing one \
         means clients and docs/CONTRACT.md have to change with it"
    );
}

/// The limits are only useful if they describe what the contract actually
/// enforces, so exercise each returned bound against the rejection it predicts.
#[test]
fn get_limits_describes_the_enforced_bounds() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let limits = client.get_limits();
    let owner = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    // max_recipients: a jar with exactly that many recipients is accepted, and
    // one more is not.
    let mut splits = vec![env];
    for _ in 0..limits.max_recipients {
        splits.push_back(Split {
            to: Address::generate(env),
            bps: limits.bps_denom / limits.max_recipients,
        });
    }
    let jar_id = String::from_str(env, "@limits");
    client.create_jar(&owner, &jar_id, &splits);

    let mut too_many = splits.clone();
    too_many.push_back(Split {
        to: Address::generate(env),
        bps: 1,
    });
    assert_eq!(
        client.try_create_jar(&owner, &String::from_str(env, "@over"), &too_many),
        Err(Ok(Error::TooManyRecipients.into()))
    );

    // bps_denom: the shares of an accepted jar sum to exactly the denominator.
    let stored = client.get_jar(&jar_id);
    let total: u32 = (0..stored.splits.len())
        .map(|i| stored.splits.get(i).unwrap().bps)
        .sum();
    assert_eq!(total, limits.bps_denom);

    // max_jar_id_len: the longest canonical id ("@" plus a full-length body)
    // is accepted, and one byte more is not.
    let mut exact_bytes = vec_of_bytes(limits.max_jar_id_len - 1);
    exact_bytes.insert(0, b'@');
    let exact_id = String::from_bytes(env, &exact_bytes);
    client.create_jar(&owner, &exact_id, &splits);

    let mut over_bytes = vec_of_bytes(limits.max_jar_id_len);
    over_bytes.insert(0, b'@');
    let over_id = String::from_bytes(env, &over_bytes);
    assert_eq!(
        client.try_create_jar(&owner, &over_id, &splits),
        Err(Ok(Error::InvalidJarId.into()))
    );

    // max_message_len: a message of exactly that many bytes is accepted, and
    // one byte more is not.
    let exact = String::from_bytes(env, &vec_of_bytes(limits.max_message_len));
    client.tip(&tipper, &jar_id, &1_000, &exact);

    let over = String::from_bytes(env, &vec_of_bytes(limits.max_message_len + 1));
    assert_eq!(
        client.try_tip(&tipper, &jar_id, &1, &over),
        Err(Ok(Error::MessageTooLong.into()))
    );

    // max_batch_tips: a batch of exactly that many entries is accepted, and one
    // more is not. The batch needs its own jar: one recipient on the whole
    // 10 000 bps, so a 1-stroop tip is above the truncation floor.
    let batch_jar = String::from_str(env, "@batch");
    client.create_jar(
        &owner,
        &batch_jar,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: limits.bps_denom,
            },
        ],
    );
    let batch_count = limits.max_batch_tips + 1;
    token_admin.mint(&tipper, &(batch_count as i128));

    let exact_batch = batch_of(env, &batch_jar, limits.max_batch_tips);
    assert_eq!(
        client.tip_many(&tipper, &exact_batch, &None).len(),
        limits.max_batch_tips
    );

    let over_batch = batch_of(env, &batch_jar, batch_count);
    assert_eq!(
        client.try_tip_many(&tipper, &over_batch, &None),
        Err(Ok(Error::BatchTooLarge.into()))
    );
}

/// `n` filler ASCII bytes — one UTF-8 byte each, so the byte length is `n`.
fn vec_of_bytes(n: u32) -> std::vec::Vec<u8> {
    std::vec![b'a'; n as usize]
}

#[test]
fn get_admin_returns_constructor_admin() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert_eq!(client.get_admin(), s.admin);
}

/// `get_token` returns the token address passed to the constructor (#71).
#[test]
fn get_token_returns_constructor_token() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert_eq!(client.get_token(), s.token);
}

#[test]
fn get_min_tip_amount_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let jar_id = String::from_str(env, "@ghost");
    let res = client.try_get_min_tip_amount(&jar_id);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

#[test]
fn preview_split_enforces_minimum_tip_amount() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@previewmin");
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

    client.set_min_tip_amount(&jar_id, &Some(100_i128));

    // Preview below minimum must fail with BelowMinTipAmount
    let res_low = client.try_preview_split(&jar_id, &99_i128);
    assert_eq!(res_low, Err(Ok(Error::BelowMinTipAmount.into())));

    // Preview at exact minimum succeeds
    let res_exact = client.try_preview_split(&jar_id, &100_i128);
    assert!(res_exact.is_ok());
}

/// #97: `preview_split` must apply the amount-sign check before the jar's
/// minimum, exactly as `tip` does, so a client previewing then tipping gets
/// one consistent error instead of "below the minimum" from one and "enter a
/// positive amount" from the other.
#[test]
fn preview_split_and_tip_agree_on_error_order() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    let jar_id = String::from_str(env, "@previeword");
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
    client.set_min_tip_amount(&jar_id, &Some(100_i128));
    let message = String::from_str(env, "");

    for (amount, expected) in [
        (0_i128, Error::AmountZero),
        (-5_i128, Error::AmountNegative),
    ] {
        let preview = client.try_preview_split(&jar_id, &amount).unwrap_err();
        let tip = client
            .try_tip(&tipper, &jar_id, &amount, &message)
            .unwrap_err();
        assert_eq!(
            preview,
            Ok(expected.into()),
            "a non-positive amount is a sign error even when a minimum is set"
        );
        assert_eq!(
            preview, tip,
            "preview_split and tip must report the same error for {amount}"
        );
    }

    // Between 1 and the minimum, both still report the minimum rather than a
    // sign error: the order change must not swallow the minimum check.
    let preview = client.try_preview_split(&jar_id, &99_i128).unwrap_err();
    let tip = client
        .try_tip(&tipper, &jar_id, &99_i128, &message)
        .unwrap_err();
    assert_eq!(preview, Ok(Error::BelowMinTipAmount.into()));
    assert_eq!(preview, tip);
}

/// The sign check is unconditional: it applies to a jar with no minimum too,
/// where `preview_split` previously reached `compute_shares` and reported the
/// same codes only incidentally.
#[test]
fn preview_split_rejects_non_positive_amounts_without_a_minimum() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@previewzero");
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

    assert_eq!(
        client.try_preview_split(&jar_id, &0_i128),
        Err(Ok(Error::AmountZero.into()))
    );
    assert_eq!(
        client.try_preview_split(&jar_id, &-5_i128),
        Err(Ok(Error::AmountNegative.into()))
    );
}

// #319 — get_jar_info matches the individual views.
#[test]
fn get_jar_info_matches_the_individual_views() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@info");
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

    let info = client.get_jar_info(&jar_id);
    let jar = client.get_jar(&jar_id);
    let stats = client.get_jar_stats(&jar_id);
    assert_eq!(info.owner, jar.owner);
    assert_eq!(info.splits, jar.splits);
    assert_eq!(info.locked, jar.locked);
    assert_eq!(info.paused, client.is_jar_paused(&jar_id));
    assert_eq!(info.min_tip_amount, client.get_min_tip_amount(&jar_id));
    assert_eq!(info.min_accepted_tip, client.get_min_accepted_tip(&jar_id));
    assert_eq!(info.tip_count, stats.tip_count);
    assert_eq!(info.total, stats.total);
    assert_eq!(info.fee_bps, client.get_fee_bps());

    let res = client.try_get_jar_info(&String::from_str(env, "@nope"));
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}
// ---------------------------------------------------------------------------
// #167 — per-jar running tip totals
// ---------------------------------------------------------------------------

/// A jar exists before it has been tipped, so its counters must read zero
/// rather than `JarNotFound`: "nobody has tipped this yet" is a real answer a
/// tip page shows.
#[test]
fn get_jar_stats_is_zero_before_the_first_tip() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let jar_id = String::from_str(env, "@untouched");
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

    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 0,
            total: 0
        }
    );
}

/// Several tips to one jar accumulate into that jar, in both fields.
#[test]
fn get_jar_stats_counts_several_tips_to_one_jar() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let one = Address::generate(env);
    let two = Address::generate(env);
    token_admin.mint(&one, &1_000);
    token_admin.mint(&two, &1_000);

    let jar_id = String::from_str(env, "@tally");
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

    client.tip(&one, &jar_id, &100, &String::from_str(env, "a"));
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 1,
            total: 100
        }
    );

    client.tip(&two, &jar_id, &250, &String::from_str(env, "b"));
    client.tip(&one, &jar_id, &50, &String::from_str(env, "c"));
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 3,
            total: 400
        },
        "the jar total counts every supporter, not one supporter's tips"
    );
}

/// Two jars tipped by the same person must not share counters — a single
/// running total would make one creator's leaderboard out of another's tips.
#[test]
fn get_jar_stats_does_not_leak_between_jars() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &10_000);

    let splits = vec![
        env,
        Split {
            to: alice.clone(),
            bps: 10000,
        },
    ];
    let first = String::from_str(env, "@first");
    let second = String::from_str(env, "@second");
    client.create_jar(&owner, &first, &splits);
    client.create_jar(
        &owner,
        &second,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );

    client.tip(&tipper, &first, &700, &String::from_str(env, "one"));
    client.tip(&tipper, &first, &300, &String::from_str(env, "two"));
    client.tip(&tipper, &second, &50, &String::from_str(env, "three"));

    assert_eq!(
        client.get_jar_stats(&first),
        JarStats {
            tip_count: 2,
            total: 1000
        }
    );
    assert_eq!(
        client.get_jar_stats(&second),
        JarStats {
            tip_count: 1,
            total: 50
        }
    );
}

/// A typo'd slug must not look like an untouched jar.
#[test]
fn get_jar_stats_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let res = client.try_get_jar_stats(&String::from_str(env, "@ghost"));
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// The counters are only an authoritative check on the indexer if a tip that
/// never settled never reaches them. A transfer that traps reverts the whole
/// call, including the stats writes.
#[test]
fn reverted_tip_leaves_both_totals_unchanged() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@revert");
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

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "ok"));

    // More than the remaining balance: the transfer traps.
    let res = client.try_tip(&tipper, &jar_id, &5_000, &String::from_str(env, "nope"));
    assert!(res.is_err(), "an unfundable tip must revert");

    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 1,
            total: 100
        }
    );
    assert_eq!(
        client.get_supporter_stats(&jar_id, &tipper),
        SupporterStats {
            tip_count: 1,
            total: 100
        }
    );
}

// ---------------------------------------------------------------------------
// #168 — per-supporter totals, the data a badge contract needs
// ---------------------------------------------------------------------------

/// A milestone check for someone who has never tipped must read zeros, not
/// fail — the badge contract asks this of arbitrary addresses.
#[test]
fn get_supporter_stats_is_zero_for_an_address_that_never_tipped() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let supporter = Address::generate(env);
    let stranger = Address::generate(env);
    token_admin.mint(&supporter, &1_000);

    let jar_id = String::from_str(env, "@badges");
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

    assert_eq!(
        client.get_supporter_stats(&jar_id, &stranger),
        SupporterStats {
            tip_count: 0,
            total: 0
        }
    );

    client.tip(&supporter, &jar_id, &100, &String::from_str(env, "hi"));
    assert_eq!(
        client.get_supporter_stats(&jar_id, &stranger),
        SupporterStats {
            tip_count: 0,
            total: 0
        },
        "someone else's tip must not move a bystander's counters"
    );
}

/// The entry is keyed by `(jar_id, supporter)`, so one supporter tipping two
/// jars keeps two independent histories — a badge for jar A must not be won
/// by tipping jar B.
#[test]
fn supporter_stats_are_independent_across_jars() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let supporter = Address::generate(env);
    token_admin.mint(&supporter, &10_000);

    let first = String::from_str(env, "@jara");
    let second = String::from_str(env, "@jarb");
    client.create_jar(
        &owner,
        &first,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 10000,
            },
        ],
    );
    client.create_jar(
        &owner,
        &second,
        &vec![
            env,
            Split {
                to: bob.clone(),
                bps: 10000,
            },
        ],
    );

    client.tip(&supporter, &first, &1_000, &String::from_str(env, "a"));
    client.tip(&supporter, &first, &1_000, &String::from_str(env, "b"));
    client.tip(&supporter, &second, &250, &String::from_str(env, "c"));

    assert_eq!(
        client.get_supporter_stats(&first, &supporter),
        SupporterStats {
            tip_count: 2,
            total: 2_000
        }
    );
    assert_eq!(
        client.get_supporter_stats(&second, &supporter),
        SupporterStats {
            tip_count: 1,
            total: 250
        }
    );
    // The jar aggregate still spans both supporters' jars.
    assert_eq!(
        client.get_jar_stats(&first),
        JarStats {
            tip_count: 2,
            total: 2_000
        }
    );
}

/// Two supporters on one jar each get their own counters, and the jar's total
/// is the sum of both.
#[test]
fn supporter_stats_are_independent_across_supporters() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let one = Address::generate(env);
    let two = Address::generate(env);
    token_admin.mint(&one, &10_000);
    token_admin.mint(&two, &10_000);

    let jar_id = String::from_str(env, "@shared");
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

    client.tip(&one, &jar_id, &1_000, &String::from_str(env, "x"));
    client.tip(&two, &jar_id, &7, &String::from_str(env, "y"));
    client.tip(&two, &jar_id, &7, &String::from_str(env, "z"));
    client.tip(&two, &jar_id, &7, &String::from_str(env, "w"));

    assert_eq!(
        client.get_supporter_stats(&jar_id, &one),
        SupporterStats {
            tip_count: 1,
            total: 1_000
        }
    );
    assert_eq!(
        client.get_supporter_stats(&jar_id, &two),
        SupporterStats {
            tip_count: 3,
            total: 21
        }
    );
    assert_eq!(
        client.get_jar_stats(&jar_id),
        JarStats {
            tip_count: 4,
            total: 1_021
        }
    );
}

#[test]
fn get_supporter_stats_on_missing_jar_fails() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    let supporter = Address::generate(env);
    let res = client.try_get_supporter_stats(&String::from_str(env, "@ghost"), &supporter);
    assert_eq!(res, Err(Ok(Error::JarNotFound.into())));
}

/// The extra writes #167 asks about are real rent, and the PR has to state
/// their cost. Two persistent entries per tip — the jar's and the supporter's
/// — on top of whatever the transfers themselves write.
#[test]
fn tip_writes_the_two_totals_entries() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@budget");
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

    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "costed"));
    let first = env.cost_estimate().resources();

    // Measured against the same tip with `bump_totals` disabled: 4 write
    // entries / 884 bytes / 2 rent bumps. `create_jar` already extends the
    // jar's TTL, so the tip's own jar extension is a no-op on a fresh jar and
    // does not count. The two counters add exactly two writes and two rent
    // bumps — see `docs/CONTRACT.md`.
    assert_eq!(
        first.write_entries, 6,
        "a tip writes the two totals entries, the transfer's own three, and the instance's sequence counter"
    );
    assert_eq!(
        first.write_bytes, 1376,
        "the totals cost more bytes per tip"
    );
    assert_eq!(
        first.persistent_entry_rent_bumps, 4,
        "both totals get the jar's TTL treatment, so each is bumped"
    );

    // A repeat tip to the same jar by the same supporter rewrites the same two
    // entries, so the marginal cost per tip is flat, not growing with use.
    client.tip(&tipper, &jar_id, &100, &String::from_str(env, "again"));
    let second = env.cost_estimate().resources();
    assert_eq!(
        second.write_entries, first.write_entries,
        "a second tip adds no ledger entries beyond the two totals writes"
    );
    assert_eq!(second.write_bytes, first.write_bytes);
}

/// The point of the view is that a client can call it before it has any jar,
/// any token, or any authorization — reads no storage, so nothing to set up.
#[test]
fn version_reports_the_shape_without_reading_storage() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let contract = env.register(TipSplitter, (admin, sac.address()));

    let client = TipSplitterClient::new(&env, &contract);
    assert_eq!(client.version(), INTERFACE_VERSION);

    // Same answer once there is state to read, so the value is not derived
    // from storage.
    let owner = Address::generate(&env);
    client.create_jar(
        &owner,
        &String::from_str(&env, "@vvv"),
        &vec![
            &env,
            Split {
                to: Address::generate(&env),
                bps: 10000,
            },
        ],
    );
    assert_eq!(client.version(), INTERFACE_VERSION);
}

/// A three-recipient jar used by the preview/tip agreement tests.
fn preview_tip_jar(
    env: &Env,
    client: &TipSplitterClient,
    jar_id: &String,
    a: &Address,
    b: &Address,
    c: &Address,
) {
    let owner = Address::generate(env);
    client.create_jar(
        &owner,
        jar_id,
        &vec![
            env,
            Split {
                to: a.clone(),
                bps: 5000,
            },
            Split {
                to: b.clone(),
                bps: 3000,
            },
            Split {
                to: c.clone(),
                bps: 2000,
            },
        ],
    );
}

/// The base case: the tipper is not a recipient, so `preview_tip` reports the
/// plain split and the event's breakdown moves exactly those amounts.
#[test]
fn preview_tip_matches_the_tip_event_breakdown_when_sender_is_not_a_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    let jar_id = String::from_str(env, "@ptip_outsider");
    preview_tip_jar(env, &client, &jar_id, &alice, &bob, &carol);

    let plan = client.preview_tip(&jar_id, &tipper, &101);
    // 5000/3000/2000 of 101: 50, 30, and the last recipient absorbs the dust.
    assert_eq!(
        i128_vec_to_std(&plan),
        std::vec::Vec::from([50_i128, 30_i128, 21_i128])
    );

    client.tip(&tipper, &jar_id, &101, &String::from_str(env, "thanks"));

    let moved: std::vec::Vec<i128> = tip_breakdown(env, &s.contract, &jar_id)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(
        moved,
        i128_vec_to_std(&plan),
        "preview_tip must agree with the tip event's breakdown, recipient by recipient"
    );

    // And the balances moved agree with both, so the agreement is physical,
    // not just two views of the same helper.
    let token = token::Client::new(env, &s.token);
    assert_eq!(token.balance(&alice), 50);
    assert_eq!(token.balance(&bob), 30);
    assert_eq!(token.balance(&carol), 21);
    assert_eq!(
        token.balance(&tipper),
        899,
        "the tipper paid the full 101: 50 + 30 + 21"
    );
}

/// A tipper who is also the *first* recipient keeps their own share: the
/// self-transfer is reported as 0, every other recipient gets exactly their
/// split, and `preview_tip` must report that, not the raw split (#78).
#[test]
fn preview_tip_matches_the_tip_event_breakdown_when_sender_is_a_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    token_admin.mint(&alice, &1_000);

    let jar_id = String::from_str(env, "@ptip_self");
    preview_tip_jar(env, &client, &jar_id, &alice, &bob, &carol);

    // alice tips 1000: her own 500 is never transferred and stays with her,
    // so the payout plan is [0, 300, 200] — not the [500, 300, 200] that
    // preview_split would report.
    let plan = client.preview_tip(&jar_id, &alice, &1_000);
    assert_eq!(
        i128_vec_to_std(&plan),
        std::vec::Vec::from([0_i128, 300_i128, 200_i128])
    );
    assert_eq!(
        i128_vec_to_std(&client.preview_split(&jar_id, &1_000)),
        std::vec::Vec::from([500_i128, 300_i128, 200_i128]),
        "preview_split stays sender-agnostic; preview_tip is the sender-aware view"
    );

    client.tip(
        &alice,
        &jar_id,
        &1_000,
        &String::from_str(env, "from one of you"),
    );

    let moved: std::vec::Vec<i128> = tip_breakdown(env, &s.contract, &jar_id)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(moved, i128_vec_to_std(&plan));

    let token = token::Client::new(env, &s.token);
    assert_eq!(
        token.balance(&alice),
        500,
        "alice's own 500 stays with her: only bob's 300 and carol's 200 leave her"
    );
    assert_eq!(token.balance(&bob), 300);
    assert_eq!(token.balance(&carol), 200);
}

/// The special case of the special case: a tipper who is the *last* recipient
/// absorbs the dust rule twice over — their final entry is 0 because nothing
/// moves, and no earlier share rolls onto them.
#[test]
fn preview_tip_reports_zero_for_a_sender_who_is_the_last_recipient() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let carol = Address::generate(env);
    token_admin.mint(&carol, &1_000);

    let jar_id = String::from_str(env, "@ptip_last");
    preview_tip_jar(env, &client, &jar_id, &alice, &bob, &carol);

    let plan = client.preview_tip(&jar_id, &carol, &1_000);
    assert_eq!(
        i128_vec_to_std(&plan),
        std::vec::Vec::from([500_i128, 300_i128, 0_i128])
    );

    client.tip(
        &carol,
        &jar_id,
        &1_000,
        &String::from_str(env, "last one skips"),
    );

    let moved: std::vec::Vec<i128> = tip_breakdown(env, &s.contract, &jar_id)
        .into_iter()
        .map(|(_, amount)| amount)
        .collect();
    assert_eq!(moved, i128_vec_to_std(&plan));
}

/// A client that calls `preview_tip` before signing must see the same
/// rejections `tip` would raise: unknown jar, below the minimum, and amounts
/// `tip` itself would reject.
#[test]
fn preview_tip_rejects_what_tip_rejects() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let token_admin = token::StellarAssetClient::new(env, &s.token);

    let alice = Address::generate(env);
    let tipper = Address::generate(env);
    token_admin.mint(&tipper, &1_000);

    assert_eq!(
        client.try_preview_tip(&String::from_str(env, "@ghost"), &tipper, &100_i128),
        Err(Ok(Error::JarNotFound.into()))
    );

    let jar_id = String::from_str(env, "@ptip_min");
    let owner = Address::generate(env);
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
    client.set_min_tip_amount(&jar_id, &Some(500_i128));

    assert_eq!(
        client.try_preview_tip(&jar_id, &tipper, &499_i128),
        Err(Ok(Error::BelowMinTipAmount.into()))
    );
    assert_eq!(
        client.try_preview_tip(&jar_id, &tipper, &0_i128),
        Err(Ok(Error::AmountZero.into()))
    );
    assert_eq!(
        client.try_preview_tip(&jar_id, &tipper, &-5_i128),
        Err(Ok(Error::AmountNegative.into()))
    );
    assert!(client.try_preview_tip(&jar_id, &tipper, &500_i128).is_ok());
}

// ── #140: Assert specific errors instead of is_err ──────────────────────────

#[test]
fn jar_exists_is_false_for_rejected_jar_exact_error() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let alice = Address::generate(env);
    let bob = Address::generate(env);
    let jar_id = String::from_str(env, "@rejected-exact");

    // 6000 + 3000 = 9000, so the jar is never stored.
    let res = client.try_create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: alice.clone(),
                bps: 6000,
            },
            Split {
                to: bob.clone(),
                bps: 3000,
            },
        ],
    );
    // #140 — assert the specific error, not just is_err()
    assert_eq!(res, Err(Ok(Error::SplitSumNot100Pct.into())));
}

/// `jar_exists` is the onboarding form's keystroke check, so it must answer
/// `false` for ids `create_jar` would refuse rather than panicking on them.
#[test]
fn jar_exists_rejects_slugs_create_jar_would_reject() {
    let s = setup();
    let client = TipSplitterClient::new(&s.env, &s.contract);
    assert!(!client.jar_exists(&String::from_str(&s.env, "")));
    assert!(!client.jar_exists(&String::from_str(&s.env, &"x".repeat(65))));
    assert!(!client.jar_exists(&String::from_str(&s.env, "@Alice")));
    assert!(!client.jar_exists(&String::from_str(&s.env, "@ab")));
}

#[test]
fn get_min_accepted_tip_accounts_for_rounding_and_configured_minimum() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);
    let owner = Address::generate(env);
    let jar_id = String::from_str(env, "@smallest");
    client.create_jar(
        &owner,
        &jar_id,
        &vec![
            env,
            Split {
                to: Address::generate(env),
                bps: 7500,
            },
            Split {
                to: Address::generate(env),
                bps: 2500,
            },
        ],
    );
    // ceil(10_000 / 2_500) is 4; this is the first amount where both
    // non-final shares are positive under integer division.
    assert_eq!(client.get_min_accepted_tip(&jar_id), 4);
    client.set_min_tip_amount(&jar_id, &Some(25));
    assert_eq!(client.get_min_accepted_tip(&jar_id), 25);
}

/// #321: the list starts with the default token, follows add and remove, and
/// is capped with a typed error.
#[test]
fn get_allowed_tokens_tracks_the_allowlist_and_is_capped() {
    let s = setup();
    let env = &s.env;
    let client = TipSplitterClient::new(env, &s.contract);

    assert_eq!(client.get_allowed_tokens(), vec![env, s.token.clone()]);
    let eurc = second_asset(env, &s.admin);
    client.allow_token(&eurc);
    client.allow_token(&eurc);
    assert_eq!(
        client.get_allowed_tokens(),
        vec![env, s.token.clone(), eurc.clone()]
    );
    client.disallow_token(&eurc);
    assert_eq!(client.get_allowed_tokens(), vec![env, s.token.clone()]);

    for _ in 1..MAX_ALLOWED_TOKENS {
        client.allow_token(&Address::generate(env));
    }
    assert_eq!(client.get_allowed_tokens().len(), MAX_ALLOWED_TOKENS);
    assert_eq!(
        client.try_allow_token(&Address::generate(env)),
        Err(Ok(Error::TooManyAllowedTokens.into()))
    );
}
