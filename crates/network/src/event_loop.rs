//! The swarm task: one place where all libp2p state lives.

use std::collections::{HashMap, HashSet};

use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{gossipsub, identify, kad, mdns, request_response};
use libp2p::{Multiaddr, PeerId, Swarm};
use tokio::sync::{mpsc, oneshot};

use ovn_protocol::{ContentId, TOPIC_PROFILE_UPDATE, TOPIC_VIDEO_ANNOUNCE};

use crate::behaviour::{provider_key, Behaviour, BehaviourEvent, BlockRequest, BlockResponse};
use crate::handle::{BlockResponder, Command, DiscoverySource, NetworkEvent, NetworkStatus};
use crate::rate_limit::RateLimiter;
use crate::{NetworkConfig, NetworkError, Result};

pub(crate) struct EventLoop {
    swarm: Swarm<Behaviour>,
    config: NetworkConfig,
    commands: mpsc::Receiver<Command>,
    /// Kept so that inbound block requests can be answered.
    command_sender: mpsc::Sender<Command>,
    events: mpsc::Sender<NetworkEvent>,

    announce_topic: gossipsub::IdentTopic,
    profile_topic: gossipsub::IdentTopic,

    pending_dials: HashMap<PeerId, Vec<oneshot::Sender<Result<PeerId>>>>,
    pending_providers: HashMap<kad::QueryId, (oneshot::Sender<Vec<PeerId>>, HashSet<PeerId>)>,
    pending_blocks: HashMap<request_response::OutboundRequestId, oneshot::Sender<Result<Vec<u8>>>>,
    providing: HashSet<ContentId>,

    gossip_limiter: RateLimiter,
    block_limiter: RateLimiter,
}

impl EventLoop {
    pub(crate) fn new(
        swarm: Swarm<Behaviour>,
        config: NetworkConfig,
        commands: mpsc::Receiver<Command>,
        command_sender: mpsc::Sender<Command>,
        events: mpsc::Sender<NetworkEvent>,
    ) -> Self {
        Self {
            gossip_limiter: RateLimiter::per_minute(config.gossip_rate_per_minute),
            block_limiter: RateLimiter::per_minute(config.block_request_rate_per_minute),
            swarm,
            config,
            commands,
            command_sender,
            events,
            announce_topic: gossipsub::IdentTopic::new(TOPIC_VIDEO_ANNOUNCE),
            profile_topic: gossipsub::IdentTopic::new(TOPIC_PROFILE_UPDATE),
            pending_dials: HashMap::new(),
            pending_providers: HashMap::new(),
            pending_blocks: HashMap::new(),
            providing: HashSet::new(),
        }
    }

