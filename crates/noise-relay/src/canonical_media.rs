use std::{sync::Arc, time::Duration};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use reqwest::StatusCode;

const MAX_SHARD_BYTES: u64 = 2_100_000;

#[derive(Clone)]
pub struct CanonicalMediaClient {
    base_url: Arc<str>,
    provider: Arc<str>,
    token: Arc<str>,
    http: reqwest::Client,
}

impl CanonicalMediaClient {
    pub fn from_parts(
        base_url: Option<&str>,
        provider: Option<&str>,
        token: Option<&str>,
    ) -> anyhow::Result<Option<Self>> {
        if base_url.is_none() && provider.is_none() && token.is_none() {
            return Ok(None);
        }
        let base_url = base_url
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("canonical media URL is required")?;
        let provider = provider
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("canonical media provider is required")?;
        let token = token
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("canonical media token is required")?;

        let parsed = url::Url::parse(base_url).context("canonical media URL is invalid")?;
        let private_http = parsed.scheme() == "http"
            && parsed
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(is_private_transport_address);
        if (parsed.scheme() != "https" && !private_http)
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || base_url.ends_with('/')
        {
            bail!(
                "canonical media URL must be HTTPS or private loopback/Tailscale HTTP without a trailing slash"
            );
        }
        if provider.len() > 64
            || !provider.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            })
        {
            bail!("canonical media provider is invalid");
        }
        let decoded = STANDARD_NO_PAD
            .decode(token)
            .context("canonical media token is invalid")?;
        if decoded.len() != 32 || STANDARD_NO_PAD.encode(decoded) != token {
            bail!("canonical media token must be 32 bytes in canonical unpadded base64");
        }

        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .context("could not initialize canonical media client")?;
        Ok(Some(Self {
            base_url: Arc::from(base_url),
            provider: Arc::from(provider),
            token: Arc::from(token),
            http,
        }))
    }

    pub async fn get_shard(
        &self,
        shard_id: &str,
        expected_payload_hash: &str,
        expected_length: u64,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let url = format!(
            "{}/v1/providers/{}/shards/{}",
            self.base_url, self.provider, shard_id
        );
        let response = self
            .http
            .get(url)
            .bearer_auth(self.token.as_ref())
            .send()
            .await
            .context("canonical media service is unreachable")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() != StatusCode::OK {
            bail!("canonical media service rejected the read");
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_SHARD_BYTES)
        {
            bail!("canonical media response is too large");
        }
        let payload = response
            .bytes()
            .await
            .context("canonical media response could not be read")?;
        if payload.len() as u64 != expected_length
            || expected_length > MAX_SHARD_BYTES
            || blake3::hash(&payload).to_hex().as_str() != expected_payload_hash
        {
            bail!("canonical media response failed integrity verification");
        }
        Ok(Some(payload.to_vec()))
    }
}

fn is_private_transport_address(address: std::net::IpAddr) -> bool {
    if address.is_loopback() {
        return true;
    }
    let std::net::IpAddr::V4(address) = address else {
        return false;
    };
    let octets = address.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> String {
        STANDARD_NO_PAD.encode([0x42; 32])
    }

    #[test]
    fn canonical_media_transport_accepts_only_protected_addresses() {
        let token = token();
        assert!(
            CanonicalMediaClient::from_parts(
                Some("http://127.0.0.1:4302/internal/compat"),
                Some("primary"),
                Some(&token),
            )
            .unwrap()
            .is_some()
        );
        assert!(
            CanonicalMediaClient::from_parts(
                Some("http://100.99.59.98:4303/internal/compat"),
                Some("secondary"),
                Some(&token),
            )
            .unwrap()
            .is_some()
        );
        assert!(
            CanonicalMediaClient::from_parts(
                Some("http://203.0.113.1/internal/compat"),
                Some("secondary"),
                Some(&token),
            )
            .is_err()
        );
    }
}
