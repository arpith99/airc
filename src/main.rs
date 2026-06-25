mod client;
mod commands;
mod config;
mod dcc;
mod error;
mod net;
mod tui;
mod ui;

use clap::Parser;
use client::IrcClient;
use config::Config;
use error::Result;
use rand::Rng;
use tracing::{debug, info, warn};
use ui::print_line;

// Constants
const DEFAULT_REALNAME: &str = "Book Worm";
const NICKNAME_PREFIX: &str = "bworm";
const MAX_NICKNAME_SUFFIX: u32 = 99999;

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
    // Initialize tracing/logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Starting airc IRC client");

    let args = Args::parse();

    // Load config from file, then merge with CLI args
    let config = Config::load()
        .await
        .unwrap_or_else(|e| {
            warn!("Failed to load config: {}, using defaults", e);
            Config::default()
        })
        .merge_with_args(
            args.server,
            args.channel,
            args.username,
            args.download_path,
            args.tls,
            args.port,
        );

    debug!("Using config: {:?}", config);

    let (username, nickname, realname) = if let Some(ref user) = config.username {
        (user.clone(), user.clone(), user.clone())
    } else {
        let mut rng = rand::rng();
        let random_nickname = format!(
            "{}{}",
            NICKNAME_PREFIX,
            rng.random_range(0..=MAX_NICKNAME_SUFFIX)
        );
        info!("Generated random nickname: {}", random_nickname);
        (
            random_nickname.clone(),
            random_nickname.clone(),
            DEFAULT_REALNAME.to_string(),
        )
    };

    let (client, receiver) = IrcClient::new(config, &username, &nickname, &realname).await?;

    info!("Spawning async tasks");
    let init_task = tokio::spawn(client::init(client.clone()));
    let write_task = tokio::spawn(client::write(client.clone(), receiver));
    let cli_task = tokio::spawn(client::cli(client.clone()));
    let receive_task = tokio::spawn(client::receive_loop(client.clone()));

    let (init_result, write_result, cli_result, receive_result) =
        tokio::join!(init_task, write_task, cli_task, receive_task);

    // Propagate any errors from the tasks
    init_result??;
    write_result??;
    cli_result??;
    receive_result??;

    // Wait for all DCC tasks to complete before exiting
    info!("Main tasks completed, waiting for DCC transfers");
    print_line("Waiting for DCC transfers to complete...\n", true);
    client::drain_dcc_tasks(&client.dcc_tasks).await;
    info!("All DCC transfers completed, exiting");
    print_line("All DCC transfers completed.\n", true);

    Ok(())
}
