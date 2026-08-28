# Runbook: two nodes, one game, to mate

How to play a complete correspondence game across two real `freenet` nodes on
one machine, each acting as one player.

## Status: verified end to end -- peering, propagation, and a full game to mate

**Sections 1-3 (build, starting the nodes) work today**, and section 2b's
peered pair is verified. Section 4 (`init`, `invite`, `game bind`, `move`,
`show`, `watch`) is wired: `cli/src/main.rs` routes every command to
`adjourn_client::session`.

Against a real `freenet 0.2.130` node on 2026-08-24: `adjourn init`
registered the delegate, `adjourn key new` returned a key, and `adjourn
invite accept` bound a game that `adjourn game list` then showed -- see
`CLAUDE.md`, "Runtime assumptions, verified", for what that confirms about
`MessageOrigin` and about the delegate and contract running under wasmtime.

**On 2026-08-27, on Arch (Omarchy), two PEERED `freenet network` nodes on one
machine carried moves between them** -- the cross-peer path this file
previously said was out of scope. `e2e4` reached the second node (entailed:
its `e7e5` could not have been signed otherwise), and `f1c4` reached it via
`adjourn watch`, which subscribes. `watch` is therefore now exercised against
a live node, not only against `FakeNode`.

Two things still NOT recorded here, stated plainly so the gap does not get
rounded off:

- **A full game to mate HAS now been played**, on a three-node topology (a
  dedicated gateway holding no player, plus two player peers). Both nodes
  independently reported ply 7, the same FEN, and "White wins -- checkmate",
  on the same contract id -- which is also the direct observation that both
  players derived identical `GameParams`. See `CLAUDE.md`, "Runtime
  assumptions, verified".
- **A node that misses an update recovers**, without needing to subscribe --
  verified both for a never-subscribed node and for one killed and restarted
  across the update. See section 2b.
- **The two-node shape (a player doubling as the gateway) is not reliable** --
  reported as failing to complete a game across two attempts. Use the
  three-node shape.

## 0. Prerequisites

