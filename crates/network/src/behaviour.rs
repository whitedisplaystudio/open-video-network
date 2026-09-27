//! The composed libp2p behaviour.

use std::time::Duration;

use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{connection_limits, gossipsub, identify, kad, mdns, ping, request_response};
use libp2p::{PeerId, StreamProtocol};
use serde::{Deserialize, Serialize};

use ovn_protocol::{
    ContentId, MAX_GOSSIP_MESSAGE_SIZE, PROTOCOL_CHUNK, PROTOCOL_IDENTIFY, PROTOCOL_KADEMLIA,
};

use crate::{NetworkConfig, NetworkError, Result};

/// Ask a peer for one block by content id.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRequest {
    pub cid: ContentId,
}

/// The answer to a [`BlockRequest`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status", content = "data")]
pub enum BlockResponse {
    /// Here are the bytes. The requester must still check they hash to the id.
    Found(#[serde(with = "serde_bytes")] Vec<u8>),
    /// We do not hold that block.
    NotFound,
    /// We hold it but will not serve it right now — usually a rate limit.
    Refused,
}

#[derive(libp2p::swarm::NetworkBehaviour)]
pub(crate) struct Behaviour {
    /// Enforced before anything else in the stack sees a connection.
    pub limits: connection_limits::Behaviour,
    pub kad: kad::Behaviour<kad::store::MemoryStore>,
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub blocks: request_response::cbor::Behaviour<BlockRequest, BlockResponse>,
}

impl Behaviour {
    pub(crate) fn new(keypair: &libp2p::identity::Keypair, config: &NetworkConfig) -> Result<Self> {
        let peer_id = PeerId::from(keypair.public());

        let limits = connection_limits::Behaviour::new(
            connection_limits::ConnectionLimits::default()
                .with_max_established(Some(config.max_connections))
                .with_max_established_per_peer(Some(config.max_connections_per_peer)),
        );

        // Our own Kademlia protocol name. Sharing the IPFS one would mean
        // joining a DHT full of peers that know nothing about this network.
        let mut kad_config = kad::Config::new(
            StreamProtocol::try_from_owned(PROTOCOL_KADEMLIA.to_string())
                .expect("static protocol name"),
        );
        kad_config.set_query_timeout(Duration::from_secs(30));
        let mut kad =
            kad::Behaviour::with_config(peer_id, kad::store::MemoryStore::new(peer_id), kad_config);
        kad.set_mode(Some(if config.dht_server_mode {
            kad::Mode::Server
        } else {
            kad::Mode::Client
        }));

        // Deduplicate by content hash rather than by sender and sequence
        // number: the same announcement relayed along two paths is one
        // message, and a peer cannot force a re-delivery by re-signing.
        let message_id_fn = |message: &gossipsub::Message| {
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            message.data.hash(&mut hasher);
            gossipsub::MessageId::from(hasher.finish().to_be_bytes())
        };
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(1))
            .validation_mode(gossipsub::ValidationMode::Strict)
            .max_transmit_size(MAX_GOSSIP_MESSAGE_SIZE)
            .duplicate_cache_time(Duration::from_secs(600))
            .message_id_fn(message_id_fn)
            .build()
            .map_err(|e| NetworkError::Setup(format!("gossipsub config: {e}")))?;
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(keypair.clone()),
            gossipsub_config,
        )
        .map_err(|e| NetworkError::Setup(format!("gossipsub: {e}")))?;

        let mdns = if config.enable_mdns {
            Toggle::from(Some(
                mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id)
                    .map_err(|e| NetworkError::Setup(format!("mdns: {e}")))?,
            ))
        } else {
            Toggle::from(None)
        };

        let identify = identify::Behaviour::new(
            identify::Config::new(PROTOCOL_IDENTIFY.to_string(), keypair.public())
                .with_agent_version(format!("ovn/{}", env!("CARGO_PKG_VERSION"))),
        );

        let ping = ping::Behaviour::new(ping::Config::new());

        let blocks = request_response::cbor::Behaviour::new(
            [(
                StreamProtocol::try_from_owned(PROTOCOL_CHUNK.to_string())
                    .expect("static protocol name"),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default().with_request_timeout(Duration::from_secs(30)),
        );

        Ok(Self {
            limits,
            kad,
            gossipsub,
            mdns,
            identify,
            ping,
            blocks,
        })
    }
}

/// Kademlia provider key for a content id.
pub(crate) fn provider_key(cid: &ContentId) -> kad::RecordKey {
    kad::RecordKey::new(&cid.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovn_protocol::{from_cbor_slice, to_cbor_vec};

    #[test]
    fn block_requests_round_trip_through_cbor() {
        let request = BlockRequest {
            cid: ContentId::from_raw(b"chunk"),
        };
        let bytes = to_cbor_vec(&request).unwrap();
        let back: BlockRequest = from_cbor_slice(&bytes).unwrap();
        assert_eq!(back.cid, request.cid);
    }

    #[test]
    fn block_responses_round_trip_through_cbor() {
        for response in [
            BlockResponse::Found(b"payload".to_vec()),
            BlockResponse::NotFound,
            BlockResponse::Refused,
        ] {
            let bytes = to_cbor_vec(&response).unwrap();
            let back: BlockResponse = from_cbor_slice(&bytes).unwrap();
            match (response, back) {
                (BlockResponse::Found(a), BlockResponse::Found(b)) => assert_eq!(a, b),
                (BlockResponse::NotFound, BlockResponse::NotFound) => {}
                (BlockResponse::Refused, BlockResponse::Refused) => {}
                (a, b) => panic!("{a:?} became {b:?}"),
            }
        }
    }

    #[test]
    fn provider_keys_are_derived_from_the_content_id() {
        let a = ContentId::from_dag_cbor(b"one");
        let b = ContentId::from_dag_cbor(b"two");
        assert_eq!(provider_key(&a), provider_key(&a));
        assert_ne!(provider_key(&a), provider_key(&b));
    }
}
