//! The swarm task: one place where all libp2p state lives.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::StreamExt;
use libp2p::core::transport::ListenerId;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::swarm::{DialError, SwarmEvent};
use libp2p::{dcutr, gossipsub, identify, kad, mdns, relay, request_response, upnp};
use libp2p::{Multiaddr, PeerId, Swarm};
use tokio::sync::{mpsc, oneshot};

use ovn_protocol::{
    ChannelRequest, ChannelResponse, ContentId, VideoAnnouncement, TOPIC_PROFILE_UPDATE,
    TOPIC_VIDEO_ANNOUNCE,
};

use crate::behaviour::{
    channel_key, provider_key, speaks_relay_hop, Behaviour, BehaviourEvent, BlockRequest,
    BlockResponse,
};
use crate::handle::{
    BlockResponder, ChannelResponder, Command, DiscoverySource, NetworkEvent, NetworkStatus,
    Reachability,
};
use crate::rate_limit::RateLimiter;
use crate::{NetworkConfig, NetworkError, Result};

/// How long to wait for a dial before telling the caller it did not work.
const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Someone waiting on a dial, and when we stop waiting on their behalf.
type PendingDial = (oneshot::Sender<Result<PeerId>>, tokio::time::Instant);

pub(crate) struct EventLoop {
    swarm: Swarm<Behaviour>,
    config: NetworkConfig,
    commands: mpsc::Receiver<Command>,
    /// Kept so that inbound block requests can be answered.
    command_sender: mpsc::Sender<Command>,
    events: mpsc::Sender<NetworkEvent>,

    announce_topic: gossipsub::IdentTopic,
    profile_topic: gossipsub::IdentTopic,

    /// Dials waiting on a connection, with the moment we give up on them.
    pending_dials: HashMap<PeerId, Vec<PendingDial>>,
    pending_providers: HashMap<kad::QueryId, (oneshot::Sender<Vec<PeerId>>, HashSet<PeerId>)>,
    pending_blocks: HashMap<request_response::OutboundRequestId, oneshot::Sender<Result<Vec<u8>>>>,
    pending_channels: HashMap<
        request_response::OutboundRequestId,
        oneshot::Sender<Result<Vec<VideoAnnouncement>>>,
    >,
    providing: HashSet<ContentId>,

