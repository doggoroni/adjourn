//! The browser transport.
//!
//! `freenet-stdlib`'s browser `WebApi` is callback-based: it takes a result
//! handler and pushes into it, with no `recv()` (the native one has one). But
//! `NodeClient`'s methods are request/response. An unbounded channel bridges
//! the two -- the handler sends, the client awaits.
//!
//! Update notifications arrive on the same channel as request answers, so they
//! are separated by [`route`] and parked in `pending`. Mistaking one for the
//! other would let whichever request is in flight swallow a move, which is
//! exactly the failure `watch` exists to avoid.
//!
//! `route`/`Routed` are kept target-independent and ungated: `HostResponse`,
//! `ContractResponse` and `UpdateData` exist on every target. Everything else
//! here touches `freenet_stdlib::client_api::WebApi`, which `freenet-stdlib`
//! resolves to a different, single-argument, non-callback type on a native
//! target (`cfg(all(target_family = "wasm", feature = "net"))` gates the
//! browser one). So the browser client itself is gated to wasm32 -- without
//! that gate the crate fails to compile natively at all, which would also
//! take the native-only board tests down with it.

use freenet_stdlib::client_api::{ContractResponse, HostResponse};
use freenet_stdlib::prelude::*;

/// What a frame from the node turned out to be.
#[derive(Debug)]
pub enum Routed {
    Response(HostResponse),
    Notification(ContractInstanceId, UpdateData<'static>),
    Ignored,
}

/// Classify one frame. Pure, so it can be tested without a browser.
pub fn route(resp: HostResponse) -> Routed {
    match resp {
        HostResponse::ContractResponse(ContractResponse::UpdateNotification { key, update }) => {
            Routed::Notification(*key.id(), update)
        }
        other => Routed::Response(other),
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::{route, Routed};
    use adjourn_client::node::NodeClient;
    use adjourn_core::delegate_api::{Request, Response};
    use anyhow::{anyhow, bail};
    use freenet_stdlib::client_api::{
        ClientRequest, ContractRequest, ContractResponse, DelegateRequest, HostResponse, WebApi,
    };
    use freenet_stdlib::prelude::*;
    use futures::channel::{mpsc, oneshot};
    use futures::StreamExt;
    use std::collections::VecDeque;

    pub struct BrowserClient {
        api: WebApi,
        inbox: mpsc::UnboundedReceiver<HostResponse>,
        pending: VecDeque<(ContractInstanceId, UpdateData<'static>)>,
        delegate_key: Option<DelegateKey>,
    }

    impl BrowserClient {
        /// Open a WebSocket to the node and wait for it to be usable.
        ///
        /// Sending before `onopen` fires is silently dropped by the browser, so the
        /// connect future does not resolve until the socket is open.
        pub async fn connect(url: &str) -> anyhow::Result<Self> {
            let socket = web_sys::WebSocket::new(url)
                .map_err(|e| anyhow!("could not open a WebSocket to {url}: {e:?}"))?;

            let (tx, inbox) = mpsc::unbounded();
            let (open_tx, open_rx) = oneshot::channel();
            let mut open_tx = Some(open_tx);

            let api = WebApi::start(
                socket,
                move |result| {
                    if let Ok(resp) = result {
                        // A closed receiver means the client is gone; nothing to do.
                        let _ = tx.unbounded_send(resp);
                    }
                },
                |_err| {},
                move || {
                    if let Some(tx) = open_tx.take() {
                        let _ = tx.send(());
                    }
                },
            );

            open_rx
                .await
                .map_err(|_| anyhow!("the WebSocket closed before it opened"))?;

            Ok(Self {
                api,
                inbox,
                pending: VecDeque::new(),
                delegate_key: None,
            })
        }

        /// The next request answer, parking any notification that arrives first.
        async fn next_response(&mut self, op: &str) -> anyhow::Result<HostResponse> {
            loop {
                let frame = self
                    .inbox
                    .next()
                    .await
                    .ok_or_else(|| anyhow!("the connection closed while waiting for {op}"))?;
                match route(frame) {
                    Routed::Response(resp) => return Ok(resp),
                    Routed::Notification(id, update) => self.pending.push_back((id, update)),
                    Routed::Ignored => {}
                }
            }
        }

        pub async fn register_delegate(
            &mut self,
            container: DelegateContainer,
        ) -> anyhow::Result<()> {
            self.delegate_key = Some(container.key().clone());
            self.api
                .send(ClientRequest::DelegateOp(
                    DelegateRequest::RegisterDelegate {
                        delegate: container,
                        cipher: [0u8; 32],
                        nonce: [0u8; 24],
                    },
                ))
                .await
                .map_err(|e| anyhow!("sending RegisterDelegate: {e}"))?;
            match self.next_response("RegisterDelegate").await? {
                HostResponse::Ok | HostResponse::DelegateResponse { .. } => Ok(()),
                other => bail!("unexpected response to RegisterDelegate: {other:?}"),
            }
        }
    }

    impl NodeClient for BrowserClient {
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
                    // When we asked to subscribe, wait for the subscription to be
                    // established before the node answers. With `false` the node
                    // replies first and the subscription lands later, leaving a
                    // window in which the opponent's move is broadcast to
                    // subscribers we are not yet among -- lost with no error, and
                    // a terminal stuck on a stale position forever.
                    blocking_subscribe: subscribe,
                }))
                .await
                .map_err(|e| anyhow!("sending Get: {e}"))?;
            loop {
                match self.next_response("Get").await? {
                    HostResponse::ContractResponse(ContractResponse::GetResponse {
                        state, ..
                    }) => return Ok(Some(state.as_ref().to_vec())),
                    HostResponse::ContractResponse(ContractResponse::NotFound { .. }) => {
                        return Ok(None)
                    }
                    // A subscribe ack or a stray notification can arrive first.
                    HostResponse::ContractResponse(ContractResponse::SubscribeResponse {
                        ..
                    }) => {}
                    other => bail!("unexpected response to Get: {other:?}"),
                }
            }
        }

