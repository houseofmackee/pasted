//! Link previews: page title, description, image and icon for link clips.
//!
//! Fetching is the only network access Pasted does, and it can be turned off
//! in settings. Only public http(s) URLs are fetched: hosts that are local,
//! private or resolve to private addresses are skipped, redirects are checked
//! hop by hop, and every response is size and time limited.

use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use url::Url;

use crate::store::Store;

const HTML_LIMIT: u64 = 512 * 1024;
const IMAGE_LIMIT: u64 = 2 * 1024 * 1024;
const ICON_LIMIT: u64 = 256 * 1024;
const MAX_REDIRECTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(8);
const BATCH: usize = 6;
const IDLE: Duration = Duration::from_secs(20);
const USER_AGENT: &str = concat!("Mozilla/5.0 (Macintosh) Pasted/", env!("CARGO_PKG_VERSION"), " (link preview)");

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinkPreview {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<Vec<u8>>,
    pub icon: Option<Vec<u8>>,
}

/// Metadata found in a page's HTML.
#[derive(Debug, Default, PartialEq)]
pub struct PageMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<Url>,
    pub icon_url: Option<Url>,
}

static META_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<meta\s[^>]*>").unwrap());
static LINK_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<link\s[^>]*>").unwrap());
static ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)([a-zA-Z:_-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#).unwrap());
static TITLE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap());
static ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").unwrap());

fn attrs(tag: &str) -> Vec<(String, String)> {
    ATTR.captures_iter(tag)
        .map(|c| {
            let value = c.get(2).or(c.get(3)).or(c.get(4)).map(|m| m.as_str()).unwrap_or("");
            (c[1].to_ascii_lowercase(), decode_entities(value))
        })
        .collect()
}

pub fn decode_entities(s: &str) -> String {
    ENTITY
        .replace_all(s, |c: &regex::Captures| {
            let e = &c[1];
            let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = e.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match e {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    "ndash" | "mdash" => Some('-'),
                    "hellip" => Some('\u{2026}'),
                    _ => None,
                }
            };
            ch.map(String::from).unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

fn clean(s: &str, max: usize) -> Option<String> {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    (!s.is_empty()).then(|| crate::model::truncate_chars(&s, max))
}

pub fn parse_html(html: &str, base: &Url) -> PageMeta {
    let mut meta = PageMeta::default();

    // Lower rank wins: Open Graph first, then Twitter, then plain HTML.
    let mut title: Option<(usize, String)> = None;
    let mut description: Option<(usize, String)> = None;
    let mut image: Option<(usize, String)> = None;
    let consider = |slot: &mut Option<(usize, String)>, rank: usize, value: String| {
        if !value.trim().is_empty() && slot.as_ref().is_none_or(|(r, _)| rank < *r) {
            *slot = Some((rank, value));
        }
    };
    for tag in META_TAG.find_iter(html) {
        let a = attrs(tag.as_str());
        let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let key = get("property").or_else(|| get("name")).unwrap_or_default().to_ascii_lowercase();
        let Some(content) = get("content") else { continue };
        match key.as_str() {
            "og:title" => consider(&mut title, 0, content),
            "twitter:title" => consider(&mut title, 1, content),
            "og:description" => consider(&mut description, 0, content),
            "twitter:description" => consider(&mut description, 1, content),
            "description" => consider(&mut description, 2, content),
            "og:image:secure_url" | "og:image:url" | "og:image" => consider(&mut image, 0, content),
            "twitter:image" | "twitter:image:src" => consider(&mut image, 1, content),
            _ => {}
        }
    }
    if let Some(c) = TITLE_TAG.captures(html) {
        consider(&mut title, 2, decode_entities(&c[1]));
    }

    // Prefer a large touch icon, then any icon, then /favicon.ico.
    let mut icon: Option<(usize, String)> = None;
    for tag in LINK_TAG.find_iter(html) {
        let a = attrs(tag.as_str());
        let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let (Some(rel), Some(href)) = (get("rel"), get("href")) else { continue };
        let rel = rel.to_ascii_lowercase();
        let rank = if rel.contains("apple-touch-icon") {
            0
        } else if rel.split_whitespace().any(|r| r == "icon") {
            1
        } else {
            continue;
        };
        if href.ends_with(".svg") {
            continue;
        }
        consider(&mut icon, rank, href);
    }

    meta.title = title.and_then(|(_, t)| clean(&t, 300));
    meta.description = description.and_then(|(_, d)| clean(&d, 500));
    meta.image_url = image.and_then(|(_, u)| base.join(u.trim()).ok()).filter(|u| u.scheme().starts_with("http"));
    meta.icon_url = icon.and_then(|(_, u)| base.join(u.trim()).ok()).or_else(|| base.join("/favicon.ico").ok());
    meta
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| is_private_ip(IpAddr::V4(v4)))
        }
    }
}

