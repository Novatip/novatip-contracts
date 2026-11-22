//! Deterministic randomized regression tests (#255, #256, #257).
use super::*;
use soroban_sdk::vec;

fn fixed_cases(cases: u32) -> ProptestConfig {
    let mut config = ProptestConfig::with_cases(cases);
    if std::env::var_os("PROPTEST_RNG_SEED").is_none() {
        config.rng_seed = proptest::test_runner::RngSeed::Fixed(0x6e6f_7661_7469_7073);
    }
    config
}

fn bps_strategy() -> impl Strategy<Value = std::vec::Vec<u32>> {
    (1usize..=20usize).prop_flat_map(|n| {
        proptest::collection::vec(1u64..=100_000u64, n).prop_map(move |weights| {
            let remain = 10_000u64 - n as u64;
            let weight_sum: u64 = weights.iter().sum();
            let mut used = 0u64;
            let mut out = std::vec::Vec::with_capacity(n);
            for weight in weights.iter().take(n - 1) {
                let extra = weight * remain / weight_sum;
                used += extra;
                out.push(1 + extra as u32);
            }
            out.push(1 + (remain - used) as u32);
            out
        })
    })
}

fn generate_splits(
    env: &Env,
    bps: &[u32],
    supporter: &Address,
    sender_index: Option<usize>,
) -> (Vec<Split>, std::vec::Vec<Address>) {
    let recipients: std::vec::Vec<Address> = bps.iter().enumerate().map(|(idx, _)| {
        if sender_index == Some(idx) { supporter.clone() } else { Address::generate(env) }
    }).collect();
    let mut splits = vec![env];
    for (address, share) in recipients.iter().zip(bps) {
        splits.push_back(Split { to: address.clone(), bps: *share });
    }
    (splits, recipients)
}

// #256: preview_tip is a sender-aware *moved* amount, unlike preview_split.
proptest! {
    #![proptest_config(fixed_cases(32))]
    #[test]
    fn preview_tip_equals_event_moved_and_actual_balance_deltas(
        bps in bps_strategy(),
        amount in 100_000i128..=1_000_000i128,
        fee_bps in 0u32..=500u32,
        choice in any::<usize>(),
        include_sender in any::<bool>(),
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let token = token::Client::new(env, &s.token);
        let supporter = Address::generate(env);
        token::StellarAssetClient::new(env, &s.token).mint(&supporter, &(amount * 2));
        let sender_index = if include_sender && bps.len() > 1 { Some(choice % bps.len()) } else { None };
        let (splits, recipients) = generate_splits(env, &bps, &supporter, sender_index);
        let jar = String::from_str(env, "@preview_prop_sender");
        client.create_jar(&Address::generate(env), &jar, &splits);
        client.set_fee_bps(&fee_bps);
        let preview = client.preview_tip(&jar, &supporter, &amount);
        let before: std::vec::Vec<i128> = recipients.iter().map(|r| token.balance(r)).collect();
        let before_sender = token.balance(&supporter);
        let before_admin = token.balance(&s.admin);
        client.tip(&supporter, &jar, &amount, &String::from_str(env, "prop"));
        let event = last_tip_event(env, &s.contract, &jar);
        prop_assert_eq!(event.fee, amount * i128::from(fee_bps) / 10_000);
        prop_assert_eq!(event.token, s.token.clone());
        prop_assert_eq!(event.breakdown.len(), preview.len());
        for (idx, recipient) in recipients.iter().enumerate() {
            let (to, moved, held) = event.breakdown.get(idx as u32).unwrap();
            prop_assert_eq!(to, recipient.clone());
            prop_assert!(!held);
            prop_assert_eq!(moved, preview.get(idx as u32).unwrap());
            // The sender can also be a recipient. Their event moved amount
            // is zero, but their wallet is debited for all *other* recipients
            // and the fee; it does not simply stay at its pre-tip balance.
            if sender_index != Some(idx) {
                prop_assert_eq!(token.balance(recipient) - before[idx], moved);
            } else {
                prop_assert_eq!(moved, 0);
            }
        }
        let net_moved: i128 = (0..preview.len()).map(|idx| preview.get(idx).unwrap()).sum();
        prop_assert_eq!(before_sender - token.balance(&supporter), net_moved + event.fee);
        prop_assert_eq!(token.balance(&s.admin) - before_admin, event.fee);
        prop_assert_eq!(client.get_jar_stats(&jar).total, amount);
    }
}