    pub(crate) async fn run(mut self) {
        let mut bootstrap_timer = tokio::time::interval(self.config.bootstrap_interval);
        // The first tick fires immediately; the initial bootstrap is driven by
        // the node once it has dialled its known peers, so skip it here.
        bootstrap_timer.tick().await;

        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    self.on_swarm_event(event).await;
                }
                command = self.commands.recv() => {
                    match command {
                        Some(Command::Shutdown) | None => break,
                        Some(command) => self.on_command(command).await,
                    }
                }
                _ = bootstrap_timer.tick() => {
                    let _ = self.swarm.behaviour_mut().kad.bootstrap();
                }
            }
        }
        tracing::info!("network event loop stopped");
    }

    /// Deliberately not `async`: the swarm must never be blocked waiting for
    /// a consumer. A full channel means the node is not draining events, and
    /// dropping one is better than stalling every peer we are connected to.
    fn emit(&self, event: NetworkEvent) {
        if self.events.try_send(event).is_err() {
            tracing::warn!("network event dropped: consumer is not keeping up");
        }
    }

    // ------------------------------------------------------------ commands

    async fn on_command(&mut self, command: Command) {
        match command {
            Command::Dial { addr, reply } => self.dial(addr, reply),
            Command::AddPeerAddress { peer, addr } => {
                self.swarm.behaviour_mut().kad.add_address(&peer, addr);
            }
            Command::Publish { topic, data, reply } => {
                let topic = if topic == TOPIC_VIDEO_ANNOUNCE {
                    self.announce_topic.clone()
                } else {
                    self.profile_topic.clone()
                };
                let result = self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(topic, data)
                    .map(|_| ())
                    .map_err(|e| match e {
                        gossipsub::PublishError::NoPeersSubscribedToTopic => NetworkError::NoPeers,
                        other => NetworkError::Publish(other.to_string()),
                    });
                let _ = reply.send(result);
            }
            Command::StartProviding { cid, reply } => {
                let result = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .start_providing(provider_key(&cid))
                    .map(|_| ())
                    .map_err(|e| NetworkError::Dht(e.to_string()));
                if result.is_ok() {
                    self.providing.insert(cid);
                }
                let _ = reply.send(result);
            }
            Command::GetProviders { cid, reply } => {
                let query = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .get_providers(provider_key(&cid));
                self.pending_providers
                    .insert(query, (reply, HashSet::new()));
            }
            Command::RequestBlock { peer, cid, reply } => {
                let id = self
                    .swarm
                    .behaviour_mut()
                    .blocks
                    .send_request(&peer, BlockRequest { cid });
                self.pending_blocks.insert(id, reply);
            }
            Command::RespondBlock { channel, response } => {
                // Failure here only means the requester went away.
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .blocks
                    .send_response(*channel, response);
            }
            Command::Status { reply } => {
                let status = NetworkStatus {
                    local_peer_id: *self.swarm.local_peer_id(),
                    listen_addrs: self.swarm.listeners().cloned().collect(),
                    external_addrs: self.swarm.external_addresses().cloned().collect(),
                    connected_peers: self.swarm.connected_peers().copied().collect(),
                    routing_table_peers: self
                        .swarm
                        .behaviour_mut()
                        .kad
                        .kbuckets()
                        .map(|bucket| bucket.num_entries())
                        .sum(),
                    providing: self.providing.len(),
                };
                let _ = reply.send(status);
            }
            Command::Bootstrap => {
                let _ = self.swarm.behaviour_mut().kad.bootstrap();
            }
            Command::Shutdown => {}
        }
    }

    fn dial(&mut self, addr: Multiaddr, reply: oneshot::Sender<Result<PeerId>>) {
        // A multiaddr that names its peer lets us report success precisely;
        // without one we still dial, but cannot match the reply to a peer id.
        let peer = extract_peer_id(&addr);
        match self.swarm.dial(addr.clone()) {
            Ok(()) => match peer {
                Some(peer) => self.pending_dials.entry(peer).or_default().push(reply),
                None => {
                    let _ = reply.send(Err(NetworkError::Dial(
                        "the address does not name a peer id".to_string(),
                    )));
                }
            },
            Err(e) => {
                let _ = reply.send(Err(NetworkError::Dial(e.to_string())));
            }
        }
    }

    // -------------------------------------------------------- swarm events

    async fn on_swarm_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!(%address, "listening");
                self.emit(NetworkEvent::Listening(address));
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                if let Some(waiting) = self.pending_dials.remove(&peer_id) {
                    for reply in waiting {
                        let _ = reply.send(Ok(peer_id));
                    }
                }
                self.emit(NetworkEvent::PeerConnected(peer_id));
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                if num_established == 0 {
                    self.emit(NetworkEvent::PeerDisconnected(peer_id));
                }
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            } => {
                if let Some(waiting) = self.pending_dials.remove(&peer_id) {
                    for reply in waiting {
                        let _ = reply.send(Err(NetworkError::Dial(error.to_string())));
                    }
                }
            }
            SwarmEvent::Behaviour(event) => self.on_behaviour_event(event).await,
            _ => {}
        }
    }

    async fn on_behaviour_event(&mut self, event: BehaviourEvent) {
        match event {
            BehaviourEvent::Mdns(mdns::Event::Discovered(peers)) => {
                let mut by_peer: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
                for (peer, addr) in peers {
                    self.swarm
                        .behaviour_mut()
                        .kad
                        .add_address(&peer, addr.clone());
                    by_peer.entry(peer).or_default().push(addr);
                }
                for (peer, addresses) in by_peer {
                    self.emit(NetworkEvent::PeerDiscovered {
                        peer,
                        addresses,
                        source: DiscoverySource::Mdns,
                    });
                }
            }
            BehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
                // Only route peers that speak our Kademlia protocol.
                let speaks_kad = info
                    .protocols
                    .iter()
                    .any(|p| p.as_ref() == ovn_protocol::PROTOCOL_KADEMLIA);
                if speaks_kad {
                    for addr in &info.listen_addrs {
                        self.swarm
                            .behaviour_mut()
                            .kad
                            .add_address(&peer_id, addr.clone());
                    }
                }
                self.emit(NetworkEvent::PeerDiscovered {
                    peer: peer_id,
                    addresses: info.listen_addrs,
                    source: DiscoverySource::Identify,
                });
            }
            BehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            }) => {
                if !self.gossip_limiter.allow(&propagation_source) {
                    tracing::debug!(peer = %propagation_source, "dropping gossip: over rate limit");
                    return;
                }
                let topic = message.topic.as_str();
                let event = if topic == self.announce_topic.hash().as_str() {
                    NetworkEvent::GossipAnnouncement {
                        from: propagation_source,
                        data: message.data,
                    }
                } else if topic == self.profile_topic.hash().as_str() {
                    NetworkEvent::GossipProfile {
                        from: propagation_source,
                        data: message.data,
                    }
                } else {
                    return;
                };
                self.emit(event);
            }
            BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                id, result, step, ..
            }) => self.on_kad_query(id, result, step).await,
            BehaviourEvent::Kad(kad::Event::RoutingUpdated {
                peer, addresses, ..
            }) => {
                // The DHT is how a node learns about peers it has never
                // spoken to. Recording them here is what lets a node keep
                // going after the entry point it used has disappeared.
                self.emit(NetworkEvent::PeerDiscovered {
                    peer,
                    addresses: addresses.into_vec(),
                    source: DiscoverySource::Dht,
                });
            }
            BehaviourEvent::Blocks(request_response::Event::Message { peer, message, .. }) => {
                match message {
                    request_response::Message::Request {
                        request, channel, ..
                    } => {
                        if !self.block_limiter.allow(&peer) {
                            let _ = self
                                .swarm
                                .behaviour_mut()
                                .blocks
                                .send_response(channel, BlockResponse::Refused);
                            return;
                        }
                        self.emit(NetworkEvent::BlockRequested {
                            peer,
                            cid: request.cid,
                            responder: BlockResponder::new(channel, self.command_sender.clone()),
                        });
                    }
                    request_response::Message::Response {
                        request_id,
                        response,
                    } => {
                        if let Some(reply) = self.pending_blocks.remove(&request_id) {
                            let _ = reply.send(match response {
                                BlockResponse::Found(data) => Ok(data),
                                BlockResponse::NotFound => Err(NetworkError::BlockNotFound),
                                BlockResponse::Refused => Err(NetworkError::BlockRefused),
                            });
                        }
                    }
                }
            }
            BehaviourEvent::Blocks(request_response::Event::OutboundFailure {
                request_id,
                error,
                ..
            }) => {
                if let Some(reply) = self.pending_blocks.remove(&request_id) {
                    let _ = reply.send(Err(NetworkError::Transfer(error.to_string())));
                }
            }
            _ => {}
        }
    }

    async fn on_kad_query(
        &mut self,
        id: kad::QueryId,
        result: kad::QueryResult,
        step: kad::ProgressStep,
    ) {
        let kad::QueryResult::GetProviders(result) = result else {
            return;
        };
        let Some((_, found)) = self.pending_providers.get_mut(&id) else {
            return;
        };
        match result {
            Ok(kad::GetProvidersOk::FoundProviders { providers, .. }) => {
                found.extend(providers);
            }
            Ok(kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. }) => {}
            Err(_) => {}
        }
        if step.last {
            if let Some((reply, found)) = self.pending_providers.remove(&id) {
                let _ = reply.send(found.into_iter().collect());
            }
            // A finished query holds resources in the DHT until it is closed.
            if let Some(mut query) = self.swarm.behaviour_mut().kad.query_mut(&id) {
                query.finish();
            }
        }
    }
}

/// Pull the `/p2p/<peer id>` component out of a multiaddr, if present.
pub(crate) fn extract_peer_id(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_ids_are_read_out_of_multiaddrs() {
        let peer = PeerId::random();
        let addr: Multiaddr = format!("/ip4/192.0.2.1/udp/4800/quic-v1/p2p/{peer}")
            .parse()
            .unwrap();
        assert_eq!(extract_peer_id(&addr), Some(peer));
    }

    #[test]
    fn an_address_without_a_peer_id_yields_none() {
        let addr: Multiaddr = "/ip4/192.0.2.1/udp/4800/quic-v1".parse().unwrap();
        assert_eq!(extract_peer_id(&addr), None);
    }
}
