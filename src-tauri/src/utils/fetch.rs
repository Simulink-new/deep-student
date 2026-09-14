use base64::Engine as _;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// ★ perf-audit A9#1/#2: 远程图片缓存——值存编码后的 base64(Arc 共享),
/// 命中路径零编码零深拷贝;带上限淘汰(旧实现存原始 bytes 无上限,
/// 且命中后每次 clone Vec + 重新 base64 编码,含图对话每轮 ~18MB 瞬时分配)
struct ImageCache {
    /// url -> (base64 已编码, mime)
    map: HashMap<String, (Arc<String>, String)>,
    /// FIFO 淘汰顺序
    order: Vec<String>,
    bytes_approx: usize,
}

const IMAGE_CACHE_MAX_ENTRIES: usize = 128;
const IMAGE_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

impl ImageCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: Vec::new(),
            bytes_approx: 0,
        }
    }

    fn get(&self, url: &str) -> Option<(Arc<String>, String)> {
        self.map.get(url).map(|(d, m)| (Arc::clone(d), m.clone()))
    }

    fn insert(&mut self, url: String, data: Arc<String>, mime: String) {
        let entry_bytes = data.len();
        if self.map.contains_key(&url) {
            return;
        }
        // 容量淘汰: FIFO 直至回到上限内
        while self.order.len() >= IMAGE_CACHE_MAX_ENTRIES
            || (self.bytes_approx + entry_bytes > IMAGE_CACHE_MAX_BYTES && !self.order.is_empty())
        {
            let evict = self.order.remove(0);
            if let Some((d, _)) = self.map.remove(&evict) {
                self.bytes_approx = self.bytes_approx.saturating_sub(d.len());
            }
        }
        if entry_bytes > IMAGE_CACHE_MAX_BYTES {
            return; // 单条超限不入缓存
        }
        self.bytes_approx += entry_bytes;
        self.order.push(url.clone());
        self.map.insert(url, (data, mime));
    }
}

static CACHE: LazyLock<Mutex<ImageCache>> = LazyLock::new(|| Mutex::new(ImageCache::new()));

fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.octets() == [255, 255, 255, 255]
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // unique local (fc00::/7)
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // link-local unicast (fe80::/10)
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // site-local (fec0::/10, deprecated but still risky)
                || (v6.segments()[0] & 0xffc0) == 0xfec0
                // IPv4-mapped IPv6 addresses
                || v6
                    .to_ipv4_mapped()
                    .map(|v4| {
                        v4.is_private()
                            || v4.is_loopback()
                            || v4.is_link_local()
                            || v4.is_unspecified()
                            || v4.is_multicast()
                            || v4.octets() == [255, 255, 255, 255]
                    })
                    .unwrap_or(false)
        }
    }
}

fn is_localhost_host(host: &str) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    normalized == "localhost" || normalized.ends_with(".localhost")
}

fn is_safe_remote_url(url: &str) -> bool {
    let parsed = match reqwest::Url::parse(url) {
        Ok(value) => value,
        Err(_) => return false,
    };

    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }

    let host = match parsed.host_str() {
        Some(value) => value,
        None => return false,
    };

    if is_localhost_host(host) {
        return false;
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return !is_blocked_ip(&ip);
    }

    let port = match parsed.port_or_known_default() {
        Some(value) => value,
        None => return false,
    };

    let resolved_addrs = match (host, port).to_socket_addrs() {
        Ok(iter) => iter.collect::<Vec<_>>(),
        Err(_) => return false,
    };

    if resolved_addrs.is_empty() {
        return false;
    }

    for addr in resolved_addrs {
        if is_blocked_ip(&addr.ip()) {
            return false;
        }
    }

    true
}

/// 取远程图片并返回 base64(缓存共享 Arc)。
///
/// 命中路径: 零网络/零 DNS/零编码/零深拷贝(旧实现命中也要 clone Vec + 重编码)。
/// 注意: 未命中路径仍为阻塞 HTTP(10s 上限,每 URL 一次),SSRF 校验含 DNS 解析——
/// 已通过「先查缓存」把每轮重复解析消掉。
pub fn fetch_base64_with_cache(url: &str) -> Option<(Arc<String>, String)> {
    // 1. 先查缓存(缓存条目均来自已过 SSRF 校验的 URL;同时跳过 is_safe_remote_url
    //    内的阻塞 DNS 解析——旧实现每轮对话对每张历史图片都重新解析一次)
    if let Some(hit) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(url)
    {
        return Some(hit);
    }

    if !is_safe_remote_url(url) {
        return None;
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(Policy::none())
        .build()
        .ok()?;

    let response = client.get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }

    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let bytes = response.bytes().ok()?;
    let data = Arc::new(base64::engine::general_purpose::STANDARD.encode(bytes.as_ref()));
    let mime = mime.unwrap_or_else(|| "application/octet-stream".to_string());
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(url.to_string(), data.clone(), mime.clone());

    Some((data, mime))
}

#[cfg(test)]
mod tests {
    use super::is_safe_remote_url;

    #[test]
    fn blocks_non_http_schemes() {
        assert!(!is_safe_remote_url("file:///etc/passwd"));
        assert!(!is_safe_remote_url("ftp://example.com/file.png"));
        assert!(!is_safe_remote_url("data:image/png;base64,AAAA"));
    }

    #[test]
    fn blocks_localhost_hosts() {
        assert!(!is_safe_remote_url("http://localhost/image.png"));
        assert!(!is_safe_remote_url("http://localhost./image.png"));
        assert!(!is_safe_remote_url("https://api.localhost/image.png"));
    }

    #[test]
    fn blocks_private_and_link_local_ipv4() {
        assert!(!is_safe_remote_url("http://127.0.0.1/image.png"));
        assert!(!is_safe_remote_url("http://10.0.0.8/image.png"));
        assert!(!is_safe_remote_url("http://172.16.5.1/image.png"));
        assert!(!is_safe_remote_url("http://192.168.1.2/image.png"));
        assert!(!is_safe_remote_url("http://169.254.169.254/image.png"));
    }

    #[test]
    fn blocks_local_ipv6_ranges() {
        assert!(!is_safe_remote_url("http://[::1]/image.png"));
        assert!(!is_safe_remote_url("http://[fc00::1]/image.png"));
        assert!(!is_safe_remote_url("http://[fe80::1]/image.png"));
    }

    #[test]
    fn allows_public_ip_literal() {
        assert!(is_safe_remote_url("https://1.1.1.1/image.png"));
    }
}
