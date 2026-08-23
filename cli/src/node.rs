//! The node seam.
//!
//! `WsClient` is the real thing. `FakeNode` (see `fake.rs`) is the other impl,
//! and it runs the real contract and delegate code so CI can exercise the
//! session flows without a Freenet node.

use adjourn_core::delegate_api::{Request, Response};
use adjourn_core::GameParams;
use anyhow::{anyhow, bail, Context};
use freenet_stdlib::client_api::{
    ClientRequest, ContractRequest, ContractResponse, DelegateRequest, HostResponse, WebApi,
};
use freenet_stdlib::prelude::*;
use std::sync::Arc;

// `NodeClient` is only ever used generically (`<N: NodeClient>`), never as
// `dyn NodeClient`, so the missing auto-trait bounds this lint warns about
// (e.g. `Send` on the returned future) are not a real hazard here.
#[allow(async_fn_in_trait)]
pub trait NodeClient {
    /// `Ok(None)` means the node does not have this contract.
    async fn get(
        &mut self,
        id: ContractInstanceId,
        subscribe: bool,
    ) -> anyhow::Result<Option<Vec<u8>>>;
    async fn put(&mut self, container: ContractContainer, state: Vec<u8>) -> anyhow::Result<()>;
    async fn update(&mut self, key: ContractKey, delta: Vec<u8>) -> anyhow::Result<()>;
    async fn delegate(&mut self, req: Request) -> anyhow::Result<Response>;
}

/// Build the contract container and its instance id from raw cargo WASM.
///
/// `fdev publish` wants a pre-packaged file, but the programmatic path takes
/// the raw module and applies the version wrapper itself — this is what River
/// does, and it is why `scripts/build-contract.sh` output is the right
/// artifact.
pub fn contract_container(
    wasm: Vec<u8>,
    params: &GameParams,
) -> anyhow::Result<(ContractContainer, ContractInstanceId)> {
    let mut param_bytes = Vec::new();
    ciborium::into_writer(params, &mut param_bytes).context("encode params")?;
    let parameters = Parameters::from(param_bytes);
    let code = ContractCode::from(wasm);
    let id = ContractInstanceId::from_params_and_code(&parameters, &code);
    let container = ContractContainer::from(ContractWasmAPIVersion::V1(WrappedContract::new(
        Arc::new(code),
        parameters,
    )));
    Ok((container, id))
}

/// The delegate key is a pure function of its code, so it is derived rather
/// than stored — nothing to keep in sync, and no stale cached key pointing at
/// a generation that is gone.
pub fn delegate_container(wasm: Vec<u8>) -> (DelegateContainer, DelegateKey) {
    let code = DelegateCode::from(wasm);
    let params = Parameters::from(Vec::<u8>::new());
    let delegate = Delegate::from((&code, &params));
    let key = delegate.key().clone();
    (
        DelegateContainer::Wasm(DelegateWasmAPIVersion::V1(delegate)),
        key,
    )
}

pub struct WsClient {
    api: WebApi,
    delegate_key: DelegateKey,
}

impl WsClient {
    pub async fn connect(url: &str, delegate_key: DelegateKey) -> anyhow::Result<Self> {
        let (stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        Ok(Self {
            api: WebApi::start(stream),
            delegate_key,
        })
    }

    pub async fn register_delegate(&mut self, container: DelegateContainer) -> anyhow::Result<()> {
        self.api
            .send(ClientRequest::DelegateOp(
                DelegateRequest::RegisterDelegate {
                    delegate: container,
                    cipher: [0u8; 32],
                    nonce: [0u8; 24],
                },
            ))
            .await?;
        // Default cipher and nonce are accepted in local mode only.
        match self.api.recv().await? {
            HostResponse::Ok | HostResponse::DelegateResponse { .. } => Ok(()),
            other => bail!("unexpected response to RegisterDelegate: {other:?}"),
        }
    }
}

impl NodeClient for WsClient {
    async fn get(
        &mut self,
        id: ContractInstanceId,
        subscribe: bool,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        self.api
            .send(ClientRequest::ContractOp(ContractRequest::Get {
                key: id,
                return_contract_code: false,
                subscribe,
                blocking_subscribe: false,
            }))
            .await?;
        loop {
            match self.api.recv().await? {
                HostResponse::ContractResponse(ContractResponse::GetResponse { state, .. }) => {
                    return Ok(Some(state.as_ref().to_vec()))
                }
                HostResponse::ContractResponse(ContractResponse::NotFound { .. }) => {
                    return Ok(None)
                }
                // A subscribe ack or a stray notification can arrive first.
                HostResponse::ContractResponse(ContractResponse::SubscribeResponse { .. })
                | HostResponse::ContractResponse(ContractResponse::UpdateNotification { .. }) => {}
                other => bail!("unexpected response to Get: {other:?}"),
            }
        }
    }

    async fn put(&mut self, container: ContractContainer, state: Vec<u8>) -> anyhow::Result<()> {
        self.api
            .send(ClientRequest::ContractOp(ContractRequest::Put {
                contract: container,
                state: WrappedState::new(state),
                related_contracts: RelatedContracts::default(),
                subscribe: false,
                blocking_subscribe: false,
            }))
            .await?;
        loop {
            match self.api.recv().await? {
                HostResponse::ContractResponse(ContractResponse::PutResponse { .. }) => {
                    return Ok(())
                }
                HostResponse::ContractResponse(ContractResponse::UpdateNotification { .. }) => {}
                other => bail!("unexpected response to Put: {other:?}"),
            }
        }
    }

    async fn update(&mut self, key: ContractKey, delta: Vec<u8>) -> anyhow::Result<()> {
        self.api
            .send(ClientRequest::ContractOp(ContractRequest::Update {
                key,
                data: UpdateData::Delta(StateDelta::from(delta)),
            }))
            .await?;
        loop {
            match self.api.recv().await? {
                HostResponse::ContractResponse(ContractResponse::UpdateResponse { .. }) => {
                    return Ok(())
                }
                HostResponse::ContractResponse(ContractResponse::UpdateNotification { .. }) => {}
                other => bail!("unexpected response to Update: {other:?}"),
            }
        }
    }

    async fn delegate(&mut self, req: Request) -> anyhow::Result<Response> {
        self.api
            .send(ClientRequest::DelegateOp(
                DelegateRequest::ApplicationMessages {
                    key: self.delegate_key.clone(),
                    params: Parameters::from(Vec::<u8>::new()),
                    inbound: vec![InboundDelegateMsg::ApplicationMessage(
                        ApplicationMessage::new(req.encode()),
                    )],
                },
            ))
            .await?;
        match self.api.recv().await? {
            HostResponse::DelegateResponse { values, .. } => {
                for msg in values {
                    if let OutboundDelegateMsg::ApplicationMessage(app) = msg {
                        return Response::decode(&app.payload)
                            .map_err(|e| anyhow!("delegate sent an undecodable reply: {e:?}"));
                    }
                }
                bail!("delegate returned no application message")
            }
            other => bail!("unexpected response to delegate call: {other:?}"),
        }
    }
}
