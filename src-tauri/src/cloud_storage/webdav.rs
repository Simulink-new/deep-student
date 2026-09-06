//! WebDAV 存储实现
//!
//! 基于 reqwest 的 WebDAV 客户端，支持坚果云、Nextcloud 等服务

use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::{Client, Method, StatusCode, Url};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use super::config::WebDavConfig;
use super::traits::{
    CloudStorage, DownloadProgressCallback, FileInfo, Result, UploadProgressCallback,
};
use crate::backup_common::calculate_file_hash;
use crate::models::AppError;

/// reqwest 发送错误的完整诊断链。
///
/// `reqwest::Error` 的 Display 只有顶层一句 "error sending request for url (...)"，
/// 真正的原因（TLS 握手失败 / 连接被重置 / 超时 / connection closed before message
/// completed）都在 source 链里。不展开会让用户只看到一句无法定位的报错。
fn reqwest_error_detail(e: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        msg.push_str(&format!(" -> {s}"));
        src = s.source();
    }
    if e.is_timeout() {
        msg.push_str(" [超时]");
    }
    if e.is_connect() {
        msg.push_str(" [连接建立失败]");
    }
    msg
}

/// 同一 provider（host|username）的限流滑窗，在所有 storage 实例间共享：
/// 多个同步任务/会话各自创建 WebDavStorage 实例时，仍共同受坚果云请求上限约束。
type RateWindow = Arc<std::sync::Mutex<std::collections::VecDeque<std::time::Instant>>>;
static PROVIDER_RATE_WINDOWS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, RateWindow>>,
> = std::sync::OnceLock::new();

/// WebDAV 存储实现
pub struct WebDavStorage {
    base_url: Url,
    username: String,
    password: String,
    root: String,
    http: Client,
    /// 已确认存在的目录缓存（实例生命周期内有效）。
    /// 逐文件上传时避免每个文件都重发整串 MKCOL——数千个小文件会多发上万次请求，
    /// 直接触发坚果云的 WebDAV 频率限制（503）。
    ensured_dirs: std::sync::Mutex<std::collections::HashSet<String>>,
    /// 频率限制预算（每 30 分钟请求数；0 = 不限速，见 `resolve_rate_limit`）
    rate_limit_per_30min: u32,
    /// 最近 30 分钟已发请求的时间戳（滑窗节流，见 `acquire_request_slot`）。
    /// 按 provider 跨实例共享（见 `PROVIDER_RATE_WINDOWS`）。
    rate_window: RateWindow,
}

