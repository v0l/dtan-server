use crate::http::HttpServer;
use crate::peers::PeerManager;
use anyhow::Result;
use clap::Parser;
use config::Config;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use log::{debug, error, info};
use nostr_relay_builder::builder::{PolicyResult, WritePolicy};
use nostr_relay_builder::prelude::Event;
use nostr_relay_builder::{LocalRelay, RelayBuilder};
use nostr_sdk::prelude::{BoxedFuture, SyncProgress};
use nostr_sdk::{
    Client, Filter, Kind, NdbDatabase, RelayPoolNotification, SubscribeOptions, SyncOptions,
};
use serde::Deserialize;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

mod http;
mod peers;

pub const DEFAULT_RELAY_PORT: u16 = 7777;

#[derive(Parser, Debug)]
#[command(about, version)]
struct Args {
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Deserialize, Clone)]
pub struct Settings {
    /// Path to store events
    pub database_dir: Option<PathBuf>,

    /// List of relays to bootstrap from
    pub relays: Vec<String>,

    /// Listen addr for relay
    pub relay_listen: Option<SocketAddr>,
}

#[derive(Debug, Clone)]
struct AcceptKinds(HashSet<Kind>);

impl AcceptKinds {
    pub fn from<I>(input: I) -> Self
    where
        I: Into<HashSet<Kind>>,
    {
        Self(input.into())
    }
}

impl WritePolicy for AcceptKinds {
    fn admit_event(&self, event: &Event, _addr: &SocketAddr) -> BoxedFuture<PolicyResult> {
        if !self.0.contains(&event.kind) {
            Box::pin(async move { PolicyResult::Reject("kind not accepted".to_string()) })
        } else {
            Box::pin(async move { PolicyResult::Accept })
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init();

    let args = Args::parse();

    let config: Settings = Config::builder()
        .add_source(config::File::from(
            args.config.unwrap_or(PathBuf::from("./config.yml")),
        ))
        .build()?
        .try_deserialize()?;

    let db = NdbDatabase::open(
        config
            .database_dir
            .clone()
            .unwrap_or(PathBuf::from("./data"))
            .to_str()
            .unwrap(),
    )?;

    const KINDS: [Kind; 2] = [Kind::Torrent, Kind::TorrentComment];

    let client = Client::builder().database(db.clone()).build();

    for relay in &config.relays {
        info!("Connecting to {}", relay);
        client.add_relay(relay).await?;
    }

    let filter = Filter::new().kinds(KINDS);
    client.connect().await;
    client
        .pool()
        .subscribe(filter.clone(), SubscribeOptions::default())
        .await?;

    // re-sync with bootstrap relays
    let client_sync = client.clone();
    tokio::spawn(async move {
        let (tx, mut rx) = SyncProgress::channel();
        tokio::spawn(async move {
            while let Ok(_) = rx.changed().await {
                let x = rx.borrow();
                info!(
                    "Sync progress: {}/{} ({:.2}%)",
                    x.current,
                    x.total,
                    x.current as f64 / x.total as f64 * 100.0
                );
            }
        });
        info!("Starting sync: {:?}", &filter);
        let opts = SyncOptions::default().progress(tx);
        if let Err(e) = client_sync.sync(filter, &opts).await {
            error!("{}", e);
        }
    });

    let policy = AcceptKinds::from(KINDS);
    let addr = config
        .relay_listen
        .clone()
        .unwrap_or(SocketAddr::new([127, 0, 0, 1].into(), DEFAULT_RELAY_PORT));
    let relay = RelayBuilder::default()
        .database(db.clone())
        .addr(addr.ip())
        .port(addr.port())
        .write_policy(policy);

    let relay = LocalRelay::new(relay).await?;

    let client_notification = client.clone();
    let relay_notify = relay.clone();
    tokio::spawn(async move {
        while let Ok(msg) = client_notification.notifications().recv().await {
            match msg {
                RelayPoolNotification::Event { event, .. } => {
                    relay_notify.notify_event(*event);
                }
                _ => {}
            }
        }
    });

    // spawn peer manager
    let mut peer_manager = PeerManager::new(client.clone(), config.clone());
    let _: JoinHandle<Result<()>> = tokio::spawn(async move {
        peer_manager.start().await?;

        loop {
            if let Err(e) = peer_manager.tick().await {
                error!("Peer manager error: {}", e);
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
        Ok(())
    });

    // http server
    let listener = TcpListener::bind(&addr).await?;
    info!("Listening on: {}", addr);
    loop {
        let (socket, addr) = listener.accept().await?;
        debug!("New connection from {}", addr);
        let io = TokioIo::new(socket);
        let server = HttpServer::new(relay.clone(), addr, PathBuf::from(""));
        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, server)
                .with_upgrades()
                .await
            {
                error!("Failed to handle request: {}", e);
            }
        });
    }
}
