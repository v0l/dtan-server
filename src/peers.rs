use crate::{DEFAULT_RELAY_PORT, Settings};
use anyhow::Result;
use log::{debug, error, info, warn};
use mainline::{Dht, Id};
use nostr_sdk::{Client, Filter, RelayUrl, SyncOptions};
use sha1::Digest;
use std::collections::HashSet;
use std::net::SocketAddrV4;
use std::num::NonZeroU16;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Manage connected relays automatically via relay discovery and DHT
pub struct PeerManager {
    /// The main filter
    filter: Filter,
    /// Nostr client to add/remove relay connections
    client: Client,
    /// DHT peer discovery
    dht: Dht,
    /// Application settings
    settings: Settings,
    /// NAT-PMP / uPnP port mapping service
    portmapper: portmapper::Client,
    /// DHT info hash for peer discovery
    info_hash: Id,
    /// External port used for DHT announcements
    external_port: Arc<RwLock<Option<u16>>>,
}

impl PeerManager {
    pub fn new(filter: Filter, client: Client, settings: Settings) -> Result<Self> {
        let digest: [u8; 20] = sha1::Sha1::digest("nostr:2003").as_slice().try_into()?;

        let mut pm_config = portmapper::Config::default();
        pm_config.protocol = portmapper::Protocol::Tcp;

        let mut dht = Dht::builder();
        if let Some(a) = &settings.dht_public_ip {
            dht.public_ip(*a);
        }

        Ok(Self {
            filter,
            client,
            settings,
            dht: dht.build()?,
            portmapper: portmapper::Client::new(pm_config),
            info_hash: Id::from(digest),
            external_port: Arc::new(RwLock::new(None)),
        })
    }

    /// Start peer manager by creating a port mapping and advertising it to DHT
    pub async fn start(&self) -> Result<()> {
        info!("Starting peer manager...");

        // probe services
        let has_portmapper = self.settings.dht_public_ip.is_none()
            && match self.portmapper.probe().await? {
                Ok(probe) => {
                    debug!("Probe result: {:?}", probe);
                    probe.nat_pmp || probe.upnp || probe.pcp
                }
                Err(e) => {
                    error!("Probe error: {:?}", e);
                    false
                }
            };

        if has_portmapper {
            let local_port = self
                .settings
                .listen
                .map(|a| a.port())
                .unwrap_or(DEFAULT_RELAY_PORT);
            debug!("Probe successful, starting portmapper...");
            self.portmapper
                .update_local_port(NonZeroU16::new(local_port).unwrap());

            // spawn listener for external port mapping
            let mut addr_rx = self.portmapper.watch_external_address();
            let external_port_clone = self.external_port.clone();
            tokio::spawn(async move {
                while let Ok(_) = addr_rx.changed().await {
                    let ext_addr = addr_rx.borrow().clone();
                    if let Some(external_address) = ext_addr {
                        info!("Port mapping added for: {}", external_address);

                        external_port_clone
                            .write()
                            .await
                            .replace(external_address.port());
                    }
                }
            });

            // TODO: timeout if no port mapping and fallback to announce local_port

            self.portmapper.procure_mapping();
        }
        Ok(())
    }

    async fn announce_peer(&self) -> Result<()> {
        let local_port = self
            .settings
            .listen
            .map(|a| a.port())
            .unwrap_or(DEFAULT_RELAY_PORT);
        let external_port = self.external_port.read().await;
        let announce_port = external_port.unwrap_or(local_port);

        info!("Announcing port {} for {}", announce_port, self.info_hash);
        if let Err(e) = self.dht.announce_peer(self.info_hash, Some(announce_port)) {
            error!("Failed to announce peer: {}", e);
        }
        Ok(())
    }

    pub async fn tick(&mut self) -> Result<()> {
        self.announce_peer().await?;
        let peers = self.dht.get_peers(self.info_hash);
        let peers: HashSet<SocketAddrV4> = peers.into_iter().flat_map(|v| v).collect();

        // TODO: rank peers
        let my_id = self.dht.info().public_address();
        let peer_relays = peers
            .iter()
            .filter(|a| Some(a.ip()) != my_id.as_ref().map(|b| b.ip()))
            .filter_map(|a| RelayUrl::parse(&format!("ws://{}", a)).ok())
            .collect::<Vec<_>>();

        let mut sync_new = Vec::new();
        for peer in peer_relays {
            if self.client.add_relay(&peer).await? {
                info!("Connecting to DTAN peer: {}", peer);
                sync_new.push(peer);
            }
        }

        if !sync_new.is_empty() {
            self.client.connect().await;

            // start sync with new peers
            let client_sync = self.client.clone();
            let filter_sync = self.filter.clone();
            tokio::spawn(async move {
                let opts = SyncOptions::default();
                if let Err(e) = client_sync.sync_with(sync_new, filter_sync, &opts).await {
                    warn!("Failed to sync: {}", e);
                }
            });
        }
        Ok(())
    }
}