impl WebDavStorage {
    /// 创建 WebDAV 存储实例
    pub fn new(config: WebDavConfig, root: String) -> Result<Self> {
        if config.endpoint.trim().is_empty() {
            return Err(AppError::validation("WebDAV endpoint 不能为空"));
        }

        let url = Url::parse(config.endpoint.trim())
            .map_err(|e| AppError::configuration(format!("无效的 WebDAV endpoint: {e}")))?;

        let is_local = url
            .host_str()
            .map(|h| matches!(h.to_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1"))
            .unwrap_or(false);
        if !is_local && url.scheme() != "https" {
            return Err(AppError::configuration(
                "WebDAV endpoint 必须使用 HTTPS 以保护 Basic Auth 凭据（仅 localhost 允许 HTTP）"
                    .to_string(),
            ));
        }

        let http = Client::builder()
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(30))
            .min_tls_version(reqwest::tls::Version::TLS_1_2)
            .user_agent(concat!("deep-student/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| AppError::internal(format!("构建 HTTP 客户端失败: {e}")))?;

        let rate_limit_per_30min = Self::resolve_rate_limit(url.host_str().unwrap_or_default());

        // 共享同一 provider 的限流窗口：多个同步任务/会话创建不同 storage
        // 实例时仍共同受坚果云请求上限约束。
        let provider_key = format!(
            "{}|{}",
            url.host_str().unwrap_or_default().to_lowercase(),
            config.username
        );
        let windows = PROVIDER_RATE_WINDOWS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let rate_window = {
            let mut all = windows.lock().unwrap_or_else(|p| p.into_inner());
            all.entry(provider_key)
                .or_insert_with(|| {
                    Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()))
                })
                .clone()
        };

        Ok(Self {
            base_url: url,
            username: config.username,
            password: config.password,
            root: root.trim_matches('/').to_string(),
            http,
            ensured_dirs: std::sync::Mutex::new(std::collections::HashSet::new()),
            rate_limit_per_30min,
            rate_window,
        })
    }

    /// 构建 Basic 认证头
    fn auth_header(&self) -> String {
        let raw = format!("{}:{}", self.username, self.password);
        format!("Basic {}", general_purpose::STANDARD.encode(raw))
    }

    /// 将相对 key 组合成 WebDAV 根目录下的远程路径
    fn remote_path(&self, key: &str) -> String {
        Self::join_paths(&self.root, key)
    }

    /// 拼接两个路径片段，去掉首尾斜杠，避免重复分隔符
    fn join_paths(base: &str, child: &str) -> String {
        let base = base.trim_matches('/');
        let preserve_trailing_slash = child.ends_with('/') && !child.trim_matches('/').is_empty();
        let child = child.trim_matches('/');

        match (base.is_empty(), child.is_empty()) {
            (true, true) => String::new(),
            (true, false) => {
                if preserve_trailing_slash {
                    format!("{child}/")
                } else {
                    child.to_string()
                }
            }
            (false, true) => base.to_string(),
            (false, false) => {
                if preserve_trailing_slash {
                    format!("{base}/{child}/")
                } else {
                    format!("{base}/{child}")
                }
            }
        }
    }

    /// 构建完整 URL
    fn build_path_url(&self, path: &str) -> Result<Url> {
        let mut url = self.base_url.clone();
        let base_segments = url
            .path()
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let remote_segments = path
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let preserve_trailing_slash = path.ends_with('/') && !remote_segments.is_empty();

        url.set_path("");
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                AppError::configuration("WebDAV endpoint 不能作为层级路径处理".to_string())
            })?;
            for segment in base_segments.iter().chain(remote_segments.iter()) {
                segments.push(segment);
            }
            if preserve_trailing_slash {
                segments.push("");
            }
        }
        Ok(url)
    }

    /// 构建完整 URL
    fn build_url(&self, key: &str) -> Result<Url> {
        self.build_path_url(&self.remote_path(key))
    }

    fn mkcol_method() -> Result<Method> {
        Method::from_bytes(b"MKCOL")
            .map_err(|e| AppError::internal(format!("无效 WebDAV 方法 MKCOL: {e}")))
    }

    fn propfind_method() -> Result<Method> {
        Method::from_bytes(b"PROPFIND")
            .map_err(|e| AppError::internal(format!("无效 WebDAV 方法 PROPFIND: {e}")))
    }

    /// 发送 HTTP 请求并带有限重试（应对连接复用竞争、短暂网络抖动等瞬时发送错误）。
    ///
    /// `build` 在每次尝试时重新构造 RequestBuilder，因此 body 必须是可重建的内容。
    /// 发送阶段的错误意味着没有收到完整响应；PUT/DELETE/PROPFIND 对同一 key 重发是幂等的。
    ///
    /// 已收到响应但状态为 429/503（坚果云 WebDAV 频率限制）时同样重试，
    /// 优先服从 Retry-After 响应头，否则指数退避（2s/8s/32s，上限 60s）。
    async fn send_with_retry(
        &self,
        desc: &str,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        const MAX_ATTEMPTS: u32 = 5;
        let mut last_detail = String::new();

        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * (1 << attempt.min(7)));
                tracing::debug!("WebDAV {} 重试 {}/{}", desc, attempt + 1, MAX_ATTEMPTS);
                tokio::time::sleep(delay).await;
            }
            self.acquire_request_slot(desc).await;
            match build().send().await {
                Ok(resp) => {
                    if Self::is_rate_limited(resp.status()) && attempt + 1 < MAX_ATTEMPTS {
                        let delay = Self::rate_limit_delay(&resp, attempt);
                        tracing::warn!(
                            "WebDAV {} 被限流（{}），{:?} 后重试 {}/{}",
                            desc,
                            resp.status(),
                            delay,
                            attempt + 2,
                            MAX_ATTEMPTS
                        );
                        super::traits::report_status(&format!(
                            "云端限流（{}），{} 秒后自动重试（第 {}/{} 次）…",
                            resp.status().as_u16(),
                            delay.as_secs(),
                            attempt + 2,
                            MAX_ATTEMPTS
                        ));
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    last_detail = reqwest_error_detail(&e);
                    tracing::warn!(
                        "WebDAV {} 第 {} 次发送失败: {}",
                        desc,
                        attempt + 1,
                        last_detail
                    );
                    if attempt + 1 < MAX_ATTEMPTS {
                        super::traits::report_status(&format!(
                            "网络波动，{} 稍后自动重试（第 {}/{} 次）…",
                            desc,
                            attempt + 2,
                            MAX_ATTEMPTS
                        ));
                    }
                }
            }
        }

        Err(AppError::network(format!(
            "WebDAV {desc} 请求失败（已重试 {MAX_ATTEMPTS} 次）: {last_detail}"
        )))
    }

    /// 是否为频率限制/服务不可用状态（坚果云限流返回 503）
    fn is_rate_limited(status: StatusCode) -> bool {
        matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        )
    }

    /// 限流退避时长：优先 Retry-After 响应头，否则指数退避（2s/8s/32s），上限 60s
    fn rate_limit_delay(resp: &reqwest::Response, attempt: u32) -> Duration {
        if let Some(v) = resp.headers().get(reqwest::header::RETRY_AFTER) {
            if let Ok(s) = v.to_str() {
                if let Ok(secs) = s.trim().parse::<u64>() {
                    return Duration::from_secs(secs.min(60));
                }
            }
        }
        Duration::from_secs((2u64 << (attempt * 2).min(4)).min(60))
    }

    /// 滑窗时长：与坚果云官方限制口径一致（30 分钟）
    const RATE_WINDOW: Duration = Duration::from_secs(30 * 60);

    /// 解析频率限制预算（每 30 分钟请求数）。
    ///
    /// 坚果云官方限制：免费版 600 次/30 分钟、付费版 1500 次/30 分钟，超限后被
    /// 拒绝服务约 1 小时。为避免撞墙，对坚果云域名默认按免费版 90% 预算（540）
    /// 主动节流；其他 WebDAV 服务商不限速。可用环境变量
    /// `DEEP_STUDENT_WEBDAV_RATE_LIMIT`（每 30 分钟请求数，0 = 关闭）覆盖。
    fn resolve_rate_limit(host: &str) -> u32 {
        let host = host.to_lowercase();
        let is_nutstore = host.contains("jianguoyun") || host.contains("nutstore");
        let default = if is_nutstore { 540 } else { 0 };
        match std::env::var("DEEP_STUDENT_WEBDAV_RATE_LIMIT") {
            Ok(v) => v.trim().parse::<u32>().unwrap_or(default),
            Err(_) => default,
        }
    }

    /// 滑窗节流：30 分钟窗口内发出的请求达到预算时，等待最早一条请求滑出窗口再放行。
    ///
    /// 等待可能长达数分钟，期间通过 `report_status` 向前端说明原因。
    /// std Mutex 不跨 await 持有：占位/计算等待时长在锁内完成，休眠在锁外。
    async fn acquire_request_slot(&self, desc: &str) {
        if self.rate_limit_per_30min == 0 {
            return;
        }
        loop {
            let wait = {
                let mut window = self.rate_window.lock().unwrap_or_else(|p| p.into_inner());
                let now = std::time::Instant::now();
                while let Some(front) = window.front() {
                    if now.duration_since(*front) >= Self::RATE_WINDOW {
                        window.pop_front();
                    } else {
                        break;
                    }
                }
                if (window.len() as u32) < self.rate_limit_per_30min {
                    window.push_back(now);
                    return;
                }
                let oldest = *window.front().expect("窗口已满则必非空");
                Self::RATE_WINDOW - now.duration_since(oldest) + Duration::from_secs(1)
            };
            tracing::warn!(
                "WebDAV {} 触发限速保护（预算 {}/30min），{}s 后继续",
                desc,
                self.rate_limit_per_30min,
                wait.as_secs()
            );
            super::traits::report_status(&format!(
                "坚果云限速保护：每 30 分钟 {} 次的请求额度已用满，{} 秒后自动继续…",
                self.rate_limit_per_30min,
                wait.as_secs()
            ));
            tokio::time::sleep(wait).await;
        }
    }

    /// 发送 HTTP 请求（带重试）
    async fn request_with_path(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response> {
        let url = self.build_path_url(path)?;
        let desc = method.to_string();
        self.send_with_retry(&desc, || {
            let builder = self
                .http
                .request(method.clone(), url.clone())
                .header("Authorization", self.auth_header());
            if let Some(ref b) = body {
                builder.body(b.clone())
            } else {
                builder
            }
        })
        .await
    }

    async fn request(
        &self,
        method: Method,
        key: &str,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response> {
        self.request_with_path(method, &self.remote_path(key), body)
            .await
    }

    /// 确保目录存在（递归创建）
    ///
    /// 已确认存在的目录会缓存（`ensured_dirs`），批量上传大量小文件时
    /// 避免每个文件都重发整串 MKCOL 触发服务端频率限制。
    async fn ensure_directory(&self, path: &str) -> Result<()> {
        let parts: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let mut current = String::new();
        for part in parts {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(part);

            // 目录缓存命中则跳过 MKCOL
            if let Ok(cache) = self.ensured_dirs.lock() {
                if cache.contains(&current) {
                    continue;
                }
            }

            // MKCOL 创建目录
            let res = self
                .request_with_path(Self::mkcol_method()?, &format!("{}/", current), None)
                .await?;

            // 405 METHOD_NOT_ALLOWED 或 409 CONFLICT 表示目录已存在，可以忽略
            if !matches!(
                res.status(),
                StatusCode::OK
                    | StatusCode::CREATED
                    | StatusCode::METHOD_NOT_ALLOWED
                    | StatusCode::CONFLICT
            ) {
                // 不是致命错误，目录可能已存在
                tracing::debug!("WebDAV MKCOL {} 返回 {}", current, res.status());
            }

            if let Ok(mut cache) = self.ensured_dirs.lock() {
                cache.insert(current.clone());
            }
        }
        Ok(())
    }

    /// 解析 PROPFIND 响应获取文件列表（使用 roxmltree 安全解析，防止 XXE 注入）
    fn parse_propfind_response(&self, xml: &str, prefix: &str) -> Vec<FileInfo> {
        let doc = match roxmltree::Document::parse(xml) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("WebDAV PROPFIND XML 解析失败: {e}");
                return Vec::new();
            }
        };

        let dav_ns = "DAV:";
        let mut files = Vec::new();

        for response in doc
            .descendants()
            .filter(|n| n.has_tag_name((dav_ns, "response")))
        {
            let href = response
                .descendants()
                .find(|n| n.has_tag_name((dav_ns, "href")))
                .and_then(|n| n.text())
                .unwrap_or_default();

            if href.ends_with('/') {
                continue;
            }

            let key = self.extract_relative_key(href, prefix);
            if key.is_empty() {
                continue;
            }

            let size = response
                .descendants()
                .find(|n| n.has_tag_name((dav_ns, "getcontentlength")))
                .and_then(|n| n.text())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);

            let modified = response
                .descendants()
                .find(|n| n.has_tag_name((dav_ns, "getlastmodified")))
                .and_then(|n| n.text())
                .and_then(|s| {
                    DateTime::parse_from_rfc2822(s)
                        .map(|dt| dt.with_timezone(&Utc))
                        .ok()
                })
                .unwrap_or_else(|| DateTime::<Utc>::from(std::time::UNIX_EPOCH));

            files.push(FileInfo {
                key,
                size,
                last_modified: modified,
                etag: None,
            });
        }

        files.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
        files
    }

    fn extract_relative_key(&self, href: &str, prefix: &str) -> String {
        // URL 解码
        let decoded = urlencoding::decode(href).unwrap_or_else(|_| href.into());

        // 提取 root 之后的路径
        let root_path = format!("/{}/", self.root);
        if let Some(idx) = decoded.find(&root_path) {
            let relative = &decoded[idx + root_path.len()..];
            // 如果有 prefix，检查是否匹配
            if !prefix.is_empty() && relative.starts_with(prefix) {
                return relative.to_string();
            } else if prefix.is_empty() {
                return relative.to_string();
            }
        }
        String::new()
    }

    /// 解析 PROPFIND 响应，同时返回文件列表和子目录列表。
    ///
    /// RFC 4918 推荐使用 Depth:1 + 客户端迭代递归替代 Depth:infinity，
    /// 避免依赖服务器对 infinity 的支持（坚果云等可能不支持）。
    fn parse_propfind_entries(
        &self,
        xml: &str,
        prefix: &str,
        request_dir: &str,
    ) -> (Vec<FileInfo>, Vec<String>) {
        let doc = match roxmltree::Document::parse(xml) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("WebDAV PROPFIND XML 解析失败: {e}");
                return (Vec::new(), Vec::new());
            }
        };

        let dav_ns = "DAV:";
        let mut files = Vec::new();
        let mut subdirs = Vec::new();
        let request_dir_normalized = request_dir.trim_matches('/');
        let prefix_normalized = prefix.trim_matches('/');

        for response in doc
            .descendants()
            .filter(|n| n.has_tag_name((dav_ns, "response")))
        {
            let href = response
                .descendants()
                .find(|n| n.has_tag_name((dav_ns, "href")))
                .and_then(|n| n.text())
                .unwrap_or_default();

            if href.ends_with('/') {
                // 目录项：提取相对路径（不做 prefix 过滤）
                let key = self.extract_relative_key(href, "");
                let dir_path = key.trim_matches('/');
                // 跳过请求目录自身和空路径
                if dir_path == request_dir_normalized || dir_path.is_empty() {
                    continue;
                }
                // 只包含 prefix 下的子目录
                if !prefix_normalized.is_empty() && !dir_path.starts_with(prefix_normalized) {
                    continue;
                }
                subdirs.push(dir_path.to_string());
            } else {
                let key = self.extract_relative_key(href, prefix);
                if key.is_empty() {
                    continue;
                }

                let size = response
                    .descendants()
                    .find(|n| n.has_tag_name((dav_ns, "getcontentlength")))
                    .and_then(|n| n.text())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);

                let modified = response
                    .descendants()
                    .find(|n| n.has_tag_name((dav_ns, "getlastmodified")))
                    .and_then(|n| n.text())
                    .and_then(|s| {
                        DateTime::parse_from_rfc2822(s)
                            .map(|dt| dt.with_timezone(&Utc))
                            .ok()
                    })
                    .unwrap_or_else(|| DateTime::<Utc>::from(std::time::UNIX_EPOCH));

                files.push(FileInfo {
                    key,
                    size,
                    last_modified: modified,
                    etag: None,
                });
            }
        }

        (files, subdirs)
    }
}