- A `freenet` binary on `PATH` (`cargo install freenet`, or build from
  [freenet-core](https://github.com/freenet/freenet-core)). This runbook was
  checked against the CLI surface in `freenet-core`'s `crates/core/src/config.rs`
  and `crates/core/src/bin/freenet.rs` as of 2026-08 — flag names below
  (`--data-dir`, `--config-dir`, `--ws-api-port`, `--network-port`) come from
  reading that source, not from having run the binary; confirm with
  `freenet local --help` if anything below doesn't match.
- This repo checked out, on the `cli` branch, with the workspace building:
  `cargo build --workspace --locked`.
- The contract and delegate WASM built via the pinned scripts — **never** a
  bare `cargo build --release`, which embeds an unshippable, machine-specific
  path (see `CLAUDE.md`, "Reproducible builds"):

  ```bash
  ./scripts/build-contract.sh
  ./scripts/build-delegate.sh
  ```

  This produces:
  - `target/wasm32-unknown-unknown/release/adjourn_contract.wasm`
  - `target/wasm32-unknown-unknown/release/adjourn_delegate.wasm`

## 1. Pick two data directories

One per node, fully isolated — separate contract/delegate storage, separate
config, separate ports:

```bash
export ALICE_DIR="$HOME/.local/share/adjourn-demo/alice"
export BOB_DIR="$HOME/.local/share/adjourn-demo/bob"
mkdir -p "$ALICE_DIR" "$BOB_DIR"
```

(On Windows, use e.g. `%LOCALAPPDATA%\adjourn-demo\alice` and adjust the
commands below to PowerShell as needed; the `freenet` flags are the same.)

## 2. Start the two nodes

**`freenet local` does NOT make the two nodes peer -- use `freenet network`
instead if you want a move to cross.** `freenet local` resolves `mode =
"local"` in the config it generates, which the binary's own `--help` describes
as "local-only mode... no real P2P". Two such nodes are fully isolated
single-node instances, each with its own on-disk contract/delegate/db state.
PUTting the same deterministic contract (same code, same `GameParams`) onto
both gives each an independently-initialized copy, not a shared one: a move
signed against Alice's copy never reaches Bob's. Confirmed by driving the UI
against exactly this pair -- see `CLAUDE.md`, "Runtime assumptions, verified".

The `freenet local` pair below is still worth running: it exercises the
delegate, the contract under wasmtime, key creation, the invite exchange and
each node's own state, which is what every earlier live run here used. It
simply cannot carry a move between nodes.

**For propagation, use section 2b.** Both nodes can still live on one machine
-- you do not need two.

`freenet local` runs a node in local (no real P2P) mode. Each flag below is
namespaced per node so the two can run on one machine without colliding:
`--ws-api-port` for the client API, `--network-port` for the node's own
listener (still bound even in local mode), `--data-dir`/`--config-dir` for
on-disk state.

**Terminal A (Alice, white):**

```bash
freenet local \
  --ws-api-port 7509 \
  --network-port 31337 \
  --data-dir "$ALICE_DIR/data" \
  --config-dir "$ALICE_DIR/config"
```

**Terminal B (Bob, black):**

```bash
freenet local \
  --ws-api-port 7510 \
  --network-port 31338 \
  --data-dir "$BOB_DIR/data" \
  --config-dir "$BOB_DIR/config"
```

Leave both running in the foreground. Each node's WebSocket API is now at:

- Alice: `ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native`
- Bob: `ws://127.0.0.1:7510/v1/contract/command?encodingProtocol=native`

## 2b. Two nodes that actually peer, on one machine

Verified 2026-08-27 against `freenet 0.2.130` on Arch (Omarchy), branch
`views` at `353b77c`. `--is-gateway` on one side, `--gateway` on the other.
The gateway's `--public-network-address` can be the machine's own LAN address
even when both nodes are local to it.

**Alice, the gateway:**

```bash
freenet network --is-gateway   --public-network-address 192.168.1.121   --public-network-port 31337   --network-port 31337   --ws-api-port 7509   --skip-load-from-network   --disable-auto-update   --data-dir "$ALICE_DIR/data" --config-dir "$ALICE_DIR/config"
```

**Bob, the peer:**

```bash
freenet network   --gateway "192.168.1.121:31337,<GATEWAY_HEX_PUBKEY>"   --network-port 31338   --ws-api-port 7510   --skip-load-from-network   --disable-auto-update   --data-dir "$BOB_DIR/data" --config-dir "$BOB_DIR/config"
```

`--gateway` takes `"ip:port,hex-pubkey"`, where the key is the gateway's
64-character hex X25519 public key. Start the gateway first and take the key
from its startup output or its `--config-dir`; no `freenet` subcommand prints
it on request.

`--skip-load-from-network` keeps the pair from reaching for the public gateway
index -- a gateway always runs isolated under it anyway, and supplying an
explicit `--gateway` entry makes the CLI entries REPLACE the on-disk
`gateways.toml` cache rather than merge with it.

### The three-node shape, which is what actually played a full game

The pair above (one player doubling as the gateway) works and is verified.
But the topology that carried a game to mate uses a **dedicated gateway
holding no player**, plus two player peers dialling it. Prefer it: neither
player needs a public address, and a player is a peer rather than
infrastructure.

```bash
# gateway -- no player, no game, just a rendezvous point
freenet network --is-gateway   --public-network-address 192.168.1.121 --public-network-port 31337   --network-port 31337 --ws-api-port 7508   --skip-load-from-network --disable-auto-update   --data-dir "$GW_DIR/data" --config-dir "$GW_DIR/config"

# player 1
freenet network --gateway "192.168.1.121:31337,<GATEWAY_HEX_PUBKEY>"   --network-port 31338 --ws-api-port 7509   --skip-load-from-network --disable-auto-update   --data-dir "$ALICE_DIR/data" --config-dir "$ALICE_DIR/config"

# player 2
freenet network --gateway "192.168.1.121:31337,<GATEWAY_HEX_PUBKEY>"   --network-port 31339 --ws-api-port 7510   --skip-load-from-network --disable-auto-update   --data-dir "$BOB_DIR/data" --config-dir "$BOB_DIR/config"
```

On this topology both nodes independently reported the same final position and
the same outcome:

```
ply 7, Black to move
fen: r1bqkb1r/pppp1Qpp/2n2n2/4p3/2B1P3/8/PPPP1PPP/RNB1K1NR b KQkq - 0 4
game over: White wins -- checkmate
```

`adjourn game list` on each also showed the **same contract id** and
`last signed ply` 7 for White against 6 for Black -- the correct split for a
7-ply game. The matching contract id is worth pausing on: it is the direct
observation that both players derived byte-identical `GameParams`, the
property whose failure mode is two players sitting on separate contracts with
no error anywhere.

### What is and is not established about a node falling behind

An earlier version of this section told you to use `watch` rather than `show`
because `show` "serves stale local state". **That was wrong as a general
claim and has been removed.** On the three-node topology, a node that had
never subscribed picked up its opponent's move through plain `adjourn show`
within 10 seconds and held it.

What actually happened in the two-node run that produced that advice: Bob sat
at ply 2 through 90s of `show` polling while Alice was at ply 3, and a
subsequent `watch` printed ply 3. Two readings fit, and the data does not
separate them -- the subscribing GET fetched the record on demand, or it
simply arrived on its own in between. The two-node topology was also
independently reported as failing to complete a game across two attempts, so
the likeliest explanation is that the topology was unreliable rather than that
`show` and `watch` differ in this way.

**Resolved: a node that misses an update recovers, without subscribing.**
Two tests on the three-node topology, fresh game:

- A node that had never subscribed picked up its opponent's move via plain
  `adjourn show` within 10 seconds.
- A node killed while its opponent moved, then restarted, reported the new ply
  via `show` within 10 seconds of coming back -- again without subscribing.

So a stuck board is NOT the expected behaviour here, and if you see one,
investigate rather than assuming it will resolve. Both contrary reports -- a
stale `show`, and a `watch` that printed one state and never advanced -- came
from the two-node player-as-gateway shape, which also failed to complete a
game twice. Use the three-node shape and the question does not arise.

### The two facts that cost real time to learn

- **The node does not survive the shell that started it.** `freenet local &`
  or `nohup freenet local & disown` in a terminal that later closes gets
  reaped along with the rest of that shell's process group — Freenet is not
  daemonizing itself, it is just a foreground process. Backgrounding it inside
  a shell you're about to close does not make it persistent. Keep the two
  terminals open (or run each inside `tmux`/`screen`) for the lifetime of this
  runbook.
- **`setsid nohup freenet local ... < /dev/null &` does survive the shell.**
  Plain `nohup freenet local & disown` is not enough — verified against a live
  node on 2026-08-24 (see `CLAUDE.md`, "Runtime assumptions, verified"). The
  three parts matter together: `setsid` detaches the process from the
  terminal's session (not just the process group `disown` removes it from),
  `nohup` blocks `SIGHUP`, and `< /dev/null` keeps a closed stdin from
  delivering EOF/SIGPIPE to a process expecting a foreground terminal. If you
  need each node to keep running after this runbook's terminal closes, use
  this form rather than the shorter `&`/`disown` idiom above.
- **`freenet service` is the real way to keep one running unattended** —
  `freenet service install` registers it as a systemd user service on Linux, a
  launchd agent on macOS, or a tray-supervised Windows service, complete with
  the auto-update supervisor loop. It is built around **one** long-lived node
  identity, though, not a pair of ad-hoc dev instances on custom ports — for
  the two-node setup in this runbook, two persistent terminals (or `tmux`
  panes) are the practical choice; reach for `freenet service` when you want a
  single node to survive logout/reboot on its own, not to daemonize this
  two-player demo.

## 3. Sanity-check both nodes are up

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:7509/v1/contract/command
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:7510/v1/contract/command
```

A response (even a 4xx for a bare GET on a WS-only endpoint) means the port is
listening. If either hangs or refuses, check that node's terminal for a bind
error before continuing.

## 4. Play the game (wired, unverified against a live node -- see Status above)

The exact commands below match the surface in
`docs/superpowers/specs/2026-08-23-adjourn-cli-design.md`. `main.rs` now
routes every one of them to `adjourn_cli::session`.

### 4.1 Register the delegate on each node

```bash
# Terminal A's shell (Alice's node)
adjourn init --node ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native

# Terminal B's shell (Bob's node)
adjourn init --node ws://127.0.0.1:7510/v1/contract/command?encodingProtocol=native
```

`init` is idempotent — safe to re-run.

### 4.2 Alice invites, as White

```bash
adjourn invite new --label alice --side white \
  --node ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native
```

This prints a base58 `Invite` blob. Copy it.

### 4.3 Bob accepts

```bash
adjourn invite accept <INVITE_BLOB_FROM_ALICE> --label bob \
  --node ws://127.0.0.1:7510/v1/contract/command?encodingProtocol=native
```

This PUTs the contract on Bob's node with empty state, binds Bob's delegate to
it, and prints a base58 `GameOffer` blob. Copy it back to Alice.

### 4.4 Alice binds

```bash
adjourn game bind --label alice <OFFER_BLOB_FROM_BOB> \
  --node ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native
```

This recomputes the contract id from Alice's own build and refuses loudly if
it disagrees with what Bob's offer named — that mismatch is exactly what a
different `adjourn-contract` build between the two players would produce, and
it must be loud rather than each player silently sitting on a separate
contract. If it matches, Alice's node PUTs the contract too (if it hasn't
already seen it) and binds.

### 4.5 Play Scholar's Mate

Same alternation the CI test in `cli/tests/full_game.rs` drives against two
`FakeNode`s — here it goes over the wire to two real nodes instead:

```bash
# Alice, ply 1
adjourn move e2e4 --label alice --node ws://127.0.0.1:7509/...

# Bob, ply 2
adjourn move e7e5 --label bob   --node ws://127.0.0.1:7510/...

# Alice, ply 3
adjourn move f1c4 --label alice --node ws://127.0.0.1:7509/...

# Bob, ply 4
adjourn move b8c6 --label bob   --node ws://127.0.0.1:7510/...

# Alice, ply 5
adjourn move d1h5 --label alice --node ws://127.0.0.1:7509/...

# Bob, ply 6
adjourn move g8f6 --label bob   --node ws://127.0.0.1:7510/...

# Alice, ply 7 -- mate
adjourn move h5f7 --label alice --node ws://127.0.0.1:7509/...
```

(`--node ws://127.0.0.1:PORT/...` above is short for the full
`ws://127.0.0.1:PORT/v1/contract/command?encodingProtocol=native` URL used
throughout; spell it out in real use.)

After each move, confirm the other side sees it before playing the next one:

```bash
adjourn show --label bob   --node ws://127.0.0.1:7510/v1/contract/command?encodingProtocol=native
```

should report the ply Alice just played, and vice versa — that is the proof
state actually crossed the network through the contract rather than one side
talking to itself. **As written, with two `freenet local` nodes and no
gateway configuration, it will not**: see the note in section 2. Each
`adjourn show` here reports only the state already on that node's own
storage.

### 4.6 Confirm the result

```bash
adjourn show --label alice --node ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native
adjourn show --label bob   --node ws://127.0.0.1:7510/v1/contract/command?encodingProtocol=native
```

Both should report checkmate, White to win, at ply 7.

## 5. Tear down

`Ctrl-C` both `freenet local` terminals. Nothing here needs `freenet service
uninstall` — these are throwaway `--data-dir`s, not an installed service. To
reset and replay from scratch, just delete them:

```bash
rm -rf "$ALICE_DIR" "$BOB_DIR"
```
