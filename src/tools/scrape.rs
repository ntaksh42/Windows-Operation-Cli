//! `Scrape` tool: fetches a URL over HTTP(S) and converts its HTML body to
//! Markdown, with per-hop SSRF protection on manually-followed redirects.
#![allow(deprecated)] // required by docs/SPEC.md; rmcp marks MCP sampling deprecated
//!
use std::net::IpAddr;
use std::time::Duration;

use reqwest::Url;
use reqwest::redirect::Policy;
use rmcp::model::{CreateMessageRequestParams, SamplingMessage};
use rmcp::{Peer, RoleServer, schemars};
use serde::Deserialize;

use crate::params::{BoolOrString, opt_bool};
use crate::tools::snapshot::{self, SnapshotParams};

const MAX_REDIRECTS: usize = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

/// Parameters for the `Scrape` tool.
///
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScrapeParams {
    /// The URL to fetch.
    #[schemars(description = "The URL to fetch.")]
    pub url: String,
    /// Optional focus query for sampled extraction.
    pub query: Option<String>,
    /// Extract from the active browser tab's DOM instead of an HTTP request.
    #[serde(default)]
    pub use_dom: Option<BoolOrString>,
    /// Summarize the content via MCP sampling. Defaults to true.
    #[serde(default)]
    pub use_sampling: Option<BoolOrString>,
}

/// Runs the `Scrape` tool.
///
/// Returns `Ok` with the caller-facing success text, or `Err` with a
/// caller-facing error message for unexpected failures (invalid/blocked
/// URLs, connection failures) which the caller should surface as an MCP
/// `isError` result.
pub async fn scrape(
    params: ScrapeParams,
    peer: Option<&Peer<RoleServer>>,
) -> Result<String, String> {
    let use_dom = opt_bool(&params.use_dom, false)?;
    let use_sampling = opt_bool(&params.use_sampling, true)?;

    let content = if use_dom {
        let (content, found) = scrape_dom(&params.url).await?;
        if !found {
            return Ok(content);
        }
        content
    } else {
        fetch_markdown(&params.url).await?
    };

    if use_sampling
        && let Some(peer) = peer
        && let Some(sampled) =
            sample_content(peer, &params.url, params.query.as_deref(), &content).await
    {
        let source = if use_dom {
            format!("{}\n", content.lines().next().unwrap_or_default())
        } else {
            String::new()
        };
        return Ok(format!("URL: {}\n{source}Content:\n{sampled}", params.url));
    }

    Ok(format!("URL: {}\nContent:\n{content}", params.url))
}

async fn scrape_dom(url: &str) -> Result<(String, bool), String> {
    let params = SnapshotParams {
        scope: None,
        window: None,
        timeout_ms: None,
        use_vision: Some(BoolOrString::Bool(false)),
        use_dom: Some(BoolOrString::Bool(true)),
        use_annotation: Some(BoolOrString::Bool(false)),
        use_ui_tree: Some(BoolOrString::Bool(true)),
        width_reference_line: None,
        height_reference_line: None,
        display: None,
    };
    let result = tokio::task::spawn_blocking(move || snapshot::capture(&params))
        .await
        .map_err(|e| format!("DOM capture task failed: {e}"))??;
    if !result.dom_found {
        return Ok((
            format!("No DOM information found. Please open {url} in browser first."),
            false,
        ));
    }
    let text = result
        .informative_nodes
        .iter()
        .map(|node| node.name.as_str())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let header = if result.dom_scroll_percent <= 0.0 {
        "Reached top"
    } else {
        "Scroll up to see more"
    };
    let footer = if result.dom_scroll_percent >= 100.0 {
        "Reached bottom"
    } else {
        "Scroll down to see more"
    };
    let source = dom_source_notice(result.focused_window_title.as_deref());
    Ok((format!("{source}\n{header}\n{text}\n{footer}"), true))
}

fn dom_source_notice(title: Option<&str>) -> String {
    format!("Source: foreground window {:?} (DOM; requested URL not verified)", title.unwrap_or("unknown"))
}

