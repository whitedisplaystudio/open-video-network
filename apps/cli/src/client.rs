//! Talking to a running node over its local HTTP API.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use ovn_node::{NodeConfig, RuntimeInfo};

/// A client for the node whose data directory is `data_dir`.
#[derive(Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    /// Connect to the node described by `runtime.json` in the data directory.
    pub fn connect(data_dir: &Path) -> Result<Self> {
        let runtime_path = NodeConfig::new(data_dir).runtime_path();
        let info = RuntimeInfo::read(&runtime_path).ok_or_else(|| {
            anyhow!(
                "no node is running for {}\n\nStart one with:\n    ourvideo start",
                data_dir.display()
            )
        })?;
        if info.api_url.is_empty() {
            return Err(anyhow!(
                "the running node has its local API disabled, so the CLI cannot reach it"
            ));
        }
        Ok(Self {
            http: reqwest::Client::builder()
                // Fetching a video can take a while; the API call blocks for
                // the whole transfer.
                .timeout(std::time::Duration::from_secs(3600))
                .build()
                .context("building the HTTP client")?,
            base: info.api_url,
            token: info.api_token,
        })
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("talking to the node at {}", self.base))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text)
                .with_context(|| format!("the node replied with something unexpected: {text}"));
        }
        // The node reports its own errors in a readable form; surface that
        // rather than an HTTP status the user has to look up.
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| text.clone());
        Err(anyhow!("{message}"))
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.send(self.http.get(format!("{}{path}", self.base)))
            .await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.send(self.http.post(format!("{}{path}", self.base)).json(&body))
            .await
    }

    pub async fn delete(&self, path: &str) -> Result<Value> {
        self.send(self.http.delete(format!("{}{path}", self.base)))
            .await
    }
}

/// The data directory a command should use: the flag, then the environment,
/// then the platform default.
pub fn resolve_data_dir(flag: Option<PathBuf>) -> PathBuf {
    flag.or_else(|| std::env::var_os("OURVIDEO_DATA_DIR").map(PathBuf::from))
        .unwrap_or_else(NodeConfig::default_data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_directory_wins() {
        let chosen = resolve_data_dir(Some(PathBuf::from("/tmp/explicit")));
        assert_eq!(chosen, PathBuf::from("/tmp/explicit"));
    }

    #[test]
    fn connecting_without_a_running_node_explains_how_to_start_one() {
        let dir = std::env::temp_dir().join("ovn-cli-no-such-node");
        let err = Client::connect(&dir).unwrap_err().to_string();
        assert!(err.contains("ourvideo start"), "{err}");
    }
}
