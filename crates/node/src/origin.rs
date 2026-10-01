//! Fetching video bytes from where the creator put them.
//!
//! The network carries what is needed to find a video and to check it. It does
//! not carry the video. This module is the other half: it goes to the
//! creator's server, asks for the bytes, and refuses to hand over anything
//! that does not match what was announced.
//!
//! That refusal is the whole reason the manifest still exists. A chunk hash is
//! no longer an address — nobody is being asked for a chunk by id — but it is
//! still a promise, signed by the creator, about what the bytes at a given
//! offset must be. A server that has been swapped, compromised, or told to
//! serve something else to some viewers cannot do it without every one of
//! them noticing.

use std::net::IpAddr;

use ovn_content::VideoManifest;
use ovn_protocol::ContentId;

use crate::{NodeError, Result};

/// How many redirects to follow. Enough for the usual object-store hop,
/// few enough that a chain cannot be used as a probe.
const MAX_REDIRECTS: usize = 4;

/// How long to wait for the start of a response.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// A client for creators' own servers.
#[derive(Clone, Debug)]
pub(crate) struct Origin {
    http: reqwest::Client,
    /// Whether a URL may point inside a network. See
    /// [`NodeConfig::allow_private_sources`](crate::NodeConfig).
    allow_private: bool,
}

impl Origin {
    pub(crate) fn new(allow_private: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            // No overall timeout: a chunk on a slow line is not an error.
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                // Checked at every hop, not only on the URL the creator
                // signed. Otherwise an announcement could point at a server
                // whose only job is to redirect a stranger's node into their
                // own network.
                match host_is_safe(attempt.url(), allow_private) {
                    Ok(()) => attempt.follow(),
                    Err(e) => attempt.error(e),
                }
            }))
            .build()
            .map_err(|e| NodeError::Origin {
                url: String::new(),
                reason: e.to_string(),
            })?;
        Ok(Self {
            http,
            allow_private,
        })
    }

    /// One chunk of a video, checked against the manifest before it is
    /// returned.
    pub(crate) async fn chunk(
        &self,
        url: &str,
        manifest: &VideoManifest,
        index: usize,
    ) -> Result<Vec<u8>> {
        let expected_cid = *manifest
            .chunks
            .get(index)
            .ok_or_else(|| NodeError::Origin {
                url: url.to_string(),
                reason: format!("the manifest has no chunk {index}"),
            })?;
        let expected_len = manifest.chunk_len(index).ok_or_else(|| NodeError::Origin {
            url: url.to_string(),
            reason: format!("the manifest gives no length for chunk {index}"),
        })?;

        let start = index as u64 * manifest.chunk_size as u64;
        let end = start + expected_len - 1;
        let bytes = self.range(url, start, end, expected_len).await?;
        verify(url, &bytes, &expected_cid, expected_len)?;
        Ok(bytes)
    }

    /// One byte range, with the size bounded before anything is read.
    async fn range(&self, url: &str, start: u64, end: u64, expected_len: u64) -> Result<Vec<u8>> {
        host_is_safe(
            &url.parse::<reqwest::Url>().map_err(|e| NodeError::Origin {
                url: url.to_string(),
                reason: e.to_string(),
            })?,
            self.allow_private,
        )
        .map_err(|reason| NodeError::Origin {
            url: url.to_string(),
            reason,
        })?;

        let response = self
            .http
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|e| NodeError::Origin {
                url: url.to_string(),
                reason: e.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(NodeError::Origin {
                url: url.to_string(),
                reason: format!("the server answered {status}"),
            });
        }
        // A server that ignores `Range` sends the whole file with 200. Reading
        // it would mean holding a whole video in memory to find the piece we
        // asked for, so that is refused rather than tolerated.
        if status != reqwest::StatusCode::PARTIAL_CONTENT && expected_len < total_hint(&response) {
            return Err(NodeError::Origin {
                url: url.to_string(),
                reason: "the server does not support range requests".to_string(),
            });
        }
        if let Some(len) = response.content_length() {
            if len != expected_len {
                return Err(NodeError::Origin {
                    url: url.to_string(),
                    reason: format!("asked for {expected_len} bytes and was offered {len}"),
                });
            }
        }
        let bytes = response.bytes().await.map_err(|e| NodeError::Origin {
            url: url.to_string(),
            reason: e.to_string(),
        })?;
        Ok(bytes.to_vec())
    }
}

