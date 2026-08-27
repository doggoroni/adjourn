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
//! Two refinements of that, both of them bugs if you get them wrong.
//!
//! **Not every call of the error handler is a dead socket.** `freenet-stdlib`
//! funnels far more than socket death through it: a non-binary frame
//! (`browser.rs` ~55), a bincode deserialize failure (~69), two stream
//! reassembly failures (~100, ~112) and four send-side paths (~187, ~204,
//! ~238, ~242) all call the same closure, alongside the genuine `onerror`
//! (~128) and `onclose` (~152). Synthesising `Closed` for all of them means one
//! undeserialisable frame -- a version skew, a reassembly failure past
//! `MAX_CONCURRENT_STREAMS` -- ends `watch` on a perfectly live socket with no
//! error surfaced anywhere, which is the exact failure `watch` exists to
//! prevent. [`socket_is_gone`] draws the line, and everything on the other side
//! of it becomes a [`Frame::Failed`]: an error for whoever is waiting, a skip
//! for whoever is not.
//!
//! **A close is one queue item, and only one waiter gets it.** A graceful
//! server-side close (a node restart, code 1000/1001) fires `onclose` exactly
//! once. If a request is in flight, `next_response` consumes that single
//! `Closed` and bails correctly -- and then the app resumes watching,
//! `next_update` finds `pending` empty, and parks on an inbox that will never
//! yield another frame and can never end. It would hang forever on a dead
//! socket, showing a stale board. So the close is LATCHED: [`CloseLatch`]
//! records the first genuine close, and both `next_response` and `next_update`
//! consult it before they await. A recoverable failure must never latch it.
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
/// which is a private alias, hence the type spelled out). `Failed` and `Closed`
/// are both synthesised by the error handler and have no `WebApi` counterpart;
/// [`socket_is_gone`] decides which of the two a given stdlib error becomes.
///
/// Not boxed, despite `clippy::large_enum_variant`: the large variant is the
/// one that arrives on every single frame, while `Failed` and `Closed` arrive
/// only when something has gone wrong. Boxing would buy 200-odd bytes on the
/// rare cases by adding an allocation to the hot one. (There is no bound on how
/// many of either arrive: the stdlib's error handler is called for recoverable
/// decode failures too, and nothing stops `onerror` and `onclose` both firing.)
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Frame {
    /// What the node said: a response, or a failure it attributes to a request.
    Result(Result<HostResponse, ClientError>),
    /// The transport reported something that is NOT the socket dying -- an
    /// undecodable frame, a reassembly failure, a refused send. Fatal to a
    /// request that is waiting, survivable for the connection.
    Failed(String),
    /// The socket errored or closed. Terminal for every waiter, forever.
    Closed(String),
}

/// Does this `freenet-stdlib` error mean the socket itself is gone?
///
/// The argument is the `source` field of the JSON payload the stdlib puts in
/// `Error::ConnectionError`. Only two of its call sites are the socket dying:
/// `onclose` tags itself `"close"`, and `onerror` -- which the WebSocket spec
/// fires only on a connection failure, always followed by a close -- tags
/// itself `"exec error"`. Every other tag (`"host response decoding"`,
/// `"host response deserialization"`, `"stream reassembly deserialization"`,
/// `"streaming reassembly"`) is a frame-level problem on a live socket, and the
/// send-side paths carry no `source` at all -- they set `origin` instead, and
/// their failure is already returned to the caller by `WebApi::send`, so
/// treating them as survivable here loses nothing.
///
/// Erring towards "survivable" is the safe direction. A genuine close misread
/// as recoverable still surfaces: the next `send` fails its ready-state
/// precondition, and the next frame the socket produces is the `onclose` that
/// does latch. A recoverable error misread as a close latches [`CloseLatch`]
/// permanently and silently ends `watch` on a live socket.
///
/// Chosen over inspecting `WebSocket::ready_state()` in the handler because it
/// is a pure function of data the stdlib already hands us -- so it is decided
/// off-wasm and unit-tested, the `route` precedent -- whereas `ready_state` is
/// a live JS object, testable only in a browser, and is not even reliable at
/// `onerror` time (the spec does not pin the state transition to the event).
pub fn socket_is_gone(source: Option<&str>) -> bool {
    matches!(source, Some("close") | Some("exec error"))
}

/// The sticky record of a socket that has died.
///
/// `Frame::Closed` is a single queue item and only one waiter can consume it,
/// so "the socket is dead" cannot be represented by the frame alone -- see the
/// module docs. This latch turns that one-shot event into a permanent state
/// every later call can read.
#[derive(Debug, Default)]
pub struct CloseLatch(Option<String>);

impl CloseLatch {
    /// Latch iff `routed` is a genuine close. First reason wins: it is the one
    /// that explains why the connection ended.
    pub fn observe(&mut self, routed: &Routed) {
        if let Routed::Closed(why) = routed {
            if self.0.is_none() {
                self.0 = Some(why.clone());
            }
        }
    }