    /// Whether anyone can dial us, as far as we have been told.
    reachability: Reachability,
    /// Relays holding a slot for us, and the listener each one created.
    relays: HashMap<PeerId, ListenerId>,

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
        let config_external = !config.external_addrs.is_empty();
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
            pending_channels: HashMap::new(),
            providing: HashSet::new(),
            // An operator who passed `--external-addr` has answered the
            // question already.
            reachability: if config_external {
                Reachability::Public
            } else {
                Reachability::Unknown
            },
            relays: HashMap::new(),
        }
    }

    /// Give up on dials that have taken too long, so a caller is never left
    /// waiting on an answer that is not coming.
    fn expire_pending_dials(&mut self) {
        let now = tokio::time::Instant::now();
        self.pending_dials.retain(|peer, waiting| {
            waiting.retain(|(_, deadline)| *deadline > now);
            if waiting.is_empty() {
                tracing::debug!(%peer, "giving up on a dial that never resolved");
            }
            !waiting.is_empty()
        });
    }

    /// Record a change in whether others can dial us, and tell the node.
    fn set_reachability(&mut self, reachability: Reachability) {
        if self.reachability == reachability {
            return;
        }
        tracing::info!(
            from = self.reachability.as_str(),
            to = reachability.as_str(),
            "reachability changed"
        );
        self.reachability = reachability;

        // A reservation we no longer need is left to lapse rather than
        // closed. Closing the listener drops libp2p's bookkeeping for that
        // connection, and a reservation acceptance or renewal already in
        // flight then panics a runtime worker inside `libp2p-relay`
        // (`priv_client.rs`: "Relay connection exist"). Since renewals happen
        // on a timer there is no moment that is reliably safe, so we stop
        // asking for new slots and let the existing ones expire on their own.
        self.emit(NetworkEvent::ReachabilityChanged { reachability });
    }

    /// Ask a peer that offers to relay for a slot, so that we have an address
    /// others can dial.
    ///
    /// Only worth doing while we are unreachable ourselves; a node others can
    /// dial directly gains nothing from a relay and should leave the capacity
    /// for someone who needs it.
    fn reserve_relay(&mut self, peer: PeerId, addresses: &[Multiaddr]) {
        if self.reachability == Reachability::Public
            || self.relays.contains_key(&peer)
            || self.relays.len() >= self.config.max_relay_reservations
        {
            return;
        }
        let Some(address) = addresses.iter().find(|a| is_dialable(a)) else {
            return;
        };

        // `<their address>/p2p/<them>/p2p-circuit` is an address on their
        // relay; listening on it is how we ask for the slot.
        let mut circuit = address.clone();
        if extract_peer_id(&circuit).is_none() {
            circuit.push(Protocol::P2p(peer));
        }
        circuit.push(Protocol::P2pCircuit);

        match self.swarm.listen_on(circuit.clone()) {
            Ok(listener) => {
                tracing::info!(%peer, %circuit, "asking a peer to relay for us");
                self.relays.insert(peer, listener);
            }
            Err(e) => {
                tracing::debug!(%peer, %circuit, error = %e, "could not ask for a relay slot")
            }
        }
    }

    pub(crate) async fn run(mut self) {
        let mut bootstrap_timer = tokio::time::interval(self.config.bootstrap_interval);
        // A dial can fail in ways the swarm never reports against the peer we
        // asked for — a relayed address whose relay refuses, say. Without a
        // deadline the caller waits forever, and `ourvideo peer add` never
        // returns.
        let mut dial_sweep = tokio::time::interval(Duration::from_secs(2));
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
                _ = dial_sweep.tick() => {
                    self.expire_pending_dials();
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
            Command::AddExternalAddress { addr } => {
                // `Swarm::add_external_address` tells the behaviours but
                // produces no event of its own, so record the consequence
                // here: we now know we are reachable.
                self.swarm.add_external_address(addr);
                self.set_reachability(Reachability::Public);
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
            Command::ProvideChannel { public_key, reply } => {
                let result = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .start_providing(channel_key(&public_key))
                    .map(|_| ())
                    .map_err(|e| NetworkError::Dht(e.to_string()));
                let _ = reply.send(result);
            }
            Command::ChannelProviders { public_key, reply } => {
                let query = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .get_providers(channel_key(&public_key));
                self.pending_providers
                    .insert(query, (reply, HashSet::new()));
            }
            Command::RequestChannel {
                peer,
                public_key,
                since,
                reply,
            } => {
                let id = self
                    .swarm
                    .behaviour_mut()
                    .channels
                    .send_request(&peer, ChannelRequest { public_key, since });
                self.pending_channels.insert(id, reply);
            }
            Command::RespondChannel { channel, response } => {
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .channels
                    .send_response(*channel, response);
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
                    reachability: self.reachability,
                    relays: self.relays.keys().copied().collect(),
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
        //
        // A relayed address names two: the relay, then the peer we actually
        // want. The last one is the destination.
        let peer = dial_target(&addr);

        if let Some(peer) = peer {
            // Someone we are already talking to needs no second connection.
            // Pasting a share link for a peer we met a minute ago is the
            // ordinary case, and every address in that link would otherwise
            // open its own connection. Four of those reach
            // `max_connections_per_peer`, after which the swarm refuses the
            // dial and we report a peer sitting right there as unreachable.
            if self.swarm.is_connected(&peer) {
                let _ = reply.send(Ok(peer));
                return;
            }
        }

        // Dialling by peer id rather than by address alone lets the swarm
        // collapse concurrent attempts at the same peer into one.
        let opts = match peer {
            Some(peer) => DialOpts::peer_id(peer)
                .addresses(vec![addr.clone()])
                .condition(PeerCondition::DisconnectedAndNotDialing)
                .build(),
            None => DialOpts::from(addr.clone()),
        };

        let outcome = self.swarm.dial(opts);
        let Some(peer) = peer else {
            let _ = reply.send(Err(NetworkError::Dial(
                "the address does not name a peer id".to_string(),
            )));
            return;
        };
        match outcome {
            // A dial is already on its way to this peer. Its result is the
            // one the caller is waiting for, so wait for it rather than
            // starting a second attempt.
            Ok(()) | Err(DialError::DialPeerConditionFalse(_)) => self
                .pending_dials
                .entry(peer)
                .or_default()
                .push((reply, tokio::time::Instant::now() + DIAL_TIMEOUT)),
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
                // A circuit address only exists once a relay has accepted, so
                // this is the first moment we can report one.
                if address.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
                    if let Some(relay) = extract_peer_id(&address) {
                        self.emit(NetworkEvent::RelayReserved {
                            relay,
                            address: address.clone(),
                        });
                    }
                }
                self.emit(NetworkEvent::Listening(address));
            }
            // An address another peer has actually reached us on.
            SwarmEvent::ExternalAddrConfirmed { address } => {
                // A relayed address is somewhere others can reach us, but it
                // is not us being reachable: our router still refuses
                // everything. Treating it as reachable would stop us looking
                // for more relays and would have us offer to relay for
                // others, which we cannot do.
                let relayed = address.iter().any(|p| matches!(p, Protocol::P2pCircuit));
                tracing::info!(%address, relayed, "confirmed reachable from outside");
                if !relayed {
                    self.set_reachability(Reachability::Public);
                }
                self.emit(NetworkEvent::Listening(address));
            }
            SwarmEvent::ListenerClosed { listener_id, .. } => {
                // A relay slot we lost; make room to find another.
                self.relays.retain(|_, id| *id != listener_id);
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                if let Some(waiting) = self.pending_dials.remove(&peer_id) {
                    for (reply, _) in waiting {
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
                    for (reply, _) in waiting {
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
            BehaviourEvent::Upnp(upnp::Event::NewExternalAddr { external_addr, .. }) => {
                // The router opened a port for us, which beats a relay.
                tracing::info!(address = %external_addr, "the router forwarded a port for us");
                self.swarm.add_external_address(external_addr);
                self.set_reachability(Reachability::Public);
            }
            BehaviourEvent::Upnp(upnp::Event::GatewayNotFound) => {
                tracing::debug!("no router willing to forward a port; a relay may be needed");
            }
            BehaviourEvent::AutonatClient(event) => {
                // A failed probe means nobody could dial the address we
                // offered, so we are behind something.
                if event.result.is_err() && self.reachability != Reachability::Public {
                    self.set_reachability(Reachability::Private);
                }
            }
            BehaviourEvent::RelayClient(relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                ..
            }) => {
                // The address it gives us arrives separately, as a listen
                // address; that is where the event carrying it is emitted.
                tracing::info!(relay = %relay_peer_id, "a peer agreed to relay for us");
            }
            BehaviourEvent::Dcutr(dcutr::Event {
                remote_peer_id,
                result,
            }) => match result {
                Ok(_) => {
                    tracing::info!(peer = %remote_peer_id, "punched a hole through both routers");
                    self.emit(NetworkEvent::HolePunched {
                        peer: remote_peer_id,
                    });
                }
                Err(e) => {
                    tracing::debug!(peer = %remote_peer_id, error = %e, "hole punching failed; staying on the relay");
                }
            },
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
                // A peer that relays is how an unreachable node gets an
                // address at all. Nobody publishes a list of them; we notice
                // as we meet them.
                if speaks_relay_hop(&info.protocols) {
                    self.reserve_relay(peer_id, &info.listen_addrs);
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
            BehaviourEvent::Channels(request_response::Event::Message {
                peer, message, ..
            }) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    // Answering costs a database read, so it shares the block
                    // budget rather than opening a second way to spend ours.
                    if !self.block_limiter.allow(&peer) {
                        let _ = self
                            .swarm
                            .behaviour_mut()
                            .channels
                            .send_response(channel, ChannelResponse::default());
                        return;
                    }
                    self.emit(NetworkEvent::ChannelRequested {
                        peer,
                        public_key: request.public_key,
                        since: request.since,
                        responder: ChannelResponder::new(channel, self.command_sender.clone()),
                    });
                }
                request_response::Message::Response {
                    request_id,
                    response,
                } => {
                    if let Some(reply) = self.pending_channels.remove(&request_id) {
                        let _ = reply.send(Ok(response.announcements));
                    }
                }
            },
            BehaviourEvent::Channels(request_response::Event::OutboundFailure {
                request_id,
                error,
                ..
            }) => {
                if let Some(reply) = self.pending_channels.remove(&request_id) {
                    let _ = reply.send(Err(NetworkError::Transfer(error.to_string())));
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

/// Is this an address we could actually dial, rather than a placeholder or
/// an address that only means something on the machine that reported it?
fn is_dialable(addr: &Multiaddr) -> bool {
    // A relayed address cannot itself host a relay, and an unspecified
    // address is a listener's wildcard rather than somewhere to connect.
    if addr.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
        return false;
    }
    addr.iter().all(|p| match p {
        Protocol::Ip4(ip) => !ip.is_unspecified(),
        Protocol::Ip6(ip) => !ip.is_unspecified(),
        _ => true,
    })
}

/// Pull the first `/p2p/<peer id>` component out of a multiaddr, if present.
///
/// For a plain address that is the peer; for a relayed one it is the relay,
/// which is what the routing table wants to know about.
pub(crate) fn extract_peer_id(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

/// The peer a dial is actually trying to reach.
///
/// `/ip4/…/p2p/<relay>/p2p-circuit/p2p/<destination>` names two peers; the
/// connection we are waiting for is with the last.
fn dial_target(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter()
        .filter_map(|p| match p {
            Protocol::P2p(peer) => Some(peer),
            _ => None,
        })
        .last()
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
    fn a_relayed_dial_waits_for_the_destination_not_the_relay() {
        let relay = PeerId::random();
        let destination = PeerId::random();
        let addr: Multiaddr =
            format!("/ip4/192.0.2.1/udp/4800/quic-v1/p2p/{relay}/p2p-circuit/p2p/{destination}")
                .parse()
                .unwrap();
        assert_eq!(dial_target(&addr), Some(destination));
        // The routing table still wants to know where the relay is.
        assert_eq!(extract_peer_id(&addr), Some(relay));

        // A plain address names one peer, and both agree on it.
        let plain: Multiaddr = format!("/ip4/192.0.2.1/udp/4800/quic-v1/p2p/{destination}")
            .parse()
            .unwrap();
        assert_eq!(dial_target(&plain), Some(destination));
        assert_eq!(extract_peer_id(&plain), Some(destination));
    }

    #[test]
    fn a_wildcard_or_relayed_address_is_not_somewhere_to_dial() {
        assert!(is_dialable(
            &"/ip4/192.0.2.1/udp/4800/quic-v1".parse().unwrap()
        ));
        assert!(is_dialable(&"/ip4/127.0.0.1/tcp/4800".parse().unwrap()));
        // A listener's wildcard means "every interface here", not an address.
        assert!(!is_dialable(&"/ip4/0.0.0.0/tcp/4800".parse().unwrap()));
        assert!(!is_dialable(&"/ip6/::/udp/4800/quic-v1".parse().unwrap()));
        // A relay cannot be reached through a relay.
        let peer = PeerId::random();
        assert!(!is_dialable(
            &format!("/ip4/192.0.2.1/tcp/4800/p2p/{peer}/p2p-circuit")
                .parse()
                .unwrap()
        ));
    }

    #[test]
    fn an_address_without_a_peer_id_yields_none() {
        let addr: Multiaddr = "/ip4/192.0.2.1/udp/4800/quic-v1".parse().unwrap();
        assert_eq!(extract_peer_id(&addr), None);
    }
}