fn total_hint(response: &reqwest::Response) -> u64 {
    response.content_length().unwrap_or(0)
}

/// Does the bytes' hash match what the creator signed?
fn verify(url: &str, bytes: &[u8], expected: &ContentId, expected_len: u64) -> Result<()> {
    if bytes.len() as u64 != expected_len {
        return Err(NodeError::Origin {
            url: url.to_string(),
            reason: format!(
                "got {} bytes where {expected_len} were promised",
                bytes.len()
            ),
        });
    }
    if !expected.verifies(bytes) {
        return Err(NodeError::OriginTampered {
            url: url.to_string(),
            cid: expected.to_string(),
        });
    }
    Ok(())
}

/// Refuse a URL that points inside a network rather than out at the internet.
///
/// A node fetches a stranger's URL because an announcement said to. Without
/// this, publishing `http://192.168.0.1/` would make every viewer's node probe
/// their own router. Only literal addresses are checked: a hostname that
/// resolves to a private address is not caught, which is stated in
/// `docs/SECURITY.md` rather than pretended away.
fn host_is_safe(url: &reqwest::Url, allow_private: bool) -> std::result::Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("scheme {other} is not fetched")),
    }
    let Some(host) = url.host_str() else {
        return Err("no host".to_string());
    };
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        let private = match ip {
            IpAddr::V4(v4) => {
                v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    // fc00::/7 unique-local and fe80::/10 link-local.
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
        if private && !allow_private {
            return Err(format!(
                "{ip} is not an address on the internet; pass \
                 --allow-private-sources if that is deliberate"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe(url: &str) -> bool {
        host_is_safe(&url.parse().unwrap(), false).is_ok()
    }

    fn safe_when_allowed(url: &str) -> bool {
        host_is_safe(&url.parse().unwrap(), true).is_ok()
    }

    #[test]
    fn an_ordinary_url_is_fetched() {
        assert!(safe("https://videos.example/clip.mp4"));
        assert!(safe("http://videos.example:8080/clip.mp4"));
    }

    #[test]
    fn an_address_inside_a_network_is_refused() {
        // Otherwise an announcement is a way to make strangers' nodes knock on
        // doors inside their own houses.
        for url in [
            "http://127.0.0.1/clip.mp4",
            "http://10.0.0.5/clip.mp4",
            "http://192.168.1.1/clip.mp4",
            "http://172.16.0.1/clip.mp4",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/clip.mp4",
            "http://[fd00::1]/clip.mp4",
            "http://[fe80::1]/clip.mp4",
            "http://0.0.0.0/clip.mp4",
        ] {
            assert!(!safe(url), "{url} should have been refused");
        }
    }

    #[test]
    fn a_network_address_is_fetched_when_that_was_asked_for() {
        // Someone serving to their own household, which is the case the
        // option exists for.
        assert!(safe_when_allowed("http://192.168.1.10:8080/clip.mp4"));
        assert!(safe_when_allowed("http://127.0.0.1:9000/clip.mp4"));
        // The scheme rule is not part of the bargain.
        assert!(!safe_when_allowed("file:///etc/passwd"));
    }

    #[test]
    fn a_scheme_that_is_not_http_is_refused() {
        assert!(!safe("file:///etc/passwd"));
        assert!(!safe("ftp://videos.example/clip.mp4"));
    }

    #[test]
    fn bytes_that_do_not_match_the_manifest_are_refused() {
        let cid = ContentId::from_raw(b"the announced bytes");
        assert!(verify("u", b"the announced bytes", &cid, 19).is_ok());
        assert!(verify("u", b"something else here", &cid, 19).is_err());
        // Right hash, wrong length is impossible; wrong length alone is
        // caught before hashing.
        assert!(verify("u", b"short", &cid, 19).is_err());
    }
}