async fn sample_content(
    peer: &Peer<RoleServer>,
    url: &str,
    query: Option<&str>,
    content: &str,
) -> Option<String> {
    if peer
        .peer_info()
        .is_none_or(|info| info.capabilities.sampling.is_none())
    {
        return None;
    }
    let focus = query
        .map(|query| format!(" Focus specifically on: {query}."))
        .unwrap_or_default();
    let system_prompt = format!(
        "You are a web content extractor. Given raw webpage content, extract and present only the meaningful information in clean, concise prose or structured format. Strip out navigation menus, cookie banners, ads, footer links, and all other boilerplate. Preserve important data, facts, and structure.{focus}"
    );
    let request = CreateMessageRequestParams::new(
        vec![SamplingMessage::user_text(format!(
            "Raw scraped content from {url}:\n\n{content}"
        ))],
        2048,
    )
    .with_system_prompt(system_prompt);
    let result = peer.create_message(request).await.ok()?;
    let text = result
        .message
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|text| text.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

async fn fetch_markdown(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("Error: Failed to build HTTP client: {e}"))?;

    let mut current = url.to_string();
    let mut response = None;
    for redirect_count in 0..=MAX_REDIRECTS {
        validate_url(&current).await?;

        let resp = client
            .get(&current)
            .send()
            .await
            .map_err(|e| format!("Error: Failed to connect to {current}: {e}"))?;

        if resp.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err("Error: Too many redirects while fetching URL".to_string());
            }
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("Error: Redirect from {current} has no Location header"))?;
            let base =
                Url::parse(&current).map_err(|e| format!("Error: Invalid URL: {current}: {e}"))?;
            let next = base.join(location).map_err(|e| {
                format!("Error: Invalid redirect location from {current}: {location}: {e}")
            })?;
            current = next.to_string();
            continue;
        }

        response = Some(resp);
        break;
    }

    let mut response =
        response.ok_or_else(|| "Error: Too many redirects while fetching URL".to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("Error: HTTP error for {current}: {status}"));
    }

    if response.content_length().is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return Err("Error: Response body exceeds the 5 MiB limit".to_string());
    }
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok()).unwrap_or("").to_string();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await
        .map_err(|e| format!("Error: Failed to read response body from {current}: {e}"))? {
        if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err("Error: Response body exceeds the 5 MiB limit".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    let encoding = content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.eq_ignore_ascii_case("charset").then(|| encoding_rs::Encoding::for_label(value.trim().trim_matches(['\'', '"']).as_bytes())).flatten()
    }).unwrap_or(encoding_rs::UTF_8);
    let html = encoding.decode(&bytes).0;
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style"])
        .build();
    converter
        .convert(&html)
        .map_err(|e| format!("Error: Failed to convert HTML to Markdown: {e}"))
}

/// Validates a URL is safe to fetch (SSRF protection): only http/https,
/// no embedded credentials, and none of the resolved addresses are
/// private, loopback, link-local, multicast, reserved, or unspecified.
async fn validate_url(url: &str) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|_| format!("Error: Invalid URL: {url}"))?;

    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "Error: URL scheme '{}' is not allowed; use http or https.",
            parsed.scheme()
        ));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| format!("Error: URL has no hostname: {url}"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Error: URLs with embedded credentials are not allowed.".to_string());
    }

    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("Error: Could not resolve hostname '{host}': {e}"))?;

    let mut any = false;
    for addr in addrs {
        any = true;
        let ip = addr.ip();
        if is_blocked_ip(ip) {
            return Err(format!(
                "Error: Private, loopback, link-local, multicast, and reserved addresses are blocked: {ip}"
            ));
        }
    }
    if !any {
        return Err(format!(
            "Error: Could not resolve hostname '{host}': no addresses returned"
        ));
    }

    Ok(())
}

/// Whether `ip` is a private, loopback, link-local, multicast, reserved, or
/// unspecified address that must not be reachable from `Scrape` (SSRF guard).
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || octets[0] == 0
                || octets[0] >= 240
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && matches!(octets[1..], [0, 0, _] | [0, 2, _] | [88, 99, _]))
                || (octets[0] == 198
                    && (octets[1] == 18 || octets[1] == 19 || octets[1] == 51 && octets[2] == 100))
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        }
        IpAddr::V6(v6) => {
            v6.to_ipv4_mapped()
                .is_some_and(|v4| is_blocked_ip(IpAddr::V4(v4)))
                || v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7: unique local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10: link-local
                || (v6.segments()[0] == 0x2001 && v6.segments()[1] == 0x0db8)
                || (v6.segments()[0] == 0x0100
                    && v6.segments()[1..].iter().all(|segment| *segment == 0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn dom_source_does_not_claim_the_requested_url_was_verified() {
        let notice = dom_source_notice(Some("Browser title"));
        assert!(notice.contains("Browser title"));
        assert!(notice.contains("requested URL not verified"));
    }

    #[test]
    fn blocks_private_ipv4() {
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1))));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
    }

    #[test]
    fn allows_public_ipv4() {
        assert!(!is_blocked_ip(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)))); // example.com
        assert!(!is_blocked_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn blocks_private_ipv6() {
        assert!(is_blocked_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_blocked_ip(IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
        assert!(is_blocked_ip(IpAddr::V6(
            "::ffff:127.0.0.1".parse().unwrap()
        )));
        assert!(is_blocked_ip(IpAddr::V6("2001:db8::1".parse().unwrap())));
    }

    #[test]
    fn blocks_reserved_ipv4() {
        for address in [
            "100.64.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
        ] {
            assert!(
                is_blocked_ip(IpAddr::V4(address.parse().unwrap())),
                "{address}"
            );
        }
    }
}
