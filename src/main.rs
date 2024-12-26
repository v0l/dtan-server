use anyhow::Result;
use clap::Parser;
use config::Config;
use nostr_relay_builder::builder::{PolicyResult, WritePolicy};
use nostr_relay_builder::prelude::Event;
use nostr_relay_builder::{LocalRelay, RelayBuilder};
use nostr_sdk::prelude::async_trait;
use nostr_sdk::{Client, NostrLMDB};
use serde::Deserialize;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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

#[derive(Debug)]
struct AcceptKinds(HashSet<u16>);

impl AcceptKinds {
    pub fn from<I>(input: I) -> Self
    where
        I: Into<HashSet<u16>>,
    {
        Self(input.into())
    }
}

#[async_trait]
impl WritePolicy for AcceptKinds {
    async fn admit_event(&self, event: &Event, _addr: &SocketAddr) -> PolicyResult {
        if !self.0.contains(&event.kind.as_u16()) {
            PolicyResult::Reject("kind not accepted".to_string())
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

    let db = NostrLMDB::open(config.database_dir.unwrap_or(PathBuf::from("./data")))?;
    let db = Arc::new(db);

    let client = Client::builder().database(db.clone()).build();
    for relay in &config.relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;

    let addr = config
        .relay_listen
        .unwrap_or(SocketAddr::new([0, 0, 0, 0].into(), 8000));
    let relay = RelayBuilder::default()
        .database(db.clone())
        .addr(addr.ip())
        .port(addr.port())
        .write_policy(AcceptKinds::from([0, 2003, 2004]));

    LocalRelay::run(relay).await?;

    // start scraper
    Ok(())
}