/// Cheap checks that need no network: scheme, credentials and host name.
pub fn is_fetchable(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d.contains('.')
                && !["localhost", ".local", ".internal", ".lan", ".home.arpa", ".localdomain"]
                    .iter()
                    .any(|s| d == s.trim_start_matches('.') || d.ends_with(s))
        }
        Some(url::Host::Ipv4(ip)) => !is_private_ip(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => !is_private_ip(IpAddr::V6(ip)),
        None => false,
    }
}

/// Resolves the host and rejects it if any address is private.
fn check_public(url: &Url) -> Result<()> {
    if !is_fetchable(url) {
        bail!("not a public http(s) URL");
    }
    let host = url.host_str().context("no host")?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<_> = (host.trim_matches(|c| c == '[' || c == ']'), port).to_socket_addrs()?.collect();
    if addrs.is_empty() || addrs.iter().any(|a| is_private_ip(a.ip())) {
        bail!("{host} resolves to a private address");
    }
    Ok(())
}

struct Fetched {
    url: Url,
    content_type: String,
    body: Vec<u8>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/// GET with manual redirects so every hop passes `check_public`.
fn get(agent: &ureq::Agent, url: &Url, limit: u64, accept: &str) -> Result<Fetched> {
    let mut url = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        check_public(&url)?;
        let mut resp = agent.get(url.as_str()).header("Accept", accept).call()?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            let location =
                resp.headers().get("location").and_then(|l| l.to_str().ok()).context("redirect without location")?;
            url = url.join(location)?;
            continue;
        }
        if !(200..300).contains(&status) {
            bail!("HTTP {status}");
        }
        let content_type =
            resp.headers().get("content-type").and_then(|c| c.to_str().ok()).unwrap_or("").to_ascii_lowercase();
        let mut body = Vec::new();
        resp.body_mut().as_reader().take(limit).read_to_end(&mut body)?;
        return Ok(Fetched { url, content_type, body });
    }
    bail!("too many redirects")
}

fn fetch_image(agent: &ureq::Agent, url: &Url, limit: u64) -> Option<Vec<u8>> {
    let f = get(agent, url, limit + 1, "image/*").ok()?;
    let ok = f.content_type.starts_with("image/") && !f.content_type.contains("svg");
    (ok && !f.body.is_empty() && (f.body.len() as u64) <= limit).then_some(f.body)
}

pub fn fetch(raw_url: &str) -> Result<LinkPreview> {
    let url = Url::parse(raw_url)?;
    let agent = agent();
    let page = get(&agent, &url, HTML_LIMIT, "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5")?;
    let mut preview = LinkPreview { url: raw_url.to_string(), ..LinkPreview::default() };
    if page.content_type.starts_with("image/") && !page.content_type.contains("svg") {
        // A direct link to an image: the image is the preview.
        if (page.body.len() as u64) < HTML_LIMIT {
            preview.image = Some(page.body);
        } else {
            preview.image = fetch_image(&agent, &page.url, IMAGE_LIMIT);
        }
        preview.title = page.url.path_segments().and_then(|mut s| s.next_back()).and_then(|n| clean(n, 300));
        return Ok(preview);
    }
    if !page.content_type.contains("html") {
        bail!("not an HTML page ({})", page.content_type);
    }
    let html = String::from_utf8_lossy(&page.body);
    let meta = parse_html(&html, &page.url);
    preview.title = meta.title;
    preview.description = meta.description;
    preview.image = meta.image_url.and_then(|u| fetch_image(&agent, &u, IMAGE_LIMIT));
    preview.icon = meta.icon_url.and_then(|u| fetch_image(&agent, &u, ICON_LIMIT));
    Ok(preview)
}

