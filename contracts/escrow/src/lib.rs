#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, Env, Symbol,
};

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

#[contracttype]
pub enum DataKey {
    /// Persistent trade record keyed by trade ID.
    Trade(u64),
    /// Instance storage counter for the last allocated trade ID.
    TradeCount,
    /// Instance storage admin address authorized for privileged actions.
    Admin,
    /// Instance storage token contract address used for escrow payments.
    Token,
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum TradeStatus {
    /// Listed and waiting for a buyer.
    Open,
    /// Buyer has deposited funds into escrow.
    Locked,
    /// Escrowed funds were released to the seller.
    Completed,
    /// Trade was flagged for admin intervention.
    Disputed,
    /// Trade was cancelled and funds were returned when applicable.
    Cancelled,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct TradeOffer {
    /// Unique trade ID allocated from DataKey::TradeCount.
    pub id: u64,
    /// Seller address that created the trade and receives released funds.
    pub seller: Address,
    /// Buyer address once funds are locked, or None while the trade is open.
    pub buyer: Option<Address>,
    /// Stablecoin amount to escrow, expressed in token base units such as stroops.
    pub amount: i128,
    /// Off-chain asset category being purchased, for example AIRTIME or DATA.
    pub asset_type: Symbol,
    /// Current lifecycle state for the trade.
    pub status: TradeStatus,
    /// Expiration time as a Unix timestamp in ledger seconds.
    pub expires_at: u64,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

fn topic_created() -> Symbol {
    symbol_short!("created")
}

fn topic_locked() -> Symbol {
    symbol_short!("locked")
}

fn topic_completed() -> Symbol {
    symbol_short!("completed")
}

fn topic_cancelled() -> Symbol {
    symbol_short!("cancelled")
}

fn topic_disputed() -> Symbol {
    symbol_short!("disputed")
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn get_admin_address(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("not initialised")
}

fn get_token_address(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Token)
        .expect("not initialised")
}

fn get_trade_or_panic(env: &Env, trade_id: u64) -> TradeOffer {
    env.storage()
        .persistent()
        .get(&DataKey::Trade(trade_id))
        .expect("trade not found")
}

fn set_trade(env: &Env, trade_id: u64, trade: &TradeOffer) {
    let key = DataKey::Trade(trade_id);
    env.storage().persistent().set(&key, trade);
    env.storage()
        .persistent()
        .extend_ttl(&key, 17_280, 17_280 * 30);
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    pub fn initialize(env: Env, admin: Address, token: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialised");
        }

        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::TradeCount, &0u64);
        env.storage().instance().extend_ttl(17_280, 17_280 * 30);
    }

    pub fn create_listing(
        env: Env,
        seller: Address,
        amount: i128,
        asset_type: Symbol,
        expires_at: u64,
    ) -> u64 {
        seller.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }

        if expires_at <= env.ledger().timestamp() {
            panic!("expires_at must be in the future");
        }

        let id = Self::trade_count(env.clone()) + 1;
        env.storage().instance().set(&DataKey::TradeCount, &id);

        let trade = TradeOffer {
            id,
            seller: seller.clone(),
            buyer: None,
            amount,
            asset_type: asset_type.clone(),
            status: TradeStatus::Open,
            expires_at,
        };

        set_trade(&env, id, &trade);
        env.events()
            .publish((topic_created(), asset_type), (id, seller, amount));

        id
    }

    pub fn deposit_to_escrow(env: Env, buyer: Address, trade_id: u64) {
        buyer.require_auth();

        let mut trade = get_trade_or_panic(&env, trade_id);

        if trade.status != TradeStatus::Open {
            panic!("trade is not open");
        }

        if env.ledger().timestamp() >= trade.expires_at {
            panic!("trade has expired");
        }

        if buyer == trade.seller {
            panic!("seller cannot buy own trade");
        }

        let token_address = get_token_address(&env);
        let token_client = token::Client::new(&env, &token_address);
        token_client.transfer(&buyer, &env.current_contract_address(), &trade.amount);

        trade.buyer = Some(buyer.clone());
        trade.status = TradeStatus::Locked;
        set_trade(&env, trade_id, &trade);

        env.events().publish((topic_locked(),), (trade_id, buyer));
    }

    pub fn release_payment(env: Env, trade_id: u64) {
        let admin = get_admin_address(&env);
        admin.require_auth();

        let mut trade = get_trade_or_panic(&env, trade_id);

        if trade.status != TradeStatus::Locked {
            panic!("trade is not locked");
        }

        let token_address = get_token_address(&env);
        let token_client = token::Client::new(&env, &token_address);
        token_client.transfer(
            &env.current_contract_address(),
            &trade.seller,
            &trade.amount,
        );

        trade.status = TradeStatus::Completed;
        set_trade(&env, trade_id, &trade);

        env.events()
            .publish((topic_completed(),), (trade_id, trade.seller));
    }

    pub fn cancel_and_refund(env: Env, caller: Address, trade_id: u64) {
        caller.require_auth();

        let admin = get_admin_address(&env);
        let mut trade = get_trade_or_panic(&env, trade_id);
        let is_admin = caller == admin;
        let is_buyer = trade.buyer.as_ref().is_some_and(|buyer| buyer == &caller);

        if !is_admin && !is_buyer {
            panic!("only admin or buyer can cancel");
        }

        if !is_admin && env.ledger().timestamp() < trade.expires_at {
            panic!("timelock has not expired yet");
        }

        if trade.status == TradeStatus::Locked {
            let buyer = trade.buyer.clone().expect("buyer not found");
            let token_address = get_token_address(&env);
            let token_client = token::Client::new(&env, &token_address);
            token_client.transfer(&env.current_contract_address(), &buyer, &trade.amount);
        } else if trade.status != TradeStatus::Open && trade.status != TradeStatus::Disputed {
            panic!("trade cannot be cancelled in its current state");
        }

        trade.status = TradeStatus::Cancelled;
        set_trade(&env, trade_id, &trade);

        env.events()
            .publish((topic_cancelled(),), (trade_id, caller));
    }

