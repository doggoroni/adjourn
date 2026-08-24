//! `adjourn`: the command-line client.
//!
//! Thin glue only -- every flow that touches the delegate or the contract
//! lives in `adjourn_cli::session` and is exercised there against
//! `FakeNode`. This file's job is: parse arguments, load the two WASM
//! modules off disk, open one `WsClient`, dispatch to `session`, and render
//! the result -- never re-implement a flow `session.rs` already owns.
//!
//! Exit codes (see `output.rs`): `0` success, `1` refusal or precondition
//! failure, `2` usage -- handled entirely by `clap`, which exits before
//! `run` below is ever reached -- `3` transport failure.

mod output;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use adjourn_cli::invite::{GameOffer, Invite};
use adjourn_cli::node::{delegate_container, NodeClient, WsClient};
use adjourn_cli::session;
use adjourn_core::delegate_api::{Request, Response, Side};
use anyhow::{bail, Context};
use clap::{Parser, Subcommand, ValueEnum};
use rand::RngCore;

const DEFAULT_NODE: &str = "ws://127.0.0.1:7509/v1/contract/command?encodingProtocol=native";
const DEFAULT_CONTRACT_WASM: &str = "target/wasm32-unknown-unknown/release/adjourn_contract.wasm";
const DEFAULT_DELEGATE_WASM: &str = "target/wasm32-unknown-unknown/release/adjourn_delegate.wasm";

#[derive(Parser)]
#[command(
    name = "adjourn",
    version,
    about = "Untimed correspondence chess over Freenet"
)]
struct Cli {
    /// Freenet node websocket URL.
    #[arg(long, global = true, default_value = DEFAULT_NODE)]
    node: String,
    /// Path to the compiled adjourn-contract WASM, built via
    /// `scripts/build-contract.sh`.
    #[arg(long, global = true, default_value = DEFAULT_CONTRACT_WASM)]
    contract_wasm: PathBuf,
    /// Path to the compiled adjourn-delegate WASM, built via
    /// `scripts/build-delegate.sh`.
    #[arg(long, global = true, default_value = DEFAULT_DELEGATE_WASM)]
    delegate_wasm: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register the delegate on the node. Idempotent -- safe to re-run.
    Init,
    #[command(subcommand)]
    Key(KeyCommand),
    #[command(subcommand)]
    Invite(InviteCommand),
    #[command(subcommand)]
    Game(GameCommand),
    /// Show the current status of a bound game.
    Show {
        #[arg(long)]
        label: String,
    },
    /// Play a move, e.g. `e2e4`.
    Move {
        uci: String,
        #[arg(long)]
        label: String,
    },
    /// Resign a game.
    Resign {
        #[arg(long)]
        label: String,
    },
    #[command(subcommand)]
    Draw(DrawCommand),
    /// Hold a subscription open and stream updates. Not implemented yet.
    Watch {
        #[arg(long)]
        label: String,
    },
}

#[derive(Subcommand)]
enum KeyCommand {
    /// Create a new signing key under `label`.
    New {
        #[arg(long)]
        label: String,
    },
    /// List every key this delegate holds.
    List,
}

#[derive(Subcommand)]
enum InviteCommand {
    /// Create an invite for a new game, playing `side`.
    New {
        #[arg(long)]
        label: String,
        #[arg(long)]
        side: SideArg,
    },
    /// Accept an invite, generating our own key and PUTting the contract.
    Accept {
        invite: String,
        #[arg(long)]
        label: String,
    },
}

#[derive(Subcommand)]
enum GameCommand {
    /// Bind the inviting side to the offer the accepter sent back.
    Bind {
        #[arg(long)]
        label: String,
        offer: String,
    },
    /// List every game this delegate has bound.
    List,
}

#[derive(Subcommand)]
enum DrawCommand {
    /// Offer a draw, anchored to the current head.
    Offer {
        #[arg(long)]
        label: String,
    },
    /// Accept the opponent's live draw offer.
    Accept {
        #[arg(long)]
        label: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
#[value(rename_all = "lower")]
enum SideArg {
    White,
    Black,
}

impl From<SideArg> for Side {
    fn from(s: SideArg) -> Self {
        match s {
            SideArg::White => Side::White,
            SideArg::Black => Side::Black,
        }
    }
}

fn read_wasm(path: &Path, what: &str, script: &str) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path).with_context(|| {
        format!(
            "reading {what} at {}; build it first with `{script}`",
            path.display()
        )
    })
}

/// Off-wasm, the host RNG `adjourn_delegate` would otherwise use is a dead
/// stub -- caller entropy is the only real randomness available here. `None`
/// gets refused by the delegate (`Refusal::NoEntropy`), and a constant would
/// make every key this CLI generates identical.
fn random_entropy() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

