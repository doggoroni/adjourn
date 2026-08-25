//! The tungstenite transport. Lives in the CLI because `tokio-tungstenite`
//! pulls `mio`, which has no wasm32 backend — that is the whole reason
//! `adjourn-client` exists as a separate crate.

use adjourn_client::node::NodeClient;
use adjourn_core::delegate_api::{Request, Response};
use anyhow::{anyhow, bail, Context};
use freenet_stdlib::client_api::{
    ClientRequest, ContractRequest, ContractResponse, DelegateRequest, HostResponse, WebApi,
};
use freenet_stdlib::prelude::*;
use std::time::Duration;

/// How long a single request will wait for the node's response before giving
/// up. Without this, a node that accepts the handshake and never answers
/// hangs the CLI forever -- no output, no exit code, nothing for a caller (or
/// a script) to act on.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

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

    /// `self.api.recv()`, bounded by [`RESPONSE_TIMEOUT`]. `op` names the
    /// request this receive is waiting on, so a timeout error says what hung
    /// rather than just that something did.
    async fn recv_timeout(&mut self, op: &str) -> anyhow::Result<HostResponse> {
        match tokio::time::timeout(RESPONSE_TIMEOUT, self.api.recv()).await {
            Ok(result) => result.map_err(anyhow::Error::from),
            Err(_) => Err(anyhow!(
                "timed out after {RESPONSE_TIMEOUT:?} waiting for a response to {op}"
            )),
        }
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
        loop {
            match self.recv_timeout("RegisterDelegate").await? {
                HostResponse::Ok | HostResponse::DelegateResponse { .. } => return Ok(()),
                HostResponse::ContractResponse(ContractResponse::UpdateNotification { .. }) => {}
                other => bail!("unexpected response to RegisterDelegate: {other:?}"),
            }
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
            match self.recv_timeout("Get").await? {
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
            match self.recv_timeout("Put").await? {
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
            match self.recv_timeout("Update").await? {
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
        loop {
            match self.recv_timeout("delegate call").await? {
                HostResponse::DelegateResponse { values, .. } => {
                    for msg in values {
                        if let OutboundDelegateMsg::ApplicationMessage(app) = msg {
                            return Response::decode(&app.payload)
                                .map_err(|e| anyhow!("delegate sent an undecodable reply: {e:?}"));
                        }
                    }
                    bail!("delegate returned no application message")
                }
                HostResponse::ContractResponse(ContractResponse::UpdateNotification { .. }) => {}
                other => bail!("unexpected response to delegate call: {other:?}"),
            }
        }
    }
}
