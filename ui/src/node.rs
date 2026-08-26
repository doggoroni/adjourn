//! The browser transport.
//!
//! `freenet-stdlib`'s browser `WebApi` is callback-based: it takes a result
//! handler and pushes into it, with no `recv()` (the native one has one). But
//! `NodeClient`'s methods are request/response. An unbounded channel bridges
//! the two -- the handler sends, the client awaits.
//!
//! The channel carries a [`Frame`], not a bare `HostResponse`, and that is the
//! whole error story on this transport. The handler's argument is
//! `Result<HostResponse, ClientError>`: the node reports PER-REQUEST failures
//! -- a rejected `Update`, a contract execution error, an `ApplicationMessages`
//! against an unbound key -- through the `Err` side. An earlier version matched
//! `if let Ok(resp)` and dropped every one of them, so a rejected move left
//! `next_response` awaiting a channel that would never yield: the button spun
//! forever, indistinguishable from a healthy idle correspondence game.
//!
//! [`Frame::Closed`] is the other half. `WebApi` wires both `onerror` and
//! `onclose` to the SAME error-handler closure (`client_api/browser.rs`, the
//! `onerror_callback` at ~128 and the `onclose_callback` at ~152), and it
//! `forget()`s the onmessage closure (~125) -- so the result handler's sender
//! is never dropped and `inbox.next()` can never return `None`. A handler that
//! only resolved `connect` and then went quiet would make every post-connect
//! socket death invisible, and "connection closed" unreachable dead code. So
//! the error handler stays live for the client's lifetime and pushes a
//! `Closed` marker into the same inbox, which is what lets `next_update`
//! honestly return `Ok(None)` per `NodeClient`'s contract.
//!
//! Update notifications arrive on the same channel as request answers, so they
//! are separated by [`route`] and parked in `pending`. Mistaking one for the
//! other would let whichever request is in flight swallow a move, which is
//! exactly the failure `watch` exists to avoid.
//!
//! `route`/`Routed`/`Frame` are kept target-independent and ungated:
//! `HostResponse`, `ContractResponse`, `ClientError` and `UpdateData` exist on
//! every target, so the classification -- including both failure arms -- is
//! unit-tested off-wasm. Everything else here touches
//! `freenet_stdlib::client_api::WebApi`, which `freenet-stdlib` resolves to a
//! different, single-argument, non-callback type on a native target
//! (`cfg(all(target_family = "wasm", feature = "net"))` gates the browser one).
//! So the browser client itself is gated to wasm32 -- without that gate the
//! crate fails to compile natively at all, which would also take the
//! native-only board tests down with it.

use freenet_stdlib::client_api::{ClientError, ContractResponse, HostResponse};
use freenet_stdlib::prelude::*;

/// One thing that came off the bridge channel.
///
/// `Result` mirrors `WebApi`'s handler argument (the stdlib's `HostResult`,
/// which is a private alias, hence the type spelled out). `Closed` is
/// synthesised by the error handler and has no `WebApi` counterpart.
///
/// Not boxed, despite `clippy::large_enum_variant`: the large variant is the
/// one that arrives on every single frame, and `Closed` arrives at most twice
/// in a session. Boxing would buy 200-odd bytes on the rarest case by adding
/// an allocation to the hottest one.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Frame {
    /// What the node said: a response, or a failure it attributes to a request.
    Result(Result<HostResponse, ClientError>),
    /// The socket errored or closed. Terminal for every waiter.
    Closed(String),
}

