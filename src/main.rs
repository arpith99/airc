mod client;
mod commands;
mod config;
mod dcc;
mod error;
mod net;
mod tui;

use clap::Parser;
use client::IrcClient;
use config::Config;
use error::Result;
use rand::Rng;
use tokio::sync::mpsc;
use tracing::info;

use crate::tui::{MessageType, UiEvent, UiMakeWriter};

/// Spawn a background task and, if it ends with an error, surface that error in
/// the message pane instead of swallowing it. Returns the join handle so the
/// caller can still await completion (used for the write task during shutdown).
fn spawn_reporting<F>(
    label: &'static str,
    fut: F,
    ui_tx: mpsc::Sender<UiEvent>,
) -> tokio::task::JoinHandle<Result<()>>
where
    F: std::future::Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let result = fut.await;
        if let Err(ref e) = result {
            let _ = ui_tx
                .send(UiEvent::Log(
                    MessageType::Error,
                    format!("{label} task ended: {e}"),
                ))
                .await;
        }
        result
    })
}

// Constants
const DEFAULT_REALNAME: &str = "Book Worm";
const NICKNAME_PREFIX: &str = "bworm";
const MAX_NICKNAME_SUFFIX: u32 = 99999;
const UI_EVENT_BUFFER: usize = 256;

#[derive(Parser, Debug)]
struct Args {
    /// The IRC server to connect to
    #[clap(short, long)]
    server: Option<String>,
    /// The IRC channel to join
    #[clap(short, long)]
    channel: Option<String>,
    /// The username to use
    #[clap(short, long)]
    username: Option<String>,
    /// Download path for DCC files
    #[clap(short, long)]
    download_path: Option<String>,
    /// Connect using TLS/SSL
    #[clap(long)]
    tls: bool,
    /// Port to connect on (defaults to 6667, or 6697 with --tls)
    #[clap(short, long)]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let config = Config::load()
        .await
        .unwrap_or_else(|_| Config::default())
        .merge_with_args(
            args.server,
            args.channel,
            args.username,
            args.download_path,
            args.tls,
            args.port,
        );

    // One channel carries every UI-bound event: logs, RX lines, downloads.
    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>(UI_EVENT_BUFFER);

    // Route tracing logs into the message pane instead of stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .without_time()
        .with_writer(UiMakeWriter::new(ui_tx.clone()))
        .init();

    info!("Starting airc IRC client");

    let (username, nickname, realname) = if let Some(ref user) = config.username {
        (user.clone(), user.clone(), user.clone())
    } else {
        let mut rng = rand::rng();
        let random_nickname = format!(
            "{}{}",
            NICKNAME_PREFIX,
            rng.random_range(0..=MAX_NICKNAME_SUFFIX)
        );
        (
            random_nickname.clone(),
            random_nickname.clone(),
            DEFAULT_REALNAME.to_string(),
        )
    };

    let (client, receiver) =
        IrcClient::new(config, &username, &nickname, &realname, ui_tx.clone()).await?;
    let _ = ui_tx.send(UiEvent::Connected).await;

    let _init = spawn_reporting("init", client::init(client.clone()), ui_tx.clone());
    let write_handle = spawn_reporting("write", client::write(client.clone(), receiver), ui_tx.clone());
    let _receive = spawn_reporting("receive", client::receive_loop(client.clone()), ui_tx.clone());

    tui::run(client, ui_rx, write_handle).await
}
