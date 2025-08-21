use crate::{DEFAULT_RELAY_PORT, Settings};
use anyhow::Result;
use log::{debug, error, info, warn};
use mainline::{Dht, Id, ServerSettings};
use nostr_sdk::Client;
use portmapper::ProbeOutput;
use sha1::Digest;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddrV4};
use std::num::NonZeroU16;

/// Manage connected relays automatically via relay discovery and DHT
pub struct PeerManager {
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
}

impl PeerManager {
    pub fn new(client: Client, settings: Settings) -> Result<Self> {
        let digest: [u8; 20] = sha1::Sha1::digest("nostr:2003").as_slice().try_into()?;

        let mut pm_config = portmapper::Config::default();
        pm_config.protocol = portmapper::Protocol::Tcp;

        let mut dht = Dht::builder();
        if let Some(a) = &settings.dht_public_ip {
            dht.public_ip(*a);
        }

        Ok(Self {
            client,
            settings,
            dht: dht.build()?,
            portmapper: portmapper::Client::new(pm_config),
            info_hash: Id::from(digest),
        })
    }

    /// Start peer manager by creating a port mapping and advertising it to DHT
    pub async fn start(&self) -> Result<()> {
        info!("Starting peer manager...");
        let local_port = self
            .settings
            .listen
            .map(|a| a.port())
            .unwrap_or(DEFAULT_RELAY_PORT);

        // probe services
        let has_service = match self.portmapper.probe().await? {
            Ok(probe) => {
                debug!("Probe result: {:?}", probe);
                probe.nat_pmp || probe.upnp || probe.pcp
            }
            Err(e) => {
                error!("Probe error: {:?}", e);
                false
            }
        };

        if has_service {
            debug!("Probe successful, starting portmapper...");
            self.portmapper
                .update_local_port(NonZeroU16::new(local_port).unwrap());

            // spawn listener for external port mapping
            let mut addr_rx = self.portmapper.watch_external_address();
            let dht_clone = self.dht.clone();
            let id_clone = self.info_hash.clone();
            tokio::spawn(async move {
                while let Ok(_) = addr_rx.changed().await {
                    if let Some(external_address) = *addr_rx.borrow() {
                        info!("Port mapping added for: {}", external_address);

                        info!(
                            "Announcing port {} for {}",
                            external_address.port(),
                            id_clone
                        );
                        if let Err(e) =
                            dht_clone.announce_peer(id_clone, Some(external_address.port()))
                        {
                            error!("Failed to announce peer: {}", e);
                        }
                    }
                }
            });

            // TODO: timeout if no port mapping and fallback to announce local_port

            self.portmapper.procure_mapping();
        } else {
            info!("Announcing port {} for {}", local_port, self.info_hash);
            if let Err(e) = self.dht.announce_peer(self.info_hash, Some(local_port)) {
                error!("Failed to announce peer: {}", e);
            }
        }
        Ok(())
    }

    pub async fn tick(&mut self) -> Result<()> {
        let peers = self.dht.get_peers(self.info_hash);
        let peers: HashSet<SocketAddrV4> = peers.into_iter().flat_map(|v| v).collect();

        info!("DHT peers: {:?}", peers);

        // TODO: connect peers

        Ok(())
    }
}
