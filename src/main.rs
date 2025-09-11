use crate::http::HttpServer;
use crate::peers::PeerManager;
use anyhow::Result;
use clap::Parser;
use config::Config;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use log::{debug, error, info};
use nostr_relay_builder::builder::{PolicyResult, WritePolicy};
use nostr_relay_builder::prelude::{Event, RelayMessage};
use nostr_relay_builder::{LocalRelay, RelayBuilder};
use nostr_sdk::pool::RelayLimits;
use nostr_sdk::prelude::{BoxedFuture, NostrDatabase, SyncProgress};
use nostr_sdk::{
    Client, ClientOptions, Filter, Kind, NdbDatabase, RelayPoolNotification, SingleLetterTag,
    SubscribeOptions, SyncOptions, TagKind,
};
use serde::Deserialize;
use std::borrow::Cow;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const KINDS: [Kind; 4] = [
    Kind::Torrent,
    Kind::TorrentComment,
    Kind::Comment,
    Kind::ZapReceipt,
];

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
    pub listen: Option<SocketAddr>,

    /// Explicit announcement address
    pub dht_public_ip: Option<Ipv4Addr>,

    /// Path to UI
    pub web_dir: Option<PathBuf>,

    /// Enable DHT system for peer connections
    pub use_dht: Option<bool>,
}

#[derive(Debug, Clone)]
struct TorrentPolicy {
    db: NdbDatabase,
}

impl TorrentPolicy {
    pub fn new(db: NdbDatabase) -> Self {
        Self { db }
    }
}

impl WritePolicy for TorrentPolicy {
    fn admit_event<'a>(
        &'a self,
        event: &'a Event,
        _addr: &SocketAddr,
    ) -> BoxedFuture<'a, PolicyResult> {
        Box::pin(async move {
            if !KINDS.contains(&event.kind) {
                PolicyResult::Reject("kind not accepted".to_string())
            } else {
                // check for duplicate by info_hash
                if event.kind == Kind::Torrent {
                    let info_hash = match event.tags.find(TagKind::Custom(Cow::Borrowed("x"))) {
                        Some(info_hash) if info_hash.content().is_some() => {
                            info_hash.content().unwrap()
                        }
                        _ => return PolicyResult::Reject("Missing 'x' tag".to_string()),
                    };
                    let f_info_hash = Filter::new()
                        .kind(Kind::Torrent)
                        .custom_tag(SingleLetterTag::from_char('x').unwrap(), info_hash);
                    let exists = match self.db.query(f_info_hash).await {
                        Ok(exists) => !exists.is_empty(), // TODO: actually check if the event is the same info_hash
                        Err(_) => {
                            return PolicyResult::Reject("Internal server error (DB)".to_string());
                        }
                    };
                    if !exists {
                        info!("New torrent event: {}", event.id);
                        PolicyResult::Accept
                    } else {
                        PolicyResult::Reject("duplicate info_hash".to_string())
                    }
                } else {
                    info!("New {} event: {}", event.kind, event.id);
                    PolicyResult::Accept
                }
            }
        })
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

    let ndb = NdbDatabase::open(
        config
            .database_dir
            .clone()
            .unwrap_or(PathBuf::from("./data"))
            .to_str()
            .unwrap(),
    )?;

    // loop until NDB ready
    loop {
        if ndb.query(Filter::new().limit(1)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        info!("Waiting for NDB query...");
    }

    let mut relay_limits = RelayLimits::default();
    relay_limits
        .events
        .max_num_tags_per_kind
        .insert(Kind::Torrent, Some(10_000)); // allow torrents to have 10k tags

    let client = Client::builder()
        .opts(ClientOptions::new().relay_limits(relay_limits))
        .database(ndb.clone())
        .build();

    for relay in &config.relays {
        info!("Connecting to {}", relay);
        client.add_relay(relay).await?;
    }

    let filter = Filter::new().kinds(KINDS);
    client.connect().await;
    client
        .pool()
        .subscribe(filter.clone().limit(10), SubscribeOptions::default())
        .await?;

    // re-sync with bootstrap relays
    let client_sync = client.clone();
    let filter_sync = filter.clone();
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
        info!("Starting sync: {:?}", &filter_sync);
        let opts = SyncOptions::default().progress(tx);
        if let Err(e) = client_sync.sync(filter_sync, &opts).await {
            error!("{}", e);
        }
    });

    let policy = TorrentPolicy::new(ndb.clone());
    let addr = config
        .listen
        .clone()
        .unwrap_or(SocketAddr::new([127, 0, 0, 1].into(), DEFAULT_RELAY_PORT));
    let relay = RelayBuilder::default()
        .database(ndb.clone())
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
                RelayPoolNotification::Message { message, .. } => match message {
                    RelayMessage::Event { .. } => {}
                    RelayMessage::Ok { .. } => {}
                    RelayMessage::EndOfStoredEvents(_) => {}
                    RelayMessage::Notice(_) => {}
                    RelayMessage::Closed { .. } => {}
                    RelayMessage::Auth { .. } => {}
                    RelayMessage::Count { .. } => {}
                    RelayMessage::NegMsg { .. } => {}
                    RelayMessage::NegErr { .. } => {}
                },
                RelayPoolNotification::Shutdown => {}
            }
        }
    });

    // spawn peer manager
    if config.use_dht.unwrap_or(true) {
        let mut peer_manager = PeerManager::new(filter.clone(), client.clone(), config.clone())?;
        let _: JoinHandle<Result<()>> = tokio::spawn(async move {
            peer_manager.start().await?;

            loop {
                if let Err(e) = peer_manager.tick().await {
                    error!("Peer manager error: {}", e);
                    break;
                }
                tokio::time::sleep(Duration::from_secs(60 * 5)).await;
            }
            Ok(())
        });
    }

    // http server
    let listener = TcpListener::bind(&addr).await?;
    info!("Listening on: {}", addr);
    loop {
        let (socket, addr) = listener.accept().await?;
        debug!("New connection from {}", addr);
        let io = TokioIo::new(socket);
        let server = HttpServer::new(
            relay.clone(),
            addr,
            config.web_dir.clone().unwrap_or(PathBuf::from("www")),
        );
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
