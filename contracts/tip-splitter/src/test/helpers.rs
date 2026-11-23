//! Shared test fixtures and helpers (#144).
//!
//! `TestSetup` replaces the repeated env/token/contract boilerplate;
//! `solo_jar` / `jar_with` build the jars the tipping tests share, and
//! `events_for` filters an invocation's events by topic so a test can decode
//! the payload it expects.

#![cfg(test)]

use super::*;

/// A fresh env, a deployed TipSplitter and its default token.
///
/// All auths are mocked, so a test only has to grant the specific signatures
/// it wants to exercise by calling `env.mock_auths` itself.
pub struct TestSetup {
    pub env: Env,
    pub admin: Address,
    pub contract: Address,
    pub token: Address,
}

impl TestSetup {
    pub fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let contract = env.register(TipSplitter, (admin.clone(), token.clone()));
        Self {
            env,
            admin,
            contract,
            token,
        }
    }

    pub fn client(&self) -> TipSplitterClient {
        TipSplitterClient::new(&self.env, &self.contract)
    }

    pub fn token_client(&self) -> token::Client {
        token::Client::new(&self.env, &self.token)
    }

    pub fn token_admin_client(&self) -> token::StellarAssetClient {
        token::StellarAssetClient::new(&self.env, &self.token)
    }
}

/// A jar paying 100% to one recipient.
pub fn solo_jar(
    env: &Env,
    client: &TipSplitterClient,
    jar_id: &str,
    owner: &Address,
    recipient: &Address,
) {
    jar_with(
        env,
        client,
        jar_id,
        owner,
        &[(recipient.clone(), BPS_DENOM)],
    );
}

/// A jar with the given `(recipient, bps)` shares.
pub fn jar_with(
    env: &Env,
    client: &TipSplitterClient,
    jar_id: &str,
    owner: &Address,
    recipients: &[(Address, u32)],
) {
    let mut splits: Vec<Split> = Vec::new(env);
    for (to, bps) in recipients {
        splits.push_back(Split {
            to: to.clone(),
            bps: *bps,
        });
    }
    client.create_jar(owner, &String::from_str(env, jar_id), &splits);
}

/// Events published by `contract` whose first topic is `topic`, in publication
/// order. Callers decode the data into the struct the event publishes.
pub fn events_for(env: &Env, contract: &Address, topic: Symbol) -> std::vec::Vec<(Vec<Val>, Val)> {
    env.events()
        .all()
        .iter()
        .filter(|(addr, topics, _)| {
            *addr == *contract && topic_is_symbol(env, topics, 0, topic.clone())
        })
        .map(|(_, topics, data)| (topics, data))
        .collect()
}