        async fn put(
            &mut self,
            container: ContractContainer,
            state: Vec<u8>,
        ) -> anyhow::Result<()> {
            self.api
                .send(ClientRequest::ContractOp(ContractRequest::Put {
                    contract: container,
                    state: WrappedState::new(state),
                    related_contracts: RelatedContracts::default(),
                    subscribe: false,
                    blocking_subscribe: false,
                }))
                .await
                .map_err(|e| anyhow!("sending Put: {e}"))?;
            loop {
                match self.next_response("Put").await? {
                    HostResponse::ContractResponse(ContractResponse::PutResponse { .. }) => {
                        return Ok(())
                    }
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
                .await
                .map_err(|e| anyhow!("sending Update: {e}"))?;
            loop {
                match self.next_response("Update").await? {
                    HostResponse::ContractResponse(ContractResponse::UpdateResponse { .. }) => {
                        return Ok(())
                    }
                    other => bail!("unexpected response to Update: {other:?}"),
                }
            }
        }

        async fn delegate(&mut self, req: Request) -> anyhow::Result<Response> {
            let delegate_key = self
                .delegate_key
                .clone()
                .ok_or_else(|| anyhow!("no delegate registered"))?;
            self.api
                .send(ClientRequest::DelegateOp(
                    DelegateRequest::ApplicationMessages {
                        key: delegate_key,
                        params: Parameters::from(Vec::<u8>::new()),
                        inbound: vec![InboundDelegateMsg::ApplicationMessage(
                            ApplicationMessage::new(req.encode()),
                        )],
                    },
                ))
                .await
                .map_err(|e| anyhow!("sending delegate call: {e}"))?;
            loop {
                match self.next_response("delegate call").await? {
                    HostResponse::DelegateResponse { values, .. } => {
                        for msg in values {
                            if let OutboundDelegateMsg::ApplicationMessage(app) = msg {
                                return Response::decode(&app.payload).map_err(|e| {
                                    anyhow!("delegate sent an undecodable reply: {e:?}")
                                });
                            }
                        }
                        bail!("delegate returned no application message")
                    }
                    other => bail!("unexpected response to delegate call: {other:?}"),
                }
            }
        }

        async fn next_update(
            &mut self,
        ) -> anyhow::Result<Option<(ContractInstanceId, UpdateData<'static>)>> {
            if let Some(parked) = self.pending.pop_front() {
                return Ok(Some(parked));
            }
            loop {
                let Some(frame) = self.inbox.next().await else {
                    return Ok(None);
                };
                match route(frame) {
                    Routed::Notification(id, update) => return Ok(Some((id, update))),
                    // A late answer to a request nobody is waiting on. Dropping the
                    // connection over it would end a healthy session.
                    Routed::Response(_) | Routed::Ignored => continue,
                }
            }
        }
    }

    /// 32 bytes from the browser's CSPRNG.
    ///
    /// `adjourn-client` takes entropy as a parameter precisely so this crate needs
    /// no `getrandom`: a `getrandom` in the graph emits wasm-bindgen placeholder
    /// imports, which is what makes a contract fail to instantiate.
    pub fn browser_entropy() -> anyhow::Result<[u8; 32]> {
        let mut bytes = [0u8; 32];
        web_sys::window()
            .ok_or_else(|| anyhow!("no window"))?
            .crypto()
            .map_err(|e| anyhow!("no crypto: {e:?}"))?
            .get_random_values_with_u8_array(&mut bytes)
            .map_err(|e| anyhow!("crypto.getRandomValues failed: {e:?}"))?;
        Ok(bytes)
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::*;