    /// Why the connection is gone, or `None` while it may still be alive.
    pub fn why(&self) -> Option<&str> {
        self.0.as_deref()
    }
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
    /// The node has no record of this contract instance --
    /// `ContractError::MissingContract`. Not a request failure: `get`'s
    /// `NodeClient` contract treats "the network hasn't seen this contract"
    /// as `Ok(None)`, the same case `WsClient::get` (`cli/src/ws.rs`) and
    /// `FakeNode::get` (`client/src/fake.rs`) each classify at their own
    /// transport boundary. Kept separate from `Failed` rather than folded
    /// into it so `get` can tell the two apart without re-parsing a string.
    ContractMissing,
}

/// Classify one frame. Pure, so it can be tested without a browser.
pub fn route(frame: Frame) -> Routed {
    match frame {
        Frame::Closed(why) => Routed::Closed(why),
        Frame::Failed(why) => Routed::Failed(why),
        Frame::Result(Err(e)) => {
            if adjourn_client::node::is_missing_contract(&e) {
                Routed::ContractMissing
            } else {
                Routed::Failed(e.to_string())
            }
        }
        Frame::Result(Ok(HostResponse::ContractResponse(
            ContractResponse::UpdateNotification { key, update },
        ))) => Routed::Notification(*key.id(), update),
        Frame::Result(Ok(other)) => Routed::Response(other),
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::{route, socket_is_gone, CloseLatch, Frame, Routed};
    use adjourn_client::node::NodeClient;
    use adjourn_core::delegate_api::{Request, Response};
    use anyhow::{anyhow, bail};
    use freenet_stdlib::client_api::{
        ClientRequest, ContractRequest, ContractResponse, DelegateRequest, Error as StdlibError,
        HostResponse, WebApi,
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
        /// Set once, by the first genuine close. See [`CloseLatch`].
        closed: CloseLatch,
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
                    // Is the socket actually gone, or is this a frame the
                    // stdlib could not decode on a live one? The handler is
                    // called for both (see `socket_is_gone`), and only the
                    // former may become a `Closed`. The stdlib puts a JSON
                    // payload in `ConnectionError` whose `source` field names
                    // the call site; that payload's type is wasm-only, so it is
                    // unwrapped here and the decision itself stays in the
                    // ungated, unit-tested `socket_is_gone`.
                    let gone = match &err {
                        StdlibError::ConnectionError(detail) => {
                            socket_is_gone(detail.get("source").and_then(|s| s.as_str()))
                        }
                        StdlibError::ConnectionClosed | StdlibError::ChannelClosed => true,
                        // Non-exhaustive upstream. An unrecognised error is
                        // reported, not treated as a death sentence.
                        _ => false,
                    };
                    let why = format!("{err:?}");
                    // Job 1, at most once: fail `connect` if it is still
                    // waiting. Without this a node that is down -- a bad URL, a
                    // refused connection -- would fire `onerror` into a handler
                    // that discarded it, `onopen` would never fire, and
                    // `open_rx.await` below would hang forever. ANY error
                    // resolves it, `gone` or not: nothing useful can arrive on
                    // a socket that never opened.
                    if let Some(tx) = err_tx.borrow_mut().take() {
                        let _ = tx.send(Err(why.clone()));
                    }
                    // Job 2, always: tell whoever is reading the inbox. This
                    // handler must NOT go quiet after connect -- it is the only
                    // path from a dead socket to a Rust error. The inbox cannot
                    // signal it by ending, because freenet-stdlib `forget()`s
                    // the onmessage closure and so never drops the sender.
                    let frame = if gone {
                        Frame::Closed(why)
                    } else {
                        Frame::Failed(why)
                    };
                    let _ = err_inbox.unbounded_send(frame);
                },
                move || {
                    if let Some(tx) = open_tx.borrow_mut().take() {
                        let _ = tx.send(Ok(()));
                    }
                },
            );

            // Bounded, for the same reason every request is. Resolving on
            // failure closes the refused-connection case -- `onclose` fires
            // and the handler above fails `open_rx`. It does NOT close the
            // case where the SYN is silently DROPPED rather than refused (a
            // firewall, a VPN, a sandboxed CI network): no `onerror`, no
            // `onclose`, no `onopen`, and nothing to resolve the oneshot. That
            // is the original hang with a narrower trigger, and it is
            // indistinguishable from a slow node -- which is exactly the
            // report-a-healthy-thing-as-broken confusion the rest of this file
            // exists to avoid. So connect gets the same `RESPONSE_TIMEOUT` as
            // a request. `next_update` remains the one deliberate exemption:
            // a correspondence move can take days, but a TCP handshake cannot.
            let expired = after(RESPONSE_TIMEOUT)?;
            pin_mut!(expired, open_rx);
            match select(open_rx, expired).await {
                Either::Left((Ok(Ok(())), _)) => {}
                Either::Left((Ok(Err(e)), _)) => bail!("could not connect to {url}: {e}"),
                Either::Left((Err(_), _)) => bail!("the WebSocket closed before it opened"),
                Either::Right(((), _)) => {
                    bail!("timed out after {RESPONSE_TIMEOUT:?} connecting to {url}")
                }
            }

            Ok(Self {
                api,
                inbox,
                pending: VecDeque::new(),
                delegate_key: None,
                closed: CloseLatch::default(),
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

        /// Pull frames until one this connection's callers must react to: a
        /// response, a failure, a missing-contract report, or the connection
        /// closing. Notifications along the way are parked in `pending`
        /// rather than returned -- shared by `next_response`, which turns
        /// every non-response case into an error, and `get`, which alone
        /// treats `ContractMissing` as `Ok(None)` rather than a failure.
        async fn next_routed(&mut self, op: &str) -> anyhow::Result<Routed> {
            // The latch first: a close another waiter already consumed is still
            // a close, and awaiting a frame that can never come would burn the
            // whole timeout before saying so.
            if let Some(why) = self.closed.why() {
                bail!("the connection is closed, so {op} can never be answered: {why}");
            }
            loop {
                let routed = route(self.next_frame(op).await?);
                self.closed.observe(&routed);
                if let Routed::Notification(id, update) = routed {
                    self.pending.push_back((id, update));
                    continue;
                }
                return Ok(routed);
            }
        }

        /// The next request answer. Every non-response outcome -- a node
        /// failure, a missing contract, or the connection closing -- is an
        /// error here; only [`BrowserClient::get`] treats `ContractMissing`
        /// as anything else.
        async fn next_response(&mut self, op: &str) -> anyhow::Result<HostResponse> {
            match self.next_routed(op).await? {
                Routed::Response(resp) => Ok(resp),
                Routed::Failed(why) => {
                    bail!("the node reported an error while waiting for {op}: {why}")
                }
                Routed::Closed(why) => {
                    bail!("the connection closed while waiting for {op}: {why}")
                }
                Routed::ContractMissing => {
                    bail!("the node has no record of this contract while waiting for {op}")
                }
                Routed::Notification(..) => {
                    unreachable!("notifications are drained by next_routed")
                }
            }
        }

        /// Whether the underlying socket is known dead — a genuine close or
        /// send-side failure the transport classified as [`socket_is_gone`],
        /// latched the first time it is observed (see [`CloseLatch`]).
        ///
        /// This is the line the actor in `conn.rs` needs: a node-reported
        /// refusal, an out-of-turn move, a bad paste -- none of those touch
        /// the socket, so none of them should tear down a healthy connection
        /// or re-run delegate registration. Only a transport death should.
        pub fn is_disconnected(&self) -> bool {
            self.closed.why().is_some()
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
                match self.next_routed("Get").await? {
                    Routed::Response(HostResponse::ContractResponse(
                        ContractResponse::GetResponse { state, .. },
                    )) => return Ok(Some(state.as_ref().to_vec())),
                    Routed::Response(HostResponse::ContractResponse(
                        ContractResponse::NotFound { .. },
                    )) => return Ok(None),
                    // A subscribe ack or a stray notification can arrive first.
                    Routed::Response(HostResponse::ContractResponse(
                        ContractResponse::SubscribeResponse { .. },
                    )) => {}
                    Routed::Response(other) => bail!("unexpected response to Get: {other:?}"),
                    // The node has no record of this contract -- not a
                    // request failure, per `NodeClient::get`'s contract (see
                    // `client/src/node.rs`).
                    Routed::ContractMissing => return Ok(None),
                    Routed::Failed(why) => {
                        bail!("the node reported an error while waiting for Get: {why}")
                    }
                    Routed::Closed(why) => {
                        bail!("the connection closed while waiting for Get: {why}")
                    }
                    Routed::Notification(..) => {
                        unreachable!("notifications are drained by next_routed")
                    }
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
            // Parked notifications first: they were genuinely received, and a
            // socket that has since died does not un-receive them.
            if let Some(parked) = self.pending.pop_front() {
                return Ok(Some(parked));
            }
            // Then the latch, BEFORE awaiting. A close is one queue item; if an
            // in-flight request consumed it, this await would never wake --
            // there is deliberately no timeout here, and the inbox never ends.
            if self.closed.why().is_some() {
                return Ok(None);
            }
            loop {
                let Some(frame) = self.inbox.next().await else {
                    return Ok(None);
                };
                let routed = route(frame);
                self.closed.observe(&routed);
                match routed {
                    Routed::Notification(id, update) => return Ok(Some((id, update))),
                    // The socket is gone. `NodeClient` documents `None` as
                    // "no more updates will arrive", which is exactly true.
                    Routed::Closed(_) => return Ok(None),
                    // A late answer -- or a failure the socket survived,
                    // such as one undecodable frame or a stray missing-contract
                    // report -- with nobody waiting on it. Ending a healthy
                    // session over either would be the very stall `watch`
                    // exists to prevent.
                    Routed::Response(_) | Routed::Failed(_) | Routed::ContractMissing => continue,
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

    /// 16 bytes from the browser's CSPRNG, for an invite's nonce.
    ///
    /// The nonce distinguishes repeat matchups between the same two players and
    /// has exactly one author, the inviter — that is what stops the two sides
    /// deriving different `GameParams`.
    pub fn browser_nonce() -> anyhow::Result<[u8; 16]> {
        let mut bytes = [0u8; 16];
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
