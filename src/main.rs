use crate::http::HttpServer;
use crate::peers::PeerManager;
use anyhow::Result;
use clap::Parser;
use config::Config;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use log::{debug, error, info, warn};
use nostr_lmdb::NostrLmdb;
use nostr_relay_builder::LocalRelay;
use nostr_relay_builder::builder::{WritePolicy, WritePolicyResult};
use nostr_relay_builder::prelude::{Event, RelayInformationDocument, RelayMessage};
use nostr_sdk::pool::RelayLimits;
use nostr_sdk::prelude::{BoxedFuture, NostrDatabase, SyncProgress};
use nostr_sdk::{
    Alphabet, Client, ClientOptions, Filter, Kind, RelayPoolNotification, SingleLetterTag,
    SubscribeOptions, SyncOptions, TagKind,
};
use serde::Deserialize;
use std::borrow::Cow;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const KINDS: [Kind; 4] = [Kind::Torrent, Kind::TorrentComment, Kind::Metadata, Kind::RelayList];
const KINDS_PLUS_K: [Kind; 2] = [Kind::Comment, Kind::ZapReceipt];

mod http;
mod peers;
#[cfg(test)]
mod tests;

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

    /// Only allow 1 event per info_hash
    pub distinct_info_hash: Option<bool>,

    /// Relay document
    pub relay_document: RelayInformationDocument,
}

#[derive(Debug, Clone)]
struct TorrentPolicy {
    db: Arc<dyn NostrDatabase>,
    distinct_info_hash: bool,
}

impl TorrentPolicy {
    pub fn new(db: Arc<dyn NostrDatabase>, distinct: bool) -> Self {
        Self {
            db,
            distinct_info_hash: distinct,
        }
    }
}

