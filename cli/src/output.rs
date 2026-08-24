//! Rendering: turn command results into terminal output, and errors into the
//! right process exit code.
//!
//! Exit codes: `0` success, `1` refusal or precondition failure, `2` usage
//! (clap owns this -- it exits before `run` in `main.rs` is ever called),
//! `3` transport failure. A refusal is not a crash: `Refusal` already has a
//! `Display` impl that reads as a sentence (see `common/src/delegate_api.rs`
//! and CLAUDE.md invariant on `PlyAlreadySigned`), and `session.rs` folds it
//! into the `anyhow::Error` chain via its `refused()` helper, so this module
//! does not re-map `Refusal` itself -- it only decides which exit code an
//! error chain earns and prints it as a plain sentence, never a Rust
//! backtrace.

use std::process::ExitCode;

use adjourn_core::delegate_api::{EntropyQuality, GameSummary};
use adjourn_core::{Decision, Reason, Status};
use freenet_stdlib::prelude::ContractInstanceId;

use adjourn_cli::invite::{GameOffer, Invite};

pub const EXIT_OK: u8 = 0;
pub const EXIT_REFUSAL: u8 = 1;
pub const EXIT_TRANSPORT: u8 = 3;

/// Print an error as a plain sentence (never a Rust backtrace) and return the
/// exit code the caller's process should use.
///
/// `{:#}` walks the whole `anyhow` chain (each `.context(...)` adds a clause)
/// rather than just the top frame, so "UPDATE contract" style context is
/// visible alongside the underlying cause.
pub fn report_error(err: &anyhow::Error) -> ExitCode {
    eprintln!("error: {err:#}");
    ExitCode::from(if is_transport_failure(err) {
        EXIT_TRANSPORT
    } else {
        EXIT_REFUSAL
    })
}

/// True when `err`'s cause chain contains an error type that only originates
/// in the node-communication layer itself -- a dropped websocket, a
/// malformed frame, the underlying connection -- rather than a request the
/// node or delegate understood and declined.
///
/// A delegate `Refusal` and every `session.rs` precondition (`bail!("it is
/// not your turn")` and friends) are plain `anyhow` strings with none of
/// these types anywhere in their chain, so they fall through to
/// `EXIT_REFUSAL`, matching the "refusal or precondition failure" exit-code
/// bucket. `std::io::Error` is deliberately excluded: a missing WASM file
/// (read straight off local disk before any connection is attempted) is a
/// precondition failure, not evidence the node's transport misbehaved, and
/// an `io::Error` reaching the node layer at all arrives already wrapped in
/// `tokio_tungstenite::tungstenite::Error`, which this function does catch.
///
/// A `RESPONSE_TIMEOUT` elapsing (`cli/src/node.rs::recv_timeout`) belongs in
/// this bucket too: a node that accepted the connection but never answers is
/// exactly the same class of failure as one that dropped the socket outright
/// -- the node/delegate never got far enough to decline the request. That
/// error is a plain `anyhow!(...)` string, so it is matched by message rather
/// than by type.
fn is_transport_failure(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.is::<freenet_stdlib::client_api::Error>()
            || cause.is::<freenet_stdlib::client_api::ClientError>()
            || cause.is::<tokio_tungstenite::tungstenite::Error>()
            || cause.to_string().starts_with("timed out after ")
    })
}

/// Render a [`Status`] for `label`.
pub fn render_status(label: &str, status: &Status) {
    println!("{label} -- ply {}, {:?} to move", status.ply, status.turn);
    println!("fen: {}", status.fen);
    if let Some(Decision { winner, reason }) = &status.decision {
        let outcome = match winner {
            Some(color) => format!("{color:?} wins"),
            None => "draw".to_string(),
        };
        println!("game over: {outcome} -- {}", render_reason(*reason));
    }
    if status.repetitions >= 3 {
        println!(
            "position repeated {} times (claimable at 3, automatic at 5)",
            status.repetitions
        );
    }
    if status.halfmove_clock >= 100 {
        println!(
            "{} halfmoves since a capture or pawn move (claimable at 100, automatic at 150)",
            status.halfmove_clock
        );
    }
    if status.ignored > 0 {
        println!(
            "{} record(s) in state are ignored by the projection (illegal, wrong-parent, or stale)",
            status.ignored
        );
    }
}

fn render_reason(reason: Reason) -> &'static str {
    match reason {
        Reason::Checkmate => "checkmate",
        Reason::Stalemate => "stalemate",
        Reason::InsufficientMaterial => "insufficient material",
        Reason::AutomaticDraw => "automatic draw (fivefold repetition or the 75-move rule)",
        Reason::Resignation => "resignation",
        Reason::DrawAgreement => "draw agreement",
        Reason::DoubleSignForfeit => "double-sign forfeit",
        Reason::MutualResignation => "mutual resignation",
    }
}

/// Print an [`Invite`] blob for the user to copy to their opponent.
pub fn render_invite(invite: &Invite) {
    println!("{}", invite.encode());
}

/// Print a [`GameOffer`] blob for the user to copy back to the inviter.
pub fn render_offer(offer: &GameOffer) {
    println!("{}", offer.encode());
}

/// `adjourn key list`: every key this delegate holds, bound or not.
pub fn render_key_list(games: &[GameSummary]) {
    if games.is_empty() {
        println!("no keys yet -- create one with `adjourn key new --label L`");
        return;
    }
    for g in games {
        let bound = if g.game_id.is_some() {
            "bound"
        } else {
            "unbound"
        };
        let entropy = match g.entropy {
            Some(EntropyQuality::HostBacked) => "host-backed entropy",
            Some(EntropyQuality::Degraded) => "DEGRADED entropy, not securely random",
            None => "entropy unknown",
        };
        println!(
            "{}\t{}\t{bound}\t{entropy}",
            g.label,
            bs58::encode(g.public_key).into_string(),
        );
    }
}

/// `adjourn game list`: only the keys that are actually bound to a game.
pub fn render_game_list(games: &[GameSummary]) {
    let bound: Vec<&GameSummary> = games.iter().filter(|g| g.game_id.is_some()).collect();
    if bound.is_empty() {
        println!("no bound games yet -- bind one with `adjourn game bind`");
        return;
    }
    for g in bound {
        let side = g
            .side
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| "?".to_string());
        let contract = g
            .contract
            .map(|c| ContractInstanceId::new(c).encode())
            .unwrap_or_default();
        let entropy = match g.entropy {
            Some(EntropyQuality::HostBacked) => "host-backed entropy",
            Some(EntropyQuality::Degraded) => "DEGRADED entropy, not securely random",
            None => "entropy unknown",
        };
        println!(
            "{}\t{side}\tcontract {contract}\tlast signed ply {}\t{entropy}",
            g.label, g.last_signed_ply
        );
    }
}
