//! Fetching a node descriptor over HTTP(S).
//!
//! Everything a web server hands us is untrusted. The response is capped in
//! size, the descriptor is verified against its own signature and peer id, and
//! only then does anything get dialled. A hostile site can waste our time; it
//! cannot make us believe it is somebody else.

use std::time::Duration;

use ovn_protocol::{NodeDescriptor, MAX_DESCRIPTOR_SIZE, WELL_KNOWN_DESCRIPTOR_PATH};

use crate::{DiscoveryError, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REDIRECTS: usize = 3;

/// Fetches and verifies descriptors. Holds one HTTP client so that repeated
/// `peer add` calls reuse connections.
#[derive(Clone, Debug)]
pub struct DescriptorFetcher {
    client: reqwest::Client,
}

impl DescriptorFetcher {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .user_agent(concat!("ovn/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|source| DiscoveryError::Unreachable {
                url: "<client>".to_string(),
                source,
            })?;
        Ok(Self { client })
    }

    /// Fetch, verify and return the descriptor published at `url`.
    pub async fn fetch(&self, url: &str) -> Result<NodeDescriptor> {
        let url = descriptor_url(url);
        let response =
            self.client
                .get(&url)
                .send()
                .await
                .map_err(|source| DiscoveryError::Unreachable {
                    url: url.clone(),
                    source,
                })?;

        let status = response.status();
        if !status.is_success() {
            return Err(DiscoveryError::HttpStatus {
                url,
                status: status.as_u16(),
            });
        }

        // Refuse an oversized body from the declared length where we can, and
        // from the actual bytes regardless.
        if let Some(declared) = response.content_length() {
            if declared > MAX_DESCRIPTOR_SIZE as u64 {
                return Err(DiscoveryError::DescriptorTooLarge {
                    url,
                    size: declared as usize,
                    limit: MAX_DESCRIPTOR_SIZE,
                });
            }
        }
        let body = response
            .bytes()
            .await
            .map_err(|source| DiscoveryError::Unreachable {
                url: url.clone(),
                source,
            })?;
        if body.len() > MAX_DESCRIPTOR_SIZE {
            return Err(DiscoveryError::DescriptorTooLarge {
                url,
                size: body.len(),
                limit: MAX_DESCRIPTOR_SIZE,
            });
        }

        let descriptor: NodeDescriptor =
            serde_json::from_slice(&body).map_err(|e| DiscoveryError::MalformedDescriptor {
                url: url.clone(),
                message: e.to_string(),
            })?;
        descriptor
            .verify()
            .map_err(|source| DiscoveryError::Untrusted {
                url: url.clone(),
                source,
            })?;
        tracing::info!(%url, peer_id = %descriptor.peer_id, "fetched node descriptor");
        Ok(descriptor)
    }
}

/// Convenience wrapper for a one-off fetch.
pub async fn fetch_descriptor(url: &str) -> Result<NodeDescriptor> {
    DescriptorFetcher::new()?.fetch(url).await
}

/// `https://video.example.jp` becomes
/// `https://video.example.jp/.well-known/ovn/node.json`, while a URL that
/// already points at a document is left alone.
fn descriptor_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    match url::Url::parse(trimmed) {
        Ok(parsed) if parsed.path().is_empty() || parsed.path() == "/" => {
            format!("{trimmed}{WELL_KNOWN_DESCRIPTOR_PATH}")
        }
        _ => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovn_identity::Identity;
    use ovn_protocol::Capability;

    #[test]
    fn a_bare_host_gets_the_well_known_path() {
        assert_eq!(
            descriptor_url("https://video.example.jp"),
            "https://video.example.jp/.well-known/ovn/node.json"
        );
        assert_eq!(
            descriptor_url("https://video.example.jp/"),
            "https://video.example.jp/.well-known/ovn/node.json"
        );
    }

    #[test]
    fn an_explicit_document_url_is_used_as_given() {
        assert_eq!(
            descriptor_url("https://example.com/nodes/mine.json"),
            "https://example.com/nodes/mine.json"
        );
    }

    /// A one-request HTTP server, so the fetch path is exercised end to end
    /// without a mocking framework. All I/O is async: a blocking write here
    /// would deadlock the test runtime once a body outgrows the socket buffer.
    async fn serve_once(body: Vec<u8>, status_line: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            // Consume the request line and headers before replying.
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen.extend_from_slice(&buf[..n]),
                }
            }
            let head = format!(
                "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
            let _ = stream.shutdown().await;
        });
        format!("http://{addr}/.well-known/ovn/node.json")
    }

    fn signed_descriptor() -> NodeDescriptor {
        NodeDescriptor::sign(
            "Example".into(),
            vec!["/ip4/192.0.2.10/udp/4800/quic-v1".into()],
            vec![Capability::bootstrap()],
            &Identity::generate(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_valid_descriptor_is_fetched_and_verified() {
        let descriptor = signed_descriptor();
        let body = serde_json::to_vec(&descriptor).unwrap();
        let url = serve_once(body, "200 OK").await;
        let fetched = fetch_descriptor(&url).await.unwrap();
        assert_eq!(fetched, descriptor);
    }

    #[tokio::test]
    async fn a_tampered_descriptor_is_refused() {
        let mut descriptor = signed_descriptor();
        descriptor.addresses = vec!["/ip4/198.51.100.99/tcp/4800".into()];
        let body = serde_json::to_vec(&descriptor).unwrap();
        let url = serve_once(body, "200 OK").await;
        assert!(matches!(
            fetch_descriptor(&url).await,
            Err(DiscoveryError::Untrusted { .. })
        ));
    }

    #[tokio::test]
    async fn an_error_status_is_reported_clearly() {
        let url = serve_once(b"nope".to_vec(), "404 Not Found").await;
        assert!(matches!(
            fetch_descriptor(&url).await,
            Err(DiscoveryError::HttpStatus { status: 404, .. })
        ));
    }

    #[tokio::test]
    async fn a_non_descriptor_response_is_reported_clearly() {
        let url = serve_once(b"<html>hello</html>".to_vec(), "200 OK").await;
        assert!(matches!(
            fetch_descriptor(&url).await,
            Err(DiscoveryError::MalformedDescriptor { .. })
        ));
    }

    #[tokio::test]
    async fn an_oversized_response_is_refused() {
        let url = serve_once(vec![b'x'; MAX_DESCRIPTOR_SIZE + 1], "200 OK").await;
        assert!(matches!(
            fetch_descriptor(&url).await,
            Err(DiscoveryError::DescriptorTooLarge { .. })
        ));
    }
}