impl WritePolicy for TorrentPolicy {
    fn admit_event<'a>(
        &'a self,
        event: &'a Event,
        _addr: &SocketAddr,
    ) -> BoxedFuture<'a, WritePolicyResult> {
        Box::pin(async move {
            if !KINDS.contains(&event.kind) && !KINDS_PLUS_K.contains(&event.kind) {
                WritePolicyResult::reject("kind not accepted".to_string())
            } else {
                // check for duplicate by info_hash
                if event.kind == Kind::Torrent && self.distinct_info_hash {
                    let info_hash = match event.tags.find(TagKind::Custom(Cow::Borrowed("x"))) {
                        Some(info_hash) if info_hash.content().is_some() => {
                            info_hash.content().unwrap()
                        }
                        _ => return WritePolicyResult::reject("Missing 'x' tag".to_string()),
                    };
                    let f_info_hash = Filter::new()
                        .kind(Kind::Torrent)
                        .custom_tag(SingleLetterTag::from_char('x').unwrap(), info_hash);
                    let existing = match self.db.query(f_info_hash).await {
                        Ok(existing) => existing,
                        Err(_) => {
                            return WritePolicyResult::reject(
                                "Internal server error (DB)".to_string(),
                            );
                        }
                    };
                    // Check if it's the SAME event (same id), not just same info_hash
                    let is_duplicate = existing.iter().any(|e| e.id == event.id);
                    if is_duplicate {
                        // Same event, accept it (might be a re-submission)
                        WritePolicyResult::Accept
                    } else if !existing.is_empty() {
                        // Different event with same info_hash - reject as duplicate torrent
                        WritePolicyResult::reject("duplicate info_hash".to_string())
                    } else {
                        info!("New torrent event: {}", event.id);
                        WritePolicyResult::Accept
                    }
                } else {
                    info!("New {} event: {}", event.kind, event.id);
                    WritePolicyResult::Accept
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

    let db = NostrLmdb::open(
        config
            .database_dir
            .clone()
            .unwrap_or(PathBuf::from("./data")),
    )
    .await?;

    let ndb: Arc<dyn NostrDatabase> = Arc::new(db);

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
    
    // Connect to relays first
    client.connect().await;
    
    // Subscribe to get real-time events (but don't sync yet)
    client
        .pool()
        .subscribe(filter.clone(), SubscribeOptions::default())
        .await?;

    let filter_k = Filter::new().kinds(KINDS_PLUS_K).custom_tag(
        SingleLetterTag::uppercase(Alphabet::K),
        Kind::Torrent.to_string(),
    );
    client
        .pool()
        .subscribe(filter_k.clone().limit(10), SubscribeOptions::default())
        .await?;

    // re-sync with bootstrap relays - this fetches historical events
    let client_sync = client.clone();
    let filter_sync = filter.clone();
    let filter_k_sync = filter_k.clone();
    
    // Track sync state with atomic flags
    use std::sync::atomic::{AtomicBool, Ordering};
    let sync_completed = Arc::new(AtomicBool::new(false));
    let sync_progress_tx = Arc::new(tokio::sync::watch::channel(0).0);
    
    tokio::spawn(async move {
        let (tx, mut rx) = SyncProgress::channel();
        let _sync_completed_clone = sync_completed.clone();
        let sync_progress_tx_clone = sync_progress_tx.clone();
        
        tokio::spawn(async move {
            while let Ok(_) = rx.changed().await {
                let x = rx.borrow();
                info!(
                    "Sync progress: {}/{} ({:.2}%)",
                    x.current,
                    x.total,
                    if x.total > 0 { x.current as f64 / x.total as f64 * 100.0 } else { 0.0 }
                );
                // Update progress channel
                let _ = sync_progress_tx_clone.send(x.current);
            }
        });
        
        info!("Starting sync for filter: {:?}", &filter_sync);
        let opts = SyncOptions::default().progress(tx.clone());
        if let Err(e) = client_sync.sync(filter_sync, &opts).await {
            error!("Sync error (filter): {}", e);
        } else {
            info!("Sync completed for filter");
        }
        
        info!("Starting sync for filter_k: {:?}", &filter_k_sync);
        let opts_k = SyncOptions::default().progress(tx);
        if let Err(e) = client_sync.sync(filter_k_sync, &opts_k).await {
            error!("Sync error (filter_k): {}", e);
        } else {
            info!("Sync completed for filter_k");
        }
        
        // Mark sync as completed
        sync_completed.store(true, Ordering::SeqCst);
        info!("Initial sync completed");
    });

    let policy = TorrentPolicy::new(ndb.clone(), config.distinct_info_hash.unwrap_or(false));
    let addr = config
        .listen
        .clone()
        .unwrap_or(SocketAddr::new([127, 0, 0, 1].into(), DEFAULT_RELAY_PORT));
    let relay = LocalRelay::builder()
        .database(ndb.clone())
        .addr(addr.ip())
        .port(addr.port())
        .write_policy(policy)
        .build();

    let client_notification = client.clone();
    let relay_notify = relay.clone();
    let ndb_clone = ndb.clone();
    tokio::spawn(async move {
        let mut event_count = 0;
        while let Ok(msg) = client_notification.notifications().recv().await {
            match msg {
                RelayPoolNotification::Event { event, .. } => {
                    event_count += 1;
                    // Only save if not already in DB (to avoid race with sync)
                    // Check if event exists before saving
                    if let Err(e) = ndb_clone.save_event(&*event).await {
                        // If it's a duplicate key error, that's fine - event already exists
                        if !e.to_string().contains("duplicate") {
                            error!("Error saving event {}: {}", event.id, e);
                        }
                    }
                    relay_notify.notify_event((*event).clone());
                    if let Err(e) = client_notification.send_event(&*event).await {
                        warn!("Error sending event: {:?}", e);
                    }
                }
                RelayPoolNotification::Message { message, .. } => match message {
                    RelayMessage::Event { .. } => {}
                    RelayMessage::Ok { .. } => {}
                    RelayMessage::EndOfStoredEvents(relay_url) => {
                        info!("End of stored events from {}", relay_url);
                    }
                    RelayMessage::Notice(notice) => {
                        info!("Notice: {}", notice);
                    }
                    RelayMessage::Closed { .. } => {}
                    RelayMessage::Auth { .. } => {}
                    RelayMessage::Count { .. } => {}
                    RelayMessage::NegMsg { .. } => {}
                    RelayMessage::NegErr { .. } => {}
                },
                RelayPoolNotification::Shutdown => {}
            }
        }
        info!("Total events received: {}", event_count);
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
            config.relay_document.clone(),
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