/// Background worker; `enabled` mirrors the `link_previews` setting.
pub fn spawn_worker(enabled: &'static AtomicBool) {
    std::thread::Builder::new()
        .name("pasted-links".into())
        .spawn(move || {
            let store = match Store::open_default() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("link preview worker could not open the database: {e:#}");
                    return;
                }
            };
            loop {
                if !enabled.load(Ordering::Relaxed) {
                    std::thread::sleep(IDLE);
                    continue;
                }
                let urls = store.links_needing_preview(BATCH).unwrap_or_default();
                if urls.is_empty() {
                    std::thread::sleep(IDLE);
                    continue;
                }
                for url in urls {
                    if !enabled.load(Ordering::Relaxed) {
                        break;
                    }
                    let parsed = Url::parse(&url).ok();
                    let result = match parsed {
                        Some(u) if is_fetchable(&u) => fetch(&url),
                        _ => Err(anyhow::anyhow!("skipped")),
                    };
                    let saved = match result {
                        Ok(preview) => store.set_link_preview(&preview, "ok"),
                        Err(e) => {
                            tracing::debug!("no preview for {url}: {e:#}");
                            store
                                .set_link_preview(&LinkPreview { url: url.clone(), ..LinkPreview::default() }, "failed")
                        }
                    };
                    if let Err(e) = saved {
                        tracing::warn!("saving link preview failed: {e:#}");
                    }
                }
            }
        })
        .expect("spawning the link preview thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_graph_with_fallbacks() {
        let base = Url::parse("https://example.com/articles/1").unwrap();
        let html = r#"<html><head>
            <title>  Plain &amp; Simple  </title>
            <meta name="description" content="html description">
            <meta content="OG &quot;Title&quot;" property="og:title" />
            <meta property='og:image' content='/img/cover.png'>
            <link rel="icon" href="favicon-32.png">
            <link rel="apple-touch-icon" href="https://cdn.example.com/touch.png">
            </head></html>"#;
        let m = parse_html(html, &base);
        assert_eq!(m.title.as_deref(), Some("OG \"Title\""));
        assert_eq!(m.description.as_deref(), Some("html description"));
        assert_eq!(m.image_url.unwrap().as_str(), "https://example.com/img/cover.png");
        assert_eq!(m.icon_url.unwrap().as_str(), "https://cdn.example.com/touch.png");
    }

    #[test]
    fn title_tag_and_default_icon() {
        let base = Url::parse("https://example.org/a/b").unwrap();
        let m = parse_html("<TITLE>Hello\n  World &#8212; &#x41;</TITLE>", &base);
        assert_eq!(m.title.as_deref(), Some("Hello World \u{2014} A"));
        assert_eq!(m.icon_url.unwrap().as_str(), "https://example.org/favicon.ico");
        assert!(m.image_url.is_none());
    }

    #[test]
    fn only_public_urls_are_fetchable() {
        for ok in ["https://example.com/x", "http://sub.example.co.uk", "https://93.184.216.34/"] {
            assert!(is_fetchable(&Url::parse(ok).unwrap()), "{ok}");
        }
        for bad in [
            "http://localhost:8080",
            "http://127.0.0.1/",
            "http://10.0.0.5/admin",
            "http://192.168.1.1",
            "http://172.20.0.1",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://printer.local/",
            "http://intranet/",
            "https://wiki.corp.internal/page",
            "https://user:pw@example.com/",
            "ftp://example.com/file",
            "file:///etc/passwd",
        ] {
            assert!(!is_fetchable(&Url::parse(bad).unwrap()), "{bad}");
        }
    }

    #[test]
    fn entities() {
        assert_eq!(decode_entities("a &lt;b&gt; &unknown; &#39;c&#39;"), "a <b> &unknown; 'c'");
    }
}