#[async_trait]
impl CloudStorage for WebDavStorage {
    fn provider_name(&self) -> &'static str {
        "WebDAV"
    }

    async fn check_connection(&self) -> Result<()> {
        // 先确保同步根目录存在，再做连接探测
        self.ensure_directory(&self.root).await?;

        // 用 PROPFIND Depth:0 探测根目录（同 stat() 的做法）。
        // 不能用 GET：坚果云等 SabreDAV 系服务器禁止 GET 目录集合，
        // 已认证用户会收到 403 Forbidden，导致误报"连接检测失败"。
        let url = self.build_path_url(&format!("{}/", self.root))?;
        let method = Self::propfind_method()?;
        let res = self
            .send_with_retry("PROPFIND(连接检测)", || {
                self.http
                    .request(method.clone(), url.clone())
                    .header("Authorization", self.auth_header())
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml")
                    .body(r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:allprop/></d:propfind>"#)
            })
            .await?;

        if res.status().is_success() || res.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(AppError::network(format!(
                "WebDAV 连接检测失败: {} {}",
                res.status(),
                res.status().canonical_reason().unwrap_or(""),
            )))
        }
    }

    async fn put_file(
        &self,
        key: &str,
        local_path: &Path,
        progress: Option<UploadProgressCallback>,
    ) -> Result<String> {
        self.ensure_directory(&self.root).await?;
        // 确保父目录存在
        if let Some(parent) = key.rfind('/') {
            let parent_path = &key[..parent];
            if !parent_path.is_empty() {
                self.ensure_directory(&self.remote_path(parent_path))
                    .await?;
            }
        }

        let metadata = std::fs::metadata(local_path)
            .map_err(|e| AppError::file_system(format!("读取文件元信息失败: {e}")))?;
        let file_size = metadata.len();
        let progress: Option<Arc<UploadProgressCallback>> = progress.map(Arc::from);
        if let Some(cb) = progress.as_ref() {
            cb(0, file_size);
        }

        let checksum = tokio::task::spawn_blocking({
            let path = local_path.to_path_buf();
            move || calculate_file_hash(&path)
        })
        .await
        .map_err(|e| AppError::internal(format!("计算校验和任务失败: {e}")))??;

        let url = self.build_url(key)?;
        let uploaded = Arc::new(AtomicU64::new(0));

        // 流式上传带有限重试：每次尝试重新打开文件、重建流。
        // 复用连接被服务端提前关闭时（"connection closed before message completed"），
        // reqwest 无法自动重放流式 body，必须重建。PUT 同一 key 内容一致，重发幂等。
        const MAX_ATTEMPTS: u32 = 3;
        let mut last_detail = String::new();
        let mut res_opt: Option<reqwest::Response> = None;

        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * (1 << attempt));
                tracing::debug!("WebDAV PUT 流式上传重试 {}/{}", attempt + 1, MAX_ATTEMPTS);
                tokio::time::sleep(delay).await;
            }
            self.acquire_request_slot("PUT 流式上传").await;

            let file = match tokio::fs::File::open(local_path).await {
                Ok(f) => f,
                Err(e) => return Err(AppError::file_system(format!("打开文件失败: {e}"))),
            };

            // 重置进度计数，避免重试时进度重复累计
            uploaded.store(0, Ordering::SeqCst);
            let progress_cb = progress.clone();
            let uploaded_ref = uploaded.clone();
            let stream = ReaderStream::new(file).map(move |chunk| {
                if let Ok(ref bytes) = chunk {
                    let new_total = uploaded_ref.fetch_add(bytes.len() as u64, Ordering::SeqCst)
                        + bytes.len() as u64;
                    if let Some(cb) = progress_cb.as_ref() {
                        cb(new_total, file_size);
                    }
                }
                chunk
            });

            // 显式 Content-Length：坚果云（SabreDAV 系）对 chunked PUT 支持不佳，
            // 可能直接断开连接，表现为 "error sending request for url"。
            match self
                .http
                .request(Method::PUT, url.clone())
                .header("Authorization", self.auth_header())
                .header("Content-Length", file_size)
                .body(reqwest::Body::wrap_stream(stream))
                .send()
                .await
            {
                Ok(res) => {
                    // 限流（429/503）：服从 Retry-After 或指数退避后重试
                    if Self::is_rate_limited(res.status()) && attempt + 1 < MAX_ATTEMPTS {
                        let delay = Self::rate_limit_delay(&res, attempt);
                        tracing::warn!(
                            "WebDAV PUT 流式上传被限流（{}），{:?} 后重试 {}/{}",
                            res.status(),
                            delay,
                            attempt + 2,
                            MAX_ATTEMPTS
                        );
                        super::traits::report_status(&format!(
                            "上传被云端限流（{}），{} 秒后自动重试（第 {}/{} 次）…",
                            res.status().as_u16(),
                            delay.as_secs(),
                            attempt + 2,
                            MAX_ATTEMPTS
                        ));
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    res_opt = Some(res);
                    break;
                }
                Err(e) => {
                    last_detail = reqwest_error_detail(&e);
                    tracing::warn!(
                        "WebDAV PUT 流式上传第 {} 次发送失败: {}",
                        attempt + 1,
                        last_detail
                    );
                    if attempt + 1 < MAX_ATTEMPTS {
                        super::traits::report_status(&format!(
                            "上传中断，稍后自动重试（第 {}/{} 次）…",
                            attempt + 2,
                            MAX_ATTEMPTS
                        ));
                    }
                }
            }
        }

        let res = res_opt.ok_or_else(|| {
            AppError::network(format!(
                "WebDAV 上传失败（已重试 {MAX_ATTEMPTS} 次）: {last_detail}"
            ))
        })?;

        if res.status().is_success() {
            if let Some(cb) = progress.as_ref() {
                cb(file_size, file_size);
            }
            Ok(checksum)
        } else {
            Err(AppError::network(format!(
                "WebDAV 上传失败: {} {}",
                res.status(),
                res.status().canonical_reason().unwrap_or("")
            )))
        }
    }

    async fn get_file(
        &self,
        key: &str,
        local_path: &Path,
        expected_checksum: Option<&str>,
        progress: Option<DownloadProgressCallback>,
    ) -> Result<String> {
        let info = self
            .stat(key)
            .await?
            .ok_or_else(|| AppError::not_found("云端文件不存在"))?;
        let total_size = info.size;
        let progress: Option<Arc<DownloadProgressCallback>> = progress.map(Arc::from);
        if let Some(cb) = progress.as_ref() {
            cb(0, total_size);
        }

        let res = self.request(Method::GET, key, None).await?;

        if res.status() == StatusCode::NOT_FOUND {
            return Err(AppError::not_found("云端文件不存在"));
        }
        if !res.status().is_success() {
            return Err(AppError::network(format!(
                "WebDAV 下载失败: {} {}",
                res.status(),
                res.status().canonical_reason().unwrap_or(""),
            )));
        }

        let parent = local_path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::file_system(format!("创建目录失败 {:?}: {}", parent, e)))?;
        let _temp_file = tempfile::Builder::new()
            .prefix(".download-")
            .tempfile_in(parent)
            .map_err(|e| AppError::file_system(format!("创建临时下载文件失败: {e}")))?;
        let temp_path = _temp_file.path().to_path_buf();

        let mut hasher = Sha256::new();
        let mut downloaded = 0u64;
        {
            let mut file = tokio::fs::File::create(&temp_path)
                .await
                .map_err(|e| AppError::file_system(format!("创建文件失败: {e}")))?;

            let mut stream = res.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let bytes = chunk.map_err(|e| AppError::network(format!("读取响应体失败: {}", reqwest_error_detail(&e))))?;
                file.write_all(&bytes)
                    .await
                    .map_err(|e| AppError::file_system(format!("写入文件失败: {e}")))?;
                hasher.update(&bytes);
                downloaded += bytes.len() as u64;
                if let Some(cb) = progress.as_ref() {
                    cb(downloaded, total_size);
                }
            }
            file.flush()
                .await
                .map_err(|e| AppError::file_system(format!("刷新文件失败: {e}")))?;
        }

        let checksum = format!("{:x}", hasher.finalize());
        if let Some(expected) = expected_checksum {
            if expected != checksum {
                return Err(AppError::validation(format!(
                    "校验失败：期望 {}, 实际 {}",
                    expected, checksum
                )));
            }
        }
        tokio::fs::rename(&temp_path, local_path)
            .await
            .map_err(|e| AppError::file_system(format!("保存下载文件失败: {e}")))?;
        Ok(checksum)
    }

    async fn put(&self, key: &str, data: &[u8]) -> Result<()> {
        self.ensure_directory(&self.root).await?;
        // 确保父目录存在
        if let Some(parent) = key.rfind('/') {
            let parent_path = &key[..parent];
            if !parent_path.is_empty() {
                self.ensure_directory(&self.remote_path(parent_path))
                    .await?;
            }
        }

        let res = self.request(Method::PUT, key, Some(data.to_vec())).await?;

        if res.status().is_success() {
            Ok(())
        } else {
            Err(AppError::network(format!(
                "WebDAV 上传失败: {} {}",
                res.status(),
                res.status().canonical_reason().unwrap_or(""),
            )))
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let res = self.request(Method::GET, key, None).await?;

        if res.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(AppError::network(format!(
                "WebDAV 下载失败: {} {}",
                res.status(),
                res.status().canonical_reason().unwrap_or(""),
            )));
        }

        let bytes = res
            .bytes()
            .await
            .map_err(|e| AppError::network(format!("读取响应体失败: {}", reqwest_error_detail(&e))))?;
        Ok(Some(bytes.to_vec()))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<FileInfo>> {
        let start_path = if prefix.is_empty() {
            String::new()
        } else {
            prefix.trim_matches('/').to_string()
        };

        let propfind_body = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getlastmodified/><d:getcontentlength/></d:prop></d:propfind>"#;

        let mut all_files = Vec::new();
        let mut dirs_to_visit = vec![start_path];
        // 坚果云单次 PROPFIND 上限 750 条目
        const JIANGUOYUN_PROPFIND_LIMIT: usize = 750;
        const MAX_DIRS: usize = 200;
        let mut visited = 0usize;

        while let Some(dir) = dirs_to_visit.pop() {
            visited += 1;
            if visited > MAX_DIRS {
                tracing::warn!("[WebDAV] 递归列举已访问 {MAX_DIRS} 个目录，停止遍历以防异常");
                break;
            }

            // RFC 4918: PROPFIND 对集合的 Request-URI 应以 `/` 结尾，
            // 否则某些服务器返回 301 重定向，而 reqwest 默认不对非 GET 方法跟随重定向。
            let dir_with_slash = if dir.is_empty() || dir.ends_with('/') {
                dir.clone()
            } else {
                format!("{}/", dir)
            };
            let url = self.build_url(&dir_with_slash)?;
            let method = Self::propfind_method()?;

            let res = self
                .send_with_retry("PROPFIND(列表)", || {
                    self.http
                        .request(method.clone(), url.clone())
                        .header("Authorization", self.auth_header())
                        .header("Depth", "1")
                        .header("Content-Type", "application/xml")
                        .body(propfind_body)
                })
                .await?;

            if res.status() == StatusCode::NOT_FOUND {
                continue;
            }
            if !res.status().is_success() {
                return Err(AppError::network(format!(
                    "WebDAV PROPFIND 失败: {} {}",
                    res.status(),
                    res.status().canonical_reason().unwrap_or(""),
                )));
            }

            let xml = res
                .text()
                .await
                .map_err(|e| AppError::network(format!("读取 PROPFIND 响应失败: {}", reqwest_error_detail(&e))))?;

            let (files, subdirs) = self.parse_propfind_entries(&xml, prefix, &dir);

            let entry_count = files.len() + subdirs.len();
            if entry_count >= JIANGUOYUN_PROPFIND_LIMIT - 1 {
                // [P1 Fix] 当条目数达到平台上限时，记录错误级别日志并继续。
                // 不能直接返回 Err，否则 prune_old_changes 内部的 list() 也会失败，
                // 导致用户无法通过清理来解决问题（鸡生蛋死锁）。
                // 返回已列出的文件，让调用方至少能处理已知文件（如执行清理）。
                tracing::error!(
                    "[WebDAV] PROPFIND 返回 {} 条目（达到坚果云 {} 上限），\
                     目录 '{}' 下可能有未列出的文件！建议尽快清理旧的同步变更文件。",
                    entry_count,
                    JIANGUOYUN_PROPFIND_LIMIT,
                    dir
                );
            }

            all_files.extend(files);
            dirs_to_visit.extend(subdirs);
        }

        all_files.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
        Ok(all_files)
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let res = self.request(Method::DELETE, key, None).await?;

        if res.status().is_success() || res.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(AppError::network(format!(
                "WebDAV 删除失败: {} {}",
                res.status(),
                res.status().canonical_reason().unwrap_or(""),
            )))
        }
    }

    async fn stat(&self, key: &str) -> Result<Option<FileInfo>> {
        let url = self.build_url(key)?;
        let method = Self::propfind_method()?;

        let res = self
            .send_with_retry("PROPFIND(stat)", || {
                self.http
                    .request(method.clone(), url.clone())
                    .header("Authorization", self.auth_header())
                    .header("Depth", "0")
                    .header("Content-Type", "application/xml")
                    .body(r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getlastmodified/><d:getcontentlength/></d:prop></d:propfind>"#)
            })
            .await?;

        if res.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(AppError::network(format!(
                "WebDAV PROPFIND 失败: {} {}",
                res.status(),
                res.status().canonical_reason().unwrap_or(""),
            )));
        }

        let xml = res
            .text()
            .await
            .map_err(|e| AppError::network(format!("读取 PROPFIND 响应失败: {}", reqwest_error_detail(&e))))?;

        let files = self.parse_propfind_response(&xml, "");
        Ok(files.into_iter().next())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_storage() -> WebDavStorage {
        WebDavStorage::new(
            WebDavConfig {
                endpoint: "http://localhost:8080/".to_string(),
                username: "webdav".to_string(),
                password: "webdav123".to_string(),
            },
            "deep-student-sync-contract/webdav/uuid".to_string(),
        )
        .expect("create test storage")
    }

    #[test]
    fn build_url_adds_root_once() {
        let storage = test_storage();
        let url = storage
            .build_url("objects/basic/hello.txt")
            .expect("build url");
        assert_eq!(
            url.as_str(),
            "http://localhost:8080/deep-student-sync-contract/webdav/uuid/objects/basic/hello.txt"
        );
    }

    #[test]
    fn build_path_url_uses_raw_server_path() {
        let storage = test_storage();
        let url = storage
            .build_path_url("deep-student-sync-contract/webdav/uuid/")
            .expect("build raw path url");
        assert_eq!(
            url.as_str(),
            "http://localhost:8080/deep-student-sync-contract/webdav/uuid/"
        );
    }

    #[test]
    fn remote_path_joins_once() {
        let storage = test_storage();
        assert_eq!(
            storage.remote_path("objects/basic/hello.txt"),
            "deep-student-sync-contract/webdav/uuid/objects/basic/hello.txt"
        );
        assert_eq!(
            storage.remote_path("objects/"),
            "deep-student-sync-contract/webdav/uuid/objects/"
        );
        assert_eq!(
            storage.build_url("objects/").unwrap().path(),
            "/deep-student-sync-contract/webdav/uuid/objects/"
        );
    }
}