    pub fn flag_dispute(env: Env, caller: Address, trade_id: u64) {
        caller.require_auth();

        let mut trade = get_trade_or_panic(&env, trade_id);
        let is_buyer = trade.buyer.as_ref().is_some_and(|buyer| buyer == &caller);

        if caller != trade.seller && !is_buyer {
            panic!("only trade parties can flag a dispute");
        }

        if trade.status != TradeStatus::Locked {
            panic!("only a locked trade can be disputed");
        }

        trade.status = TradeStatus::Disputed;
        set_trade(&env, trade_id, &trade);

        env.events()
            .publish((topic_disputed(),), (trade_id, caller));
    }

    pub fn get_trade(env: Env, trade_id: u64) -> TradeOffer {
        get_trade_or_panic(&env, trade_id)
    }

    pub fn trade_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::TradeCount)
            .unwrap_or(0u64)
    }

    pub fn get_admin(env: Env) -> Address {
        get_admin_address(&env)
    }

    pub fn get_token(env: Env) -> Address {
        get_token_address(&env)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token::{Client as TokenClient, StellarAssetClient},
        Env,
    };

    fn setup() -> (
        Env,
        EscrowContractClient<'static>,
        Address,
        Address,
        Address,
        Address,
    ) {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(EscrowContract, ());
        let client = EscrowContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let seller = Address::generate(&env);
        let buyer = Address::generate(&env);

        let token_admin = Address::generate(&env);
        let token_id = env.register_stellar_asset_contract_v2(token_admin);
        let token_address = token_id.address();
        let sac = StellarAssetClient::new(&env, &token_address);
        sac.mint(&buyer, &10_000_0000000i128);

        client.initialize(&admin, &token_address);

        (env, client, admin, seller, buyer, token_address)
    }

    #[test]
    fn test_create_listing() {
        let (env, client, _admin, seller, _buyer, _token) = setup();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);

        let trade_id = client.create_listing(
            &seller,
            &500_0000000i128,
            &symbol_short!("AIRTIME"),
            &(1_000_000 + 86_400),
        );

        assert_eq!(trade_id, 1);

        let trade = client.get_trade(&trade_id);
        assert_eq!(trade.id, trade_id);
        assert_eq!(trade.seller, seller);
        assert_eq!(trade.buyer, None);
        assert_eq!(trade.amount, 500_0000000i128);
        assert_eq!(trade.asset_type, symbol_short!("AIRTIME"));
        assert_eq!(trade.status, TradeStatus::Open);
    }

    #[test]
    fn test_deposit_to_escrow() {
        let (env, client, _admin, seller, buyer, token) = setup();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);

        let trade_id = client.create_listing(
            &seller,
            &500_0000000i128,
            &symbol_short!("AIRTIME"),
            &(1_000_000 + 86_400),
        );

        client.deposit_to_escrow(&buyer, &trade_id);

        let trade = client.get_trade(&trade_id);
        assert_eq!(trade.status, TradeStatus::Locked);
        assert_eq!(trade.buyer, Some(buyer));

        let token_client = TokenClient::new(&env, &token);
        assert_eq!(
            token_client.balance(&env.current_contract_address()),
            500_0000000i128
        );
    }

    #[test]
    fn test_release_payment() {
        let (env, client, _admin, seller, buyer, token) = setup();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);

        let trade_id = client.create_listing(
            &seller,
            &500_0000000i128,
            &symbol_short!("DATA"),
            &(1_000_000 + 86_400),
        );
        client.deposit_to_escrow(&buyer, &trade_id);
        client.release_payment(&trade_id);

        let trade = client.get_trade(&trade_id);
        assert_eq!(trade.status, TradeStatus::Completed);

        let token_client = TokenClient::new(&env, &token);
        assert_eq!(token_client.balance(&seller), 500_0000000i128);
    }

    #[test]
    fn test_cancel_and_refund_after_expiry() {
        let (env, client, _admin, seller, buyer, token) = setup();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);

        let trade_id = client.create_listing(
            &seller,
            &500_0000000i128,
            &symbol_short!("AIRTIME"),
            &(1_000_000 + 86_400),
        );
        client.deposit_to_escrow(&buyer, &trade_id);

        env.ledger().with_mut(|l| l.timestamp = 1_000_000 + 86_401);
        client.cancel_and_refund(&buyer, &trade_id);

        let trade = client.get_trade(&trade_id);
        assert_eq!(trade.status, TradeStatus::Cancelled);

        let token_client = TokenClient::new(&env, &token);
        assert_eq!(token_client.balance(&buyer), 10_000_0000000i128);
    }

    #[test]
    #[should_panic(expected = "timelock has not expired yet")]
    fn test_cancel_before_expiry_fails() {
        let (env, client, _admin, seller, buyer, _token) = setup();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);

        let trade_id = client.create_listing(
            &seller,
            &500_0000000i128,
            &symbol_short!("AIRTIME"),
            &(1_000_000 + 86_400),
        );
        client.deposit_to_escrow(&buyer, &trade_id);

        client.cancel_and_refund(&buyer, &trade_id);
    }
}
