// Fetches link metadata without allowing the preview endpoint to reach local
// networks. Each request is pinned to a DNS answer that was checked first.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, codecs::jpeg::JpegEncoder, imageops::FilterType};
use noise_transport::{LinkPreview, LinkPreviewRequest};
use reqwest::{
    StatusCode, Url,
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use scraper::{Html, Selector};
use tokio::net::lookup_host;

const MAX_URL_BYTES: usize = 2_048;
const MAX_HTML_BYTES: usize = 512 * 1_024;
const MAX_IMAGE_SOURCE_BYTES: usize = 5 * 1_024 * 1_024;
const MAX_IMAGE_PREVIEW_BYTES: usize = 512 * 1_024;
const MAX_REDIRECTS: usize = 4;
const PREVIEW_IMAGE_SIZES: &[(u32, u32, u8)] = &[(840, 480, 82), (640, 366, 76), (480, 274, 70)];

struct FetchedBytes {
    url: Url,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

pub async fn fetch(request: &LinkPreviewRequest) -> anyhow::Result<Option<LinkPreview>> {
    if request.url.len() > MAX_URL_BYTES {
        bail!("link preview URL is too large")
    }
    let requested_url = parse_public_url(&request.url)?;
    let document = fetch_bytes(
        requested_url.clone(),
        "text/html,application/xhtml+xml;q=0.9",
        MAX_HTML_BYTES,
    )
    .await?;
    if document
        .content_type
        .as_deref()
        .is_some_and(|content_type| {
            content_type != "text/html" && content_type != "application/xhtml+xml"
        })
    {
        return Ok(None);
    }

    let (title, description, site_name, image) = {
        let source = String::from_utf8_lossy(&document.bytes);
        let html = Html::parse_document(&source);
        let meta_selector = Selector::parse("meta").expect("the meta selector is valid");
        let title_selector = Selector::parse("title").expect("the title selector is valid");
        let mut title = None;
        let mut description = None;
        let mut site_name = None;
        let mut image = None;

        for element in html.select(&meta_selector) {
            let attributes = element.value();
            let key = attributes
                .attr("property")
                .or_else(|| attributes.attr("name"))
                .unwrap_or("")
                .to_ascii_lowercase();
            let Some(content) = attributes.attr("content") else {
                continue;
            };
            match key.as_str() {
                "og:title" | "twitter:title" if title.is_none() => {
                    title = clean_text(content, 200);
                }
                "og:description" | "twitter:description" | "description"
                    if description.is_none() =>
                {
                    description = clean_text(content, 500);
                }
                "og:site_name" if site_name.is_none() => {
                    site_name = clean_text(content, 100);
                }
                "og:image" | "og:image:url" | "twitter:image" if image.is_none() => {
                    image = clean_text(content, MAX_URL_BYTES);
                }
                _ => {}
            }
        }

        if title.is_none() {
            title = html
                .select(&title_selector)
                .next()
                .and_then(|element| clean_text(&element.text().collect::<Vec<_>>().join(" "), 200));
        }
        (title, description, site_name, image)
    };
    let Some(title) = title else {
        return Ok(None);
    };

    let image_url = image.and_then(|image| {
        document
            .url
            .join(&image)
            .ok()
            .and_then(|url| parse_public_url(url.as_str()).ok())
    });
    let image_data_url = if let Some(image_url) = image_url {
        fetch_image(image_url).await.unwrap_or(None)
    } else {
        None
    };

    Ok(Some(LinkPreview {
        url: requested_url.to_string(),
        title,
        description,
        site_name,
        image_data_url,
    }))
}

async fn fetch_image(url: Url) -> anyhow::Result<Option<String>> {
    let image = fetch_bytes(
        url,
        "image/webp,image/png,image/jpeg,image/gif",
        MAX_IMAGE_SOURCE_BYTES,
    )
    .await?;
    let Some(content_type) = image.content_type else {
        return Ok(None);
    };
    let format = match content_type.as_str() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => return Ok(None),
    };
    let decoded = image::load_from_memory_with_format(&image.bytes, format)
        .context("link preview image could not be decoded")?;
    let Some(encoded) = encode_preview_image(decoded)? else {
        return Ok(None);
    };
    Ok(Some(format!(
        "data:image/jpeg;base64,{}",
        STANDARD.encode(encoded)
    )))
}

fn encode_preview_image(image: DynamicImage) -> anyhow::Result<Option<Vec<u8>>> {
    for &(width, height, quality) in PREVIEW_IMAGE_SIZES {
        let resized = image.resize(width, height, FilterType::Lanczos3);
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(&resized)
            .context("link preview image could not be encoded")?;
        if bytes.len() <= MAX_IMAGE_PREVIEW_BYTES {
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

async fn fetch_bytes(
    mut url: Url,
    accept: &'static str,
    limit: usize,
) -> anyhow::Result<FetchedBytes> {
    for redirect in 0..=MAX_REDIRECTS {
        let client = public_client_for(&url).await?;
        let mut response = client
            .get(url.clone())
            .header(ACCEPT, accept)
            .send()
            .await?;
        if response.status().is_redirection() {
            if redirect == MAX_REDIRECTS {
                bail!("link preview redirected too many times")
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .context("link preview redirect has no valid location")?;
            url = parse_public_url(url.join(location)?.as_str())?;
            continue;
        }
        if response.status() != StatusCode::OK {
            bail!("link preview returned a non-success response")
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > limit)
        {
            bail!("link preview response is too large")
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(|value| value.trim().to_ascii_lowercase());
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > limit {
                bail!("link preview response is too large")
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(FetchedBytes {
            url,
            content_type,
            bytes,
        });
    }
    unreachable!("the redirect loop always returns or fails")
}

async fn public_client_for(url: &Url) -> anyhow::Result<reqwest::Client> {
    let host = url.host_str().context("link preview URL has no host")?;
    let port = url
        .port_or_known_default()
        .context("link preview URL has no port")?;
    let mut builder = reqwest::Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(10))
        .user_agent("noise-Link-Preview/1.0");

    match url.host().context("link preview URL has no host")? {
        url::Host::Ipv4(address) => require_public_ip(IpAddr::V4(address))?,
        url::Host::Ipv6(address) => require_public_ip(IpAddr::V6(address))?,
        url::Host::Domain(domain) => {
            let domain = domain.to_ascii_lowercase();
            if domain == "localhost"
                || domain.ends_with(".localhost")
                || domain.ends_with(".local")
                || domain.ends_with(".internal")
                || domain.ends_with(".home.arpa")
            {
                bail!("link preview host is not public")
            }
            let addresses = lookup_host((host, port))
                .await
                .context("link preview host could not be resolved")?
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                bail!("link preview host could not be resolved")
            }
            for address in &addresses {
                require_public_ip(address.ip())?;
            }
            builder = builder.resolve(host, SocketAddr::new(addresses[0].ip(), port));
        }
    }

    builder.build().context("link preview client is invalid")
}

fn parse_public_url(value: &str) -> anyhow::Result<Url> {
    let url = Url::parse(value).context("link preview URL is invalid")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("link preview URL is not public HTTP")
    }
    Ok(url)
}

fn require_public_ip(address: IpAddr) -> anyhow::Result<()> {
    let public = match address {
        IpAddr::V4(address) => ipv4_is_public(address),
        IpAddr::V6(address) => ipv6_is_public(address),
    };
    if !public {
        bail!("link preview address is not public")
    }
    Ok(())
}

fn ipv4_is_public(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn ipv6_is_public(address: Ipv6Addr) -> bool {
    if let Some(address) = address.to_ipv4() {
        return ipv4_is_public(address);
    }
    let segments = address.segments();
    !address.is_unspecified()
        && !address.is_loopback()
        && segments[0] & 0xe000 == 0x2000
        && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
}

fn clean_text(value: &str, limit: usize) -> Option<String> {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = value.chars().take(limit).collect::<String>();
    (!value.is_empty()).then_some(value)
}