/// What a frame from the node turned out to be.
#[derive(Debug)]
pub enum Routed {
    Response(HostResponse),
    Notification(ContractInstanceId, UpdateData<'static>),
    /// The node reported a failure. Fatal to whichever request is waiting,
    /// but not necessarily to the connection.
    Failed(String),
    /// The connection is gone. Fatal to everything.
    Closed(String),
}

/// Classify one frame. Pure, so it can be tested without a browser.
pub fn route(frame: Frame) -> Routed {
    match frame {
        Frame::Closed(why) => Routed::Closed(why),
        Frame::Result(Err(e)) => Routed::Failed(e.to_string()),
        Frame::Result(Ok(HostResponse::ContractResponse(
            ContractResponse::UpdateNotification { key, update },
        ))) => Routed::Notification(*key.id(), update),
        Frame::Result(Ok(other)) => Routed::Response(other),
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::{route, Frame, Routed};
    use adjourn_client::node::NodeClient;
    use adjourn_core::delegate_api::{Request, Response};
    use anyhow::{anyhow, bail};
    use freenet_stdlib::client_api::{
        ClientRequest, ContractRequest, ContractResponse, DelegateRequest, HostResponse, WebApi,
    };
    use freenet_stdlib::prelude::*;
    use futures::channel::{mpsc, oneshot};
    use futures::future::{select, Either};
    use futures::{pin_mut, StreamExt};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::rc::Rc;
    use std::time::Duration;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    /// How long a single request will wait for the node before giving up.
    ///
    /// Mirrors `cli/src/ws.rs`'s `RESPONSE_TIMEOUT`, for the same reason: a
    /// node that accepts the connection and never answers otherwise hangs the
    /// caller forever with no error and nothing to act on. In a browser that
    /// is a spinner that never stops.
    ///
    /// `next_update` is deliberately NOT bounded by this -- the same asymmetry
    /// `ws.rs` documents. A correspondence move can legitimately take days;
    /// timing the wait for the opponent out would be a bug, not a safeguard.
    const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

    /// A live `setTimeout`, cancelled on drop.
    ///
    /// Both halves matter: keeping the `Closure` alive is what stops the
    /// browser calling into freed memory, and clearing the handle is what
    /// stops a request that answered in time from leaving a pending callback
    /// behind. Drop order does the right thing -- `Drop::drop` runs before the
    /// fields are dropped, so the timer is cancelled before the closure goes.
    struct Timer {
        handle: i32,
        _cb: Closure<dyn FnMut()>,
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            if let Some(w) = web_sys::window() {
                w.clear_timeout_with_handle(self.handle);
            }
        }
    }

    /// A future that resolves after `d`, using the browser's own timer.
    ///
    /// There is no `tokio::time` on this target; this is why `wasm-bindgen` is
    /// a dependency of this crate.
    fn after(d: Duration) -> anyhow::Result<impl Future<Output = ()>> {
        let (tx, rx) = oneshot::channel::<()>();
        let tx = Rc::new(RefCell::new(Some(tx)));
        let cb = Closure::<dyn FnMut()>::new(move || {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(());
            }
        });
        let handle = web_sys::window()
            .ok_or_else(|| anyhow!("no window"))?
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                d.as_millis() as i32,
            )
            .map_err(|e| anyhow!("setTimeout failed: {e:?}"))?;
        let timer = Timer { handle, _cb: cb };
        Ok(async move {
            let _timer = timer;
            let _ = rx.await;
        })
    }

    pub struct BrowserClient {
        api: WebApi,
        inbox: mpsc::UnboundedReceiver<Frame>,
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

            let (tx, inbox) = mpsc::unbounded::<Frame>();
            let err_inbox = tx.clone();
            let (open_tx, open_rx) = oneshot::channel::<Result<(), String>>();
            // `onerror`/`onclose` route to the error handler, NOT to
            // `onopen_handler` -- freenet-stdlib's browser `WebApi` wires both
            // to the same error-handler closure (see `client_api/browser.rs`'s
            // `onerror_callback` at line 128 and `onclose_callback` at line
            // 152, both calling `eh(...)`, never the onopen callback). So the
            // error handler has two jobs, and it keeps doing BOTH for the life
            // of the client.
            let open_tx = Rc::new(RefCell::new(Some(open_tx)));
            let err_tx = Rc::clone(&open_tx);

            let api = WebApi::start(
                socket,
                move |result| {
                    // A closed receiver means the client is gone; nothing to do.
                    let _ = tx.unbounded_send(Frame::Result(result));
                },
                move |err| {
                    let why = format!("{err:?}");
                    // Job 1, at most once: fail `connect` if it is still
                    // waiting. Without this a node that is down -- a bad URL, a
                    // refused connection -- would fire `onerror` into a handler
                    // that discarded it, `onopen` would never fire, and
                    // `open_rx.await` below would hang forever.
                    if let Some(tx) = err_tx.borrow_mut().take() {
                        let _ = tx.send(Err(why.clone()));
                    }
                    // Job 2, always: tell whoever is reading the inbox. This
                    // handler must NOT go quiet after connect -- it is the only
                    // path from a dead socket to a Rust error. The inbox cannot
                    // signal it by ending, because freenet-stdlib `forget()`s
                    // the onmessage closure and so never drops the sender.
                    let _ = err_inbox.unbounded_send(Frame::Closed(why));
                },
                move || {
                    if let Some(tx) = open_tx.borrow_mut().take() {
                        let _ = tx.send(Ok(()));
                    }
                },
            );

            match open_rx.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => bail!("could not connect to {url}: {e}"),
                Err(_) => bail!("the WebSocket closed before it opened"),
            }

            Ok(Self {
                api,
                inbox,
                pending: VecDeque::new(),
                delegate_key: None,
            })
        }

        /// One frame, bounded by [`RESPONSE_TIMEOUT`]. `op` names the request
        /// being waited on, so a timeout says what hung rather than just that
        /// something did.
        async fn next_frame(&mut self, op: &str) -> anyhow::Result<Frame> {
            let expired = after(RESPONSE_TIMEOUT)?;
            let arrived = self.inbox.next();
            pin_mut!(expired, arrived);
            match select(arrived, expired).await {
                Either::Left((Some(frame), _)) => Ok(frame),
                Either::Left((None, _)) => {
                    bail!("the connection closed while waiting for {op}")
                }
                Either::Right(((), _)) => {
                    bail!("timed out after {RESPONSE_TIMEOUT:?} waiting for a response to {op}")
                }
            }
        }

        /// The next request answer, parking any notification that arrives first.
        ///
        /// Both failure arms end the wait. A `Failed` frame is the node saying
        /// this request will never be answered; a `Closed` frame is the socket
        /// saying nothing ever will be.
        async fn next_response(&mut self, op: &str) -> anyhow::Result<HostResponse> {
            loop {
                match route(self.next_frame(op).await?) {
                    Routed::Response(resp) => return Ok(resp),
                    Routed::Notification(id, update) => self.pending.push_back((id, update)),
                    Routed::Failed(why) => {
                        bail!("the node reported an error while waiting for {op}: {why}")
                    }
                    Routed::Closed(why) => {
                        bail!("the connection closed while waiting for {op}: {why}")
                    }
                }
            }
        }

        pub async fn register_delegate(
            &mut self,
            container: DelegateContainer,
        ) -> anyhow::Result<()> {
            let key = container.key().clone();
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
                HostResponse::Ok | HostResponse::DelegateResponse { .. } => {
                    // Only recorded once the node has actually confirmed the
                    // registration -- setting this eagerly would let a failed
                    // registration still pass `delegate()`'s "no delegate
                    // registered" guard and send a call against a key the
                    // node never bound.
                    self.delegate_key = Some(key);
                    Ok(())
                }
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

        /// Deliberately unbounded: see `RESPONSE_TIMEOUT`. The opponent may
        /// take days.
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
                    // The socket is gone. `NodeClient` documents `None` as
                    // "no more updates will arrive", which is exactly true.
                    Routed::Closed(_) => return Ok(None),
                    // A late answer -- or a late failure -- for a request
                    // nobody is waiting on. Dropping the connection over one
                    // would end a healthy session.
                    Routed::Response(_) | Routed::Failed(_) => continue,
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