// #257: get_min_accepted_tip must be exact after fee truncation and optional
// per-jar minimum, including n=1 and maximum recipient counts.
proptest! {
    #![proptest_config(fixed_cases(48))]
    #[test]
    fn min_accepted_tip_is_the_exact_fee_aware_boundary(
        bps in bps_strategy(),
        fee_bps in 0u32..=500u32,
        configured_minimum in 0i128..=25_000i128,
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let supporter = Address::generate(env);
        let (splits, _) = generate_splits(env, &bps, &supporter, None);
        let jar = String::from_str(env, "@minimum_prop");
        client.create_jar(&Address::generate(env), &jar, &splits);
        client.set_fee_bps(&fee_bps);
        if configured_minimum > 0 {
            client.set_min_tip_amount(&jar, &Some(configured_minimum));
        }
        let required_net = bps.iter()
            .map(|bps| (10_000i128 + i128::from(*bps) - 1) / i128::from(*bps))
            .max().unwrap();
        let fee_adjusted = (required_net - 1) * 10_000i128 / (10_000 - i128::from(fee_bps)) + 1;
        let expected = fee_adjusted.max(configured_minimum);
        let boundary = client.get_min_accepted_tip(&jar);
        prop_assert_eq!(boundary, expected);
        prop_assert!(client.try_preview_split(&jar, &boundary).is_ok());
        if boundary > 1 {
            prop_assert!(client.try_preview_split(&jar, &(boundary - 1)).is_err(),
                "gross amount immediately below the minimum was accepted");
        }
    }
}

// #255: identical split configurations must produce equivalent transfer plans
// under a schedule or a direct tip, including when the supporter is a recipient.
proptest! {
    #![proptest_config(fixed_cases(24))]
    #[test]
    fn recurring_collection_matches_manual_tip_for_random_splits_and_fees(
        bps in bps_strategy(),
        amount in 100_000i128..=500_000i128,
        fee_bps in 0u32..=500u32,
        choice in any::<usize>(),
        sender_is_recipient in any::<bool>(),
    ) {
        let s = setup();
        let env = &s.env;
        let client = TipSplitterClient::new(env, &s.contract);
        let token = token::Client::new(env, &s.token);
        let supporter = Address::generate(env);
        let sender_index = if sender_is_recipient && bps.len() > 1 { Some(choice % bps.len()) } else { None };
        let (splits, recipients) = generate_splits(env, &bps, &supporter, sender_index);
        let recurring_jar = String::from_str(env, "@recurring_property");
        let manual_jar = String::from_str(env, "@manual_property");
        let owner = Address::generate(env);
        client.create_jar(&owner, &recurring_jar, &splits);
        client.create_jar(&owner, &manual_jar, &splits);
        client.set_fee_bps(&fee_bps);
        token::StellarAssetClient::new(env, &s.token).mint(&supporter, &(amount * 4));
        let expiration = env.ledger().sequence() + 10_000;
        token.approve(&supporter, &s.contract, &amount, &expiration);
        let schedule = client.create_recurring(
            &supporter, &recurring_jar, &amount, &100, &String::from_str(env, "same"));
        let due = client.get_recurring(&schedule).next_due_ledger;
        env.ledger().set_sequence_number(due);
        let before_recur: std::vec::Vec<i128> = recipients.iter().map(|r| token.balance(r)).collect();
        let before_recur_admin = token.balance(&s.admin);
        let before_recur_sender = token.balance(&supporter);
        env.mock_auths(&[]);
        client.collect_recurring(&schedule);
        let recurring_event = last_tip_event(env, &s.contract, &recurring_jar);
        env.mock_all_auths();
        let recurring_deltas: std::vec::Vec<i128> = recipients.iter().enumerate()
            .map(|(i, r)| token.balance(r) - before_recur[i]).collect();
        let recurring_admin_delta = token.balance(&s.admin) - before_recur_admin;
        let recurring_sender_debit = before_recur_sender - token.balance(&supporter);
        prop_assert_eq!(token.allowance(&supporter, &s.contract), 0);
        let before_manual: std::vec::Vec<i128> = recipients.iter().map(|r| token.balance(r)).collect();
        let before_manual_admin = token.balance(&s.admin);
        let before_manual_sender = token.balance(&supporter);
        client.tip(&supporter, &manual_jar, &amount, &String::from_str(env, "same"));
        let manual_event = last_tip_event(env, &s.contract, &manual_jar);
        let manual_deltas: std::vec::Vec<i128> = recipients.iter().enumerate()
            .map(|(i, r)| token.balance(r) - before_manual[i]).collect();
        prop_assert_eq!(recurring_event.amount, manual_event.amount);
        prop_assert_eq!(recurring_event.fee, manual_event.fee);
        prop_assert_eq!(recurring_event.breakdown, manual_event.breakdown);
        prop_assert_eq!(recurring_event.token, manual_event.token);
        prop_assert_eq!(recurring_deltas, manual_deltas);
        prop_assert_eq!(recurring_admin_delta, token.balance(&s.admin) - before_manual_admin);
        prop_assert_eq!(recurring_sender_debit, before_manual_sender - token.balance(&supporter));
        prop_assert_eq!(client.get_jar_stats(&recurring_jar), client.get_jar_stats(&manual_jar));
        prop_assert_eq!(client.get_jar_stats(&recurring_jar).total, amount);
    }
}
