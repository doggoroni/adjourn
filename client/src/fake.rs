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
use adjourn_delegate::secrets::{MemoryStore, SecretStore};
use freenet_stdlib::prelude::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::node::NodeClient;

/// Contract params and state, keyed by the contract instance id's raw bytes.
///
/// `ContractInstanceId` does not implement `Ord` (freenet-stdlib 0.8.5), so a
/// `BTreeMap<ContractInstanceId, _>` does not compile; keying on the id's
/// `[u8; 32]` (via its `Deref` target) keeps the same canonical,
/// deterministic ordering `BTreeMap` is chosen for elsewhere in this
/// workspace, without needing a newtype wrapper.
///
/// Parameters live HERE rather than per-node: a node that learns a contract
/// learns its parameters with it, and a node that never PUT the contract
/// (because its opponent did first) must still be able to run the contract
/// against it.
///
/// Shared across fakes so they converge on the same public state, exactly
/// like two real nodes replicating the same contract.
///
/// The shared contract world: current state per contract, plus an ordered log
/// of every write so a second fake can observe the first's writes the way a
/// subscribed peer would.
#[derive(Default)]
pub struct WorldInner {
    pub contracts: BTreeMap<[u8; 32], (Parameters<'static>, Vec<u8>)>,
    pub log: Vec<([u8; 32], Vec<u8>)>,
}

pub type World = Arc<Mutex<WorldInner>>;

pub fn shared_world() -> World {
    Arc::new(Mutex::new(WorldInner::default()))
}

/// A `MemoryStore` for delegate secrets, paired with a handle to the shared
/// contract `World` so `SecretStore::contract_state` can answer for real.
///
/// This is what makes the delegate's best-effort illegality check
/// (`locally_known_to_be_illegal` in `adjourn_delegate`) exercisable through
/// `FakeNode`: a real node's `DelegateCtx` can read local contract state, and
/// a fake that always answered `None` here would silently stop covering that
/// path in every CLI test.
#[derive(Clone)]
struct WorldBackedStore {
    secrets: MemoryStore,
    world: World,
}

impl SecretStore for WorldBackedStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.secrets.get(key)
    }
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool {
        self.secrets.set(key, value)
    }
    fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        self.secrets.list(prefix)
    }
    fn contract_state(&self, id: &[u8; 32]) -> Option<Vec<u8>> {
        self.world
            .lock()
            .ok()?
            .contracts
            .get(id)
            .map(|(_, state)| state.clone())
    }
}

/// Runs the real contract and delegate code against in-memory state, so
/// `NodeClient` callers can be exercised without a live Freenet node.
pub struct FakeNode {
    world: World,
    store: WorldBackedStore,
    cursor: usize,
}

impl FakeNode {
    pub fn new(world: World) -> Self {
        Self {
            store: WorldBackedStore {
                secrets: MemoryStore::default(),
                world: world.clone(),
            },
            world,
            cursor: 0,
        }
    }

    /// Test helper: seed the shared world with raw bytes for an id, bypassing
    /// the contract entirely. Used to assert that two fakes see the same
    /// state once one of them writes it. Never runs the contract, so an
    /// empty placeholder `Parameters` is fine.
    pub fn put_raw(&mut self, id: ContractInstanceId, state: Vec<u8>) {
        self.world
            .lock()
            .expect("world lock poisoned")
            .contracts
            .insert(*id, (Parameters::from(Vec::<u8>::new()), state));
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
            .contracts
            .get(&*id)
            .map(|(_, state)| state.clone()))
    }

    async fn put(&mut self, container: ContractContainer, state: Vec<u8>) -> anyhow::Result<()> {
        let id = *container.id();
        let mut world = self.world.lock().expect("world lock poisoned");
        world
            .contracts
            .insert(*id, (container.params(), state.clone()));
        world.log.push((*id, state));
        Ok(())
    }

    async fn update(&mut self, key: ContractKey, delta: Vec<u8>) -> anyhow::Result<()> {
        let id = *key.id();
        let (params, current) = self
            .world
            .lock()
            .expect("world lock poisoned")
            .contracts
            .get(&*id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("fake node has no state for contract {id}"))?;

        let modification = Contract::update_state(
            params.clone(),
            State::from(current),
            vec![UpdateData::Delta(StateDelta::from(delta))],
        )
        .map_err(|e| anyhow::anyhow!("update_state: {e:?}"))?;
        let new_state = modification.unwrap_valid();

        let mut world = self.world.lock().expect("world lock poisoned");
        world
            .contracts
            .insert(*id, (params, new_state.as_ref().to_vec()));
        world.log.push((*id, new_state.as_ref().to_vec()));
        Ok(())
    }

    async fn delegate(&mut self, req: Request) -> anyhow::Result<Response> {
        Ok(adjourn_delegate::handle(&mut self.store, None, req))
    }

    async fn next_update(
        &mut self,
    ) -> anyhow::Result<Option<(ContractInstanceId, UpdateData<'static>)>> {
        let entry = {
            let world = self.world.lock().expect("world lock");
            world.log.get(self.cursor).cloned()
        };
        let Some((id, bytes)) = entry else {
            return Ok(None);
        };
        self.cursor += 1;
        Ok(Some((
            ContractInstanceId::new(id),
            UpdateData::State(State::from(bytes)),
        )))
    }
}
