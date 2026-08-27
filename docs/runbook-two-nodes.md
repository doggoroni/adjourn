# Runbook: two nodes, one game, to mate

How to play a complete correspondence game across two real `freenet` nodes on
one machine, each acting as one player.

## Status: wired, partially verified against a live node

**Sections 1–3 (build, and starting the two nodes) work today.** Section 4
(the `adjourn` game commands: `init`, `invite`, `game bind`, `move`, `show`,
...) is now wired: `cli/src/main.rs` routes every command below to
`adjourn_client::session`. `watch` is now implemented too: it subscribes to
the contract and streams every update until the game ends. It has been
exercised against `FakeNode`, not yet against a live node — poll with
`adjourn show` if it misbehaves.

Against a real `freenet 0.2.130` node on 2026-08-24: `adjourn init`
registered the delegate, `adjourn key new` returned a key, and `adjourn
invite accept` successfully bound a game that `adjourn game list` then
showed — see `CLAUDE.md`, "Runtime assumptions, verified", for what that
confirms about `MessageOrigin` and about the delegate/contract running under
wasmtime. Playing a full game to mate through this runbook has not yet been
recorded here; do that next and update this section with the result.

This host (Windows, no linker for `dlltool.exe`, no local Freenet node)
cannot itself run any of this — Task 9 here was verified with `cargo
check`/`cargo clippy -p adjourn-cli --all-targets` only, plus the `FakeNode`
test suite, which runs the real contract and delegate code but never touches
a WebSocket or a real `freenet` process. The live-node confirmation above was
run and reported separately, on Linux.

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

**This runbook, as written, does NOT make the two nodes peer.** `freenet
local` resolves `mode = "local"` in the config it generates — the binary's
own `--help` describes this as "local-only mode... no real P2P" — so the two
node processes below are fully isolated single-node instances, each with its
own on-disk contract/delegate/db state, with no gateway or peer relationship
between them. PUTting the same deterministic contract (same code, same
`GameParams`) onto both gives each an independently-initialized copy, not a
shared, synchronized one: a move signed against Alice's copy never reaches
Bob's. This was confirmed while driving the UI against exactly this setup —
see `CLAUDE.md`'s "Runtime assumptions, verified" — where playing a move
advanced the acting node's own ply correctly while the other node's storage,
checked directly, stayed unmoved. Section 4.5's "confirm the other side sees
it" step below is therefore **not achievable with the setup as written**; it
would need real peering (`freenet network --is-gateway ...` on one side,
`freenet network --gateway "host:port,pubkey" ...` on the other), which is
out of scope for this runbook.

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
