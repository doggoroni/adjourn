//! `FakeNode` — runs the real contract and delegate code in memory.
//!
//! CI has no Freenet node and never will. A fake that replayed canned
//! responses would test wiring and nothing else. This one calls
//! `adjourn_contract::Contract`'s real methods and `adjourn_delegate::handle`'s
//! real dispatch, so a genuine merge or projection mistake fails a test here
//! instead of surviving to a live node.
//!
//! Two fakes can share a [`World`] (the contract *state*, which is the thing
//! that must converge) while each keeps its own [`MemoryStore`] for delegate
//! secrets — exactly how two real nodes behave: shared public contract state,
//! private per-node signing keys.

use adjourn_contract::Contract;
use adjourn_core::delegate_api::{Request, Response};
use adjourn_delegate::secrets::MemoryStore;
use freenet_stdlib::prelude::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::node::NodeClient;

/// Contract state only, keyed by the contract instance id's raw bytes.
///
/// `ContractInstanceId` does not implement `Ord` (freenet-stdlib 0.8.5), so a
/// `BTreeMap<ContractInstanceId, _>` does not compile; keying on the id's
/// `[u8; 32]` (via its `Deref` target) keeps the same canonical,
/// deterministic ordering `BTreeMap` is chosen for elsewhere in this
/// workspace, without needing a newtype wrapper.
///
/// Shared across fakes so they converge on the same public state, exactly
/// like two real nodes replicating the same contract.
pub type World = Arc<Mutex<BTreeMap<[u8; 32], Vec<u8>>>>;

pub fn shared_world() -> World {
    Arc::new(Mutex::new(BTreeMap::new()))
}

/// Runs the real contract and delegate code against in-memory state, so
/// `NodeClient` callers can be exercised without a live Freenet node.
pub struct FakeNode {
    world: World,
    /// Params are needed by every `Contract` method but are not part of the
    /// contract *state* — each node keeps the params for the games it knows
    /// about separately, populated on `put`.
    params: BTreeMap<[u8; 32], Parameters<'static>>,
    store: MemoryStore,
}

impl FakeNode {
    pub fn new(world: World) -> Self {
        Self {
            world,
            params: BTreeMap::new(),
            store: MemoryStore::default(),
        }
    }

    /// Test helper: seed the shared world with raw bytes for an id, bypassing
    /// the contract entirely. Used to assert that two fakes see the same
    /// state once one of them writes it.
    pub fn put_raw(&mut self, id: ContractInstanceId, state: Vec<u8>) {
        self.world
            .lock()
            .expect("world lock poisoned")
            .insert(*id, state);
    }
}

impl NodeClient for FakeNode {
    async fn get(
        &mut self,
        id: ContractInstanceId,
        _subscribe: bool,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self
            .world
            .lock()
            .expect("world lock poisoned")
            .get(&*id)
            .cloned())
    }

    async fn put(&mut self, container: ContractContainer, state: Vec<u8>) -> anyhow::Result<()> {
        let id = *container.id();
        self.params.insert(*id, container.params());
        self.world
            .lock()
            .expect("world lock poisoned")
            .insert(*id, state);
        Ok(())
    }

    async fn update(&mut self, key: ContractKey, delta: Vec<u8>) -> anyhow::Result<()> {
        let id = *key.id();
        let params = self
            .params
            .get(&*id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("fake node has no params for contract {id}"))?;
        let current = self
            .world
            .lock()
            .expect("world lock poisoned")
            .get(&*id)
            .cloned()
            .unwrap_or_default();

        let modification = Contract::update_state(
            params,
            State::from(current),
            vec![UpdateData::Delta(StateDelta::from(delta))],
        )
        .map_err(|e| anyhow::anyhow!("update_state: {e:?}"))?;
        let new_state = modification.unwrap_valid();

        self.world
            .lock()
            .expect("world lock poisoned")
            .insert(*id, new_state.as_ref().to_vec());
        Ok(())
    }

    async fn delegate(&mut self, req: Request) -> anyhow::Result<Response> {
        let world = self.world.clone();
        Ok(adjourn_delegate::handle(
            &mut self.store,
            |id| world.lock().ok()?.get(id).cloned(),
            None,
            req,
        ))
    }
}
