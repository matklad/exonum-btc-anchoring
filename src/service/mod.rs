pub mod schema;
pub mod error;

mod handler;
mod anchoring;
mod transfering;

use std::sync::{Arc, Mutex};
use std::collections::HashMap;

use serde_json::Value;
use serde_json::value::ToJson;
use bitcoin::util::base58::ToBase58;

use exonum::blockchain::{Service, Transaction, NodeState};
use exonum::crypto::Hash;
use exonum::messages::{RawTransaction, Message, FromRaw, Error as MessageError};
use exonum::storage::{View, Error as StorageError};

use config::{AnchoringNodeConfig, AnchoringConfig};
use {AnchoringRpc, BitcoinSignature};
use transactions::{TxKind, AnchoringTx};

use self::schema::{ANCHORING_SERVICE, AnchoringTransaction, AnchoringSchema, TxAnchoringSignature};
use self::error::Error as ServiceError;
use self::handler::{LectKind, MultisigAddress};

pub use self::handler::AnchoringHandler;

pub struct AnchoringService {
    genesis: AnchoringConfig,
    handler: Arc<Mutex<AnchoringHandler>>,
}

// TODO error chain

impl AnchoringService {
    pub fn new(client: AnchoringRpc,
               genesis: AnchoringConfig,
               cfg: AnchoringNodeConfig)
               -> AnchoringService {
        AnchoringService {
            genesis: genesis,
            handler: Arc::new(Mutex::new(AnchoringHandler::new(client, cfg))),
        }
    }

    pub fn handler(&self) -> Arc<Mutex<AnchoringHandler>> {
        self.handler.clone()
    }
}

impl Transaction for AnchoringTransaction {
    fn verify(&self) -> bool {
        self.verify_signature(self.from())
    }

    fn execute(&self, view: &View) -> Result<(), StorageError> {
        match *self {
            AnchoringTransaction::Signature(ref msg) => msg.execute(view),
            AnchoringTransaction::UpdateLatest(ref msg) => msg.execute(view),
        }
    }
}

impl Service for AnchoringService {
    fn service_id(&self) -> u16 {
        ANCHORING_SERVICE
    }

    fn state_hash(&self, _: &View) -> Result<Vec<Hash>, StorageError> {
        Ok(Vec::new())
    }

    fn tx_from_raw(&self, raw: RawTransaction) -> Result<Box<Transaction>, MessageError> {
        AnchoringTransaction::from_raw(raw).map(|tx| Box::new(tx) as Box<Transaction>)
    }

    fn handle_genesis_block(&self, view: &View) -> Result<Value, StorageError> {
        let handler = self.handler.lock().unwrap();
        let cfg = self.genesis.clone();
        let (_, addr) = cfg.redeem_script();
        handler.client
            .importaddress(&addr.to_base58check(), "multisig", false, false)
            .unwrap();

        AnchoringSchema::new(view).create_genesis_config(&cfg)?;
        Ok(cfg.to_json())
    }

    fn handle_commit(&self, state: &mut NodeState) -> Result<(), StorageError> {
        debug!("Handle commit, height={}", state.height());
        match self.handler.lock().unwrap().handle_commit(state) {
            Err(ServiceError::Rpc(e)) => {
                error!("An error occured: {}", e);
                Ok(())
            }
            Err(ServiceError::Storage(e)) => Err(e),
            Ok(()) => Ok(()),
        }
    }
}

pub fn collect_signatures<'a, I>(proposal: &AnchoringTx,
                                 genesis: &AnchoringConfig,
                                 msgs: I)
                                 -> Option<HashMap<u32, Vec<BitcoinSignature>>>
    where I: Iterator<Item = &'a TxAnchoringSignature>
{
    let mut signatures = HashMap::new();
    for input in proposal.inputs() {
        signatures.insert(input, vec![None; genesis.validators.len()]);
    }

    for msg in msgs {
        let input = msg.input();
        let validator = msg.validator() as usize;

        let mut signatures_by_input = signatures.get_mut(&input).unwrap();
        signatures_by_input[validator] = Some(msg.signature().to_vec());
    }

    let majority_count = genesis.majority_count() as usize;

    // remove holes from signatures preserve order
    let mut actual_signatures = HashMap::new();
    for (input, signatures) in signatures.into_iter() {
        let signatures = signatures.into_iter()
            .filter_map(|x| x)
            .take(majority_count)
            .collect::<Vec<_>>();

        if signatures.len() < majority_count {
            return None;
        }
        actual_signatures.insert(input, signatures);
    }
    Some(actual_signatures)
}