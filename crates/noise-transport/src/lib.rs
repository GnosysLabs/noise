use std::io::Cursor;

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bhttp::{Message, Mode, StatusCode};
use url::{Host, Url};

pub const OHTTP_KEYS_MEDIA_TYPE: &str = "application/ohttp-keys";
pub const OHTTP_REQUEST_MEDIA_TYPE: &str = "message/ohttp-req";
pub const OHTTP_RESPONSE_MEDIA_TYPE: &str = "message/ohttp-res";
pub const GATEWAY_HEADER: &str = "noise-gateway";
pub const OHTTP_GATEWAY_PATH: &str = "/v1/ohttp/gateway";
pub const OHTTP_KEYS_PATH: &str = "/v1/ohttp-keys";
pub const OHTTP_RELAY_PATH: &str = "/v1/ohttp/relay";

const PAD_BUCKETS: &[usize] = &[
    1024,
    4096,
    16 * 1024,
    64 * 1024,
    256 * 1024,
    512 * 1024,
    1024 * 1024,
    2_500_000,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayDescriptor {
    pub base_url: String,
    pub ohttp_config: Option<Vec<u8>>,
}

impl RelayDescriptor {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let value = value.trim();
        if value.is_empty() {
            bail!("relay address is empty")
        }
        let mut url = Url::parse(value).context("relay address is invalid")?;
        if !url.username().is_empty() || url.password().is_some() {
            bail!("relay addresses cannot contain credentials")
        }
        if url.query().is_some() || !matches!(url.path(), "" | "/") {
            bail!("relay addresses cannot contain a path or query")
        }
        let local = match url.host() {
            Some(Host::Domain("localhost")) => true,
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            _ => false,
        };
        if url.scheme() != "https" && !(url.scheme() == "http" && local) {
            bail!("relay addresses must use HTTPS outside local development")
        }
        let ohttp_config = url
            .fragment()
            .map(|fragment| {
                let encoded = fragment
                    .strip_prefix("ohttp=")
                    .context("relay fragment is not an OHTTP key")?;
                let decoded = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .context("relay OHTTP key is invalid")?;
                if decoded.is_empty() {
                    bail!("relay OHTTP key is empty")
                }
                Ok(decoded)
            })
            .transpose()?;
        url.set_fragment(None);
        let base_url = url.as_str().trim_end_matches('/').to_owned();
        Ok(Self {
            base_url,
            ohttp_config,
        })
    }

    #[must_use]
    pub fn shareable(base_url: &str, ohttp_config: &[u8]) -> String {
        format!(
            "{}#ohttp={}",
            base_url.trim_end_matches('/'),
            URL_SAFE_NO_PAD.encode(ohttp_config)
        )
    }
}

#[derive(Debug)]
pub struct PlainRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct PlainResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub fn encode_request(
    method: &str,
    scheme: &str,
    authority: &str,
    path: &str,
    body: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut message = Message::request(
        method.as_bytes().to_vec(),
        scheme.as_bytes().to_vec(),
        authority.as_bytes().to_vec(),
        path.as_bytes().to_vec(),
    );
    if !body.is_empty() {
        message.put_header("content-type", "application/json");
        message.write_content(body);
    }
    encode_padded(&message)
}

pub fn decode_request(bytes: &[u8]) -> anyhow::Result<PlainRequest> {
    let message = Message::read_bhttp(&mut Cursor::new(bytes)).context("invalid binary request")?;
    let method = message
        .control()
        .method()
        .context("binary message is not a request")?;
    let path = message
        .control()
        .path()
        .context("binary request has no path")?;
    Ok(PlainRequest {
        method: String::from_utf8(method.to_vec()).context("request method is not UTF-8")?,
        path: String::from_utf8(path.to_vec()).context("request path is not UTF-8")?,
        body: message.content().to_vec(),
    })
}

pub fn encode_response(status: u16, body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut message =
        Message::response(StatusCode::try_from(status).context("invalid response status")?);
    if !body.is_empty() {
        message.put_header("content-type", "application/json");
        message.write_content(body);
    }
    encode_padded(&message)
}

pub fn decode_response(bytes: &[u8]) -> anyhow::Result<PlainResponse> {
    let message =
        Message::read_bhttp(&mut Cursor::new(bytes)).context("invalid binary response")?;
    let status = message
        .control()
        .status()
        .context("binary message is not a response")?
        .code();
    Ok(PlainResponse {
        status,
        body: message.content().to_vec(),
    })
}

fn encode_padded(message: &Message) -> anyhow::Result<Vec<u8>> {
    let mut encoded = Vec::new();
    message
        .write_bhttp(Mode::KnownLength, &mut encoded)
        .context("could not encode binary HTTP")?;
    let Some(bucket) = PAD_BUCKETS
        .iter()
        .copied()
        .find(|bucket| *bucket >= encoded.len())
    else {
        bail!("oblivious message exceeds the maximum padded size")
    };
    encoded.resize(bucket, 0);
    Ok(encoded)
}