async fn connect(node_url: &str, delegate_wasm: &Path) -> anyhow::Result<WsClient> {
    let wasm = read_wasm(
        delegate_wasm,
        "the delegate WASM",
        "scripts/build-delegate.sh",
    )?;
    let (_container, key) = delegate_container(wasm);
    WsClient::connect(node_url, key).await
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init => {
            let wasm = read_wasm(
                &cli.delegate_wasm,
                "the delegate WASM",
                "scripts/build-delegate.sh",
            )?;
            let (container, key) = delegate_container(wasm);
            let mut node = WsClient::connect(&cli.node, key.clone()).await?;
            node.register_delegate(container).await?;
            println!("delegate registered: {key}");
        }

        Command::Key(KeyCommand::New { label }) => {
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let response = node
                .delegate(Request::CreateGameKey {
                    label: label.clone(),
                    caller_entropy: Some(random_entropy()),
                })
                .await
                .context("CreateGameKey")?;
            match response {
                Response::GameKey {
                    public_key,
                    entropy,
                    ..
                } => {
                    println!("{label}: {}", bs58::encode(public_key).into_string());
                    if matches!(
                        entropy,
                        adjourn_core::delegate_api::EntropyQuality::Degraded
                    ) {
                        eprintln!(
                            "warning: this key was generated with degraded entropy -- it is not securely random"
                        );
                    }
                }
                Response::Refused(r) => bail!("delegate refused: {r}"),
                other => bail!("unexpected response to CreateGameKey: {other:?}"),
            }
        }

        Command::Key(KeyCommand::List) => {
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let games = list_games(&mut node).await?;
            output::render_key_list(&games);
        }

        Command::Invite(InviteCommand::New { label, side }) => {
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let invite = session::invite_new(&mut node, &label, side.into()).await?;
            output::render_invite(&invite);
        }

        Command::Invite(InviteCommand::Accept { invite, label }) => {
            let contract_wasm = read_wasm(
                &cli.contract_wasm,
                "the contract WASM",
                "scripts/build-contract.sh",
            )?;
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let invite = Invite::decode(&invite).context("decoding invite")?;
            let offer = session::invite_accept(&mut node, &label, &invite, contract_wasm).await?;
            output::render_offer(&offer);
        }

        Command::Game(GameCommand::Bind { label, offer }) => {
            let contract_wasm = read_wasm(
                &cli.contract_wasm,
                "the contract WASM",
                "scripts/build-contract.sh",
            )?;
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let offer = GameOffer::decode(&offer).context("decoding offer")?;
            let id = session::game_bind(&mut node, &label, &offer, contract_wasm).await?;
            println!("{label} bound to contract {}", id.encode());
        }

        Command::Game(GameCommand::List) => {
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let games = list_games(&mut node).await?;
            output::render_game_list(&games);
        }

        Command::Show { label } => {
            let contract_wasm = read_wasm(
                &cli.contract_wasm,
                "the contract WASM",
                "scripts/build-contract.sh",
            )?;
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let status = session::show_label(&mut node, &label, contract_wasm).await?;
            output::render_status(&label, &status);
        }

        Command::Move { uci, label } => {
            let contract_wasm = read_wasm(
                &cli.contract_wasm,
                "the contract WASM",
                "scripts/build-contract.sh",
            )?;
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let status = session::play_move(&mut node, &label, &uci, contract_wasm).await?;
            output::render_status(&label, &status);
        }

        Command::Resign { label } => {
            let contract_wasm = read_wasm(
                &cli.contract_wasm,
                "the contract WASM",
                "scripts/build-contract.sh",
            )?;
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let status = session::resign(&mut node, &label, contract_wasm).await?;
            output::render_status(&label, &status);
        }

        Command::Draw(DrawCommand::Offer { label }) => {
            let contract_wasm = read_wasm(
                &cli.contract_wasm,
                "the contract WASM",
                "scripts/build-contract.sh",
            )?;
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let status = session::draw_offer(&mut node, &label, contract_wasm).await?;
            output::render_status(&label, &status);
        }

        Command::Draw(DrawCommand::Accept { label }) => {
            let contract_wasm = read_wasm(
                &cli.contract_wasm,
                "the contract WASM",
                "scripts/build-contract.sh",
            )?;
            let mut node = connect(&cli.node, &cli.delegate_wasm).await?;
            let status = session::draw_accept(&mut node, &label, contract_wasm).await?;
            output::render_status(&label, &status);
        }

        Command::Watch { label: _ } => {
            bail!("watch: not implemented yet; poll with `adjourn show`");
        }
    }
    Ok(())
}

/// `ListGames` straight through the delegate -- there is no `session.rs`
/// wrapper for it (it backs both `key list` and `game list`, which render it
/// differently), so this is the one request this file sends directly rather
/// than through `session`.
async fn list_games<N: NodeClient>(
    node: &mut N,
) -> anyhow::Result<Vec<adjourn_core::delegate_api::GameSummary>> {
    let response = node
        .delegate(Request::ListGames)
        .await
        .context("ListGames")?;
    match response {
        Response::Games(games) => Ok(games),
        Response::Refused(r) => bail!("delegate refused: {r}"),
        other => bail!("unexpected response to ListGames: {other:?}"),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::from(output::EXIT_OK),
        Err(e) => output::report_error(&e),
    }
}
