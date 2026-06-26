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

use crate::tui::{UiEvent, UiMakeWriter};

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

    let _init = tokio::spawn(client::init(client.clone()));
    let write_handle = tokio::spawn(client::write(client.clone(), receiver));
    let _receive = tokio::spawn(client::receive_loop(client.clone()));

    tui::run(client, ui_rx, write_handle).await
}
