use anyhow::Result;
use clap::Parser;
use config::Config;
use log::error;
use nostr_relay_builder::builder::{PolicyResult, QueryPolicy, WritePolicy};
use nostr_relay_builder::prelude::Event;
use nostr_relay_builder::{LocalRelay, RelayBuilder};
use nostr_sdk::prelude::async_trait;
use nostr_sdk::{Client, Filter, Kind, NdbDatabase, Options, SubscribeOptions, SyncOptions};
use serde::Deserialize;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(about, version)]
struct Args {
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Deserialize)]
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

#[async_trait]
impl WritePolicy for AcceptKinds {
    async fn admit_event(&self, event: &Event, _addr: &SocketAddr) -> PolicyResult {
        if !self.0.contains(&event.kind) {
            PolicyResult::Reject("kind not accepted".to_string())
        } else {
            PolicyResult::Accept
        }
    }
}

#[async_trait]
impl QueryPolicy for AcceptKinds {
    async fn admit_query(&self, query: &[Filter], _addr: &SocketAddr) -> PolicyResult {
        if !query.iter().all(|q| {
            q.kinds
                .as_ref()
                .map(|k| k.iter().all(|v| self.0.contains(v)))
                .unwrap_or(false)
        }) {
            PolicyResult::Reject("kind not supported".to_string())
        } else {
            PolicyResult::Accept
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
            .unwrap_or(PathBuf::from("./data"))
            .to_str()
            .unwrap(),
    )?;

    const KINDS: [Kind; 4] = [
        Kind::Metadata,
        Kind::RelayList,
        Kind::Torrent,
        Kind::TorrentComment,
    ];

    let client = Client::builder()
        .database(db.clone())
        .opts(Options::default())
        .build();

    for relay in &config.relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;
    client
        .pool()
        .subscribe(
            vec![Filter::new().kinds(KINDS.to_vec())],
            SubscribeOptions::default().close_on(None),
        )
        .await?;

    // re-sync with bootstrap relays
    let client_sync = client.clone();
    let sync_kinds = KINDS[1..].to_vec();
    tokio::spawn(async move {
        if let Err(e) = client_sync
            .sync(Filter::new().kinds(sync_kinds), &SyncOptions::default())
            .await
        {
            error!("{}", e);
        }
    });

    let policy = AcceptKinds::from(KINDS);
    let addr = config
        .relay_listen
        .unwrap_or(SocketAddr::new([127, 0, 0, 1].into(), 8000));
    let relay = RelayBuilder::default()
        .database(db.clone())
        .addr(addr.ip())
        .port(addr.port())
        .write_policy(policy.clone())
        .query_policy(policy);

    let relay = LocalRelay::run(relay).await?;

    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?
        .recv()
        .await
        .expect("failed to listen to shutdown signal");

    relay.shutdown();

    Ok(())
}
