//! S3 兼容存储实现
//!
//! 支持 AWS S3、Cloudflare R2、阿里云 OSS、MinIO 等 S3 兼容服务
//!
//! 需要启用 `cloud_storage_s3` feature
//!
//! 🔄 2026-09-10: 手写 SigV4 客户端替代 aws-sdk-s3（执行层审计 P1-c，
//! 消除整个 AWS SDK 依赖树）。签名见 `s3_sigv4.rs`；payload 一律
//! UNSIGNED-PAYLOAD，PUT 走流式上传（Content-Length 显式指定）。
//! 行为与 aws-sdk 版逐项镜像：100MB 分块阈值 / 8MB 块 / 10000 块上限 /
//! SHA256 校验 / continuation 翻页 / 200-with-Error 陷阱检查。

#![cfg(feature = "cloud_storage_s3")]

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use super::config::S3Config;
use super::s3_sigv4::{
    amz_date_now, encode_query, uri_encode, RequestToSign, SigV4Signer, UNSIGNED_PAYLOAD,
};
use super::traits::{
    CloudStorage, DownloadProgressCallback, FileInfo, Result, UploadProgressCallback, CHUNK_SIZE,
    MIN_MULTIPART_SIZE,
};
use crate::models::AppError;

/// S3 兼容存储实现
pub struct S3Storage {
    http: reqwest::Client,
    signer: SigV4Signer,
    endpoint: reqwest::Url,
    bucket: String,
    /// 地址风格：true=/{bucket}/{key}，false=virtual-hosted（{bucket}.{host}/{key}）
    path_style: bool,
    root: String,
}

/// 请求结果：NotFound 单列（get/stat 语义需要把 404 变为 Ok(None)）
enum S3Response {
    Ok(reqwest::Response),
    NotFound,
}

impl S3Storage {
    /// 创建 S3 存储实例
    pub async fn new(config: S3Config, root: String) -> Result<Self> {
        if config.endpoint.trim().is_empty() {
            return Err(AppError::validation("S3 endpoint 不能为空"));
        }
        if config.bucket.trim().is_empty() {
            return Err(AppError::validation("S3 bucket 不能为空"));
        }

        let endpoint = reqwest::Url::parse(config.endpoint.trim())
            .map_err(|e| AppError::validation(format!("S3 endpoint 无效: {}", e)))?;

        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| AppError::internal(format!("构建 S3 HTTP 客户端失败: {}", e)))?;

        // 区域缺省 us-east-1（某些 S3 兼容服务需要，与 aws-sdk 版一致）
        let region = config
            .region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_string());

        let signer = SigV4Signer {
            access_key: config.access_key_id,
            secret_key: config.secret_access_key,
            region,
        };

        Ok(Self {
            http,
            signer,
            endpoint,
            bucket: config.bucket,
            path_style: config.path_style,
            root: root.trim_matches('/').to_string(),
        })
    }

    /// 构建完整的对象 key
    fn full_key(&self, key: &str) -> String {
        let key = key.trim_start_matches('/');
        if self.root.is_empty() {
            key.to_string()
        } else {
            format!("{}/{}", self.root, key)
        }
    }

    /// 从完整 key 中提取相对 key
    fn relative_key(&self, full_key: &str) -> String {
        let prefix = if self.root.is_empty() {
            String::new()
        } else {
            format!("{}/", self.root)
        };

        if full_key.starts_with(&prefix) {
            full_key[prefix.len()..].to_string()
        } else {
            full_key.to_string()
        }
    }

    /// 构建操作 URL：路径与查询串一次性规范编码（与 SigV4 canonical 一致）。
    /// endpoint 若带路径前缀（如 MinIO 子路径部署）会予以保留。
    fn op_url(&self, key: Option<&str>, query: &[(String, String)]) -> Result<reqwest::Url> {
        let mut url = self.endpoint.clone();
        if !self.path_style {
            let host = url.host_str().unwrap_or_default().to_string();
            let bucket_host = format!("{}.{}", self.bucket, host);
            url.set_host(Some(&bucket_host))
                .map_err(|e| AppError::validation(format!("S3 虚拟主机地址无效: {}", e)))?;
        }

        let base_path = url.path().trim_end_matches('/');
        let mut path = String::from(base_path);
        if self.path_style {
            path.push('/');
            path.push_str(&uri_encode(&self.bucket, true));
        }
        if let Some(k) = key {
            path.push('/');
            path.push_str(&uri_encode(k, false));
        }
        if path.is_empty() {
            path.push('/');
        }
        url.set_path(&path);

        let q = encode_query(query);
        if q.is_empty() {
            url.set_query(None);
        } else {
            url.set_query(Some(&q));
        }
        Ok(url)
    }

    /// 签名并发送请求；404 → NotFound，其余非 2xx → 带 XML 错误详情的 Err
    async fn do_request(
        &self,
        method: reqwest::Method,
        key: Option<&str>,
        query: Vec<(String, String)>,
        body: reqwest::Body,
        content_type: Option<&str>,
        content_length: Option<u64>,
    ) -> Result<S3Response> {
        let url = self.op_url(key, &query)?;
        let (amz_date, date_stamp) = amz_date_now();
        let auth = self.signer.authorization_header(RequestToSign {
            method: method.as_str(),
            url: &url,
            content_type,
            payload_hash: UNSIGNED_PAYLOAD,
            amz_date: &amz_date,
            date_stamp: &date_stamp,
        });

        let mut req = self
            .http
            .request(method, url)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", UNSIGNED_PAYLOAD)
            .header(reqwest::header::AUTHORIZATION, auth);
        if let Some(ct) = content_type {
            req = req.header(reqwest::header::CONTENT_TYPE, ct);
        }
        if let Some(len) = content_length {
            req = req.header(reqwest::header::CONTENT_LENGTH, len.to_string());
        }

        let resp = req
            .body(body)
            .send()
            .await
            .map_err(|e| AppError::network(format!("S3 请求失败: {}", e)))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(S3Response::NotFound);
        }
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(s3_xml_error(status.as_u16(), &body_text));
        }
        Ok(S3Response::Ok(resp))
    }

    /// 解析 S3 XML 错误响应为可读错误
    fn s3_xml_error(status: u16, body: &str) -> crate::models::AppError {
        if let Ok(doc) = roxmltree::Document::parse(body) {
            let root = doc.root_element();
            if root.tag_name().name() == "Error" {
                let code = child_text(&root, "Code").unwrap_or_else(|| "Unknown".to_string());
                let message = child_text(&root, "Message").unwrap_or_default();
                return AppError::network(format!("S3 {}: {} ({})", status, message, code));
            }
        }
        AppError::network(format!("S3 HTTP {} 错误", status))
    }

    /// 解析 InitiateMultipartUpload 响应中的 UploadId
    fn parse_upload_id(body: &str) -> Result<String> {
        let doc = roxmltree::Document::parse(body)
            .map_err(|e| AppError::internal(format!("解析 S3 分块上传响应失败: {}", e)))?;
        let root = doc.root_element();
        child_text(&root, "UploadId")
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AppError::internal("S3 分块上传未返回 upload_id".to_string()))
    }

    /// 解析 ListObjectsV2 响应。返回 (条目列表, 是否截断, 下一页 token)
    fn parse_list_body(
        body: &str,
    ) -> Result<(
        Vec<(String, u64, Option<DateTime<Utc>>, Option<String>)>,
        bool,
        Option<String>,
    )> {
        let doc = roxmltree::Document::parse(body)
            .map_err(|e| AppError::internal(format!("解析 S3 列表响应失败: {}", e)))?;
        let root = doc.root_element();

        let mut entries = Vec::new();
        let mut truncated = false;
        let mut next_token: Option<String> = None;

        for node in root.children().filter(|n| n.is_element()) {
            match node.tag_name().name() {
                "IsTruncated" => {
                    truncated = node.text().map(|t| t.trim() == "true").unwrap_or(false);
                }
                "NextContinuationToken" => {
                    next_token = node
                        .text()
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty());
                }
                "Contents" => {
                    let key = child_text(&node, "Key").unwrap_or_default();
                    let size: u64 = child_text(&node, "Size")
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    let last_modified = child_text(&node, "LastModified")
                        .and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok())
                        .map(|dt| dt.with_timezone(&Utc));
                    let etag = child_text(&node, "ETag");
                    if !key.is_empty() {
                        entries.push((key, size, last_modified, etag));
                    }
                }
                _ => {}
            }
        }
        Ok((entries, truncated, next_token))
    }

    /// XML 文本节点转义（CompleteMultipartUpload 请求体用）
    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
}

/// 读取直接子元素文本（trim 后），不存在返回 None。
/// 返回 String 以规避 roxmltree 双生命周期参数的标注负担。
fn child_text(node: &roxmltree::Node, name: &str) -> Option<String> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
}

#[async_trait]
impl CloudStorage for S3Storage {
    fn provider_name(&self) -> &'static str {
        "S3"
    }

    async fn check_connection(&self) -> Result<()> {
        // HEAD bucket 检查连接
        match self
            .do_request(
                reqwest::Method::HEAD,
                None,
                Vec::new(),
                reqwest::Body::empty(),
                None,
                None,
            )
            .await?
        {
            S3Response::Ok(_) => Ok(()),
            S3Response::NotFound => Err(AppError::not_found("S3 bucket 不存在")),
        }
    }

    async fn put_file(
        &self,
        key: &str,
        local_path: &Path,
        progress: Option<UploadProgressCallback>,
    ) -> Result<String> {
        let metadata = std::fs::metadata(local_path)
            .map_err(|e| AppError::file_system(format!("读取文件元信息失败: {e}")))?;
        let file_size = metadata.len();
        let full_key = self.full_key(key);

        let progress: Option<std::sync::Arc<UploadProgressCallback>> =
            progress.map(std::sync::Arc::from);
        if let Some(cb) = progress.as_ref() {
            cb(0, file_size);
        }

        if file_size < MIN_MULTIPART_SIZE {
            let checksum = tokio::task::spawn_blocking({
                let path = local_path.to_path_buf();
                move || crate::backup_common::calculate_file_hash(&path)
            })
            .await
            .map_err(|e| AppError::internal(format!("计算校验和任务失败: {e}")))??;

            let file = tokio::fs::File::open(local_path)
                .await
                .map_err(|e| AppError::file_system(format!("读取文件失败: {e}")))?;
            let body = reqwest::Body::wrap_stream(ReaderStream::with_capacity(file, 64 * 1024));

            match self
                .do_request(
                    reqwest::Method::PUT,
                    Some(&full_key),
                    Vec::new(),
                    body,
                    None,
                    Some(file_size),
                )
                .await?
            {
                S3Response::Ok(_) => {}
                S3Response::NotFound => {
                    return Err(AppError::not_found("S3 bucket 不存在"));
                }
            }
            if let Some(cb) = progress.as_ref() {
                cb(file_size, file_size);
            }
            return Ok(checksum);
        }

        // 创建分块上传
        let create_body = match self
            .do_request(
                reqwest::Method::POST,
                Some(&full_key),
                vec![("uploads".to_string(), String::new())],
                reqwest::Body::empty(),
                None,
                None,
            )
            .await?
        {
            S3Response::Ok(resp) => resp
                .text()
                .await
                .map_err(|e| AppError::network(format!("读取 S3 分块上传响应失败: {}", e)))?,
            S3Response::NotFound => {
                return Err(AppError::not_found("S3 bucket 不存在"));
            }
        };
        let upload_id = Self::parse_upload_id(&create_body)?;

        let upload_result: Result<String> = async {
            let mut file = tokio::fs::File::open(local_path)
                .await
                .map_err(|e| AppError::file_system(format!("打开文件失败: {e}")))?;
            let mut hasher = Sha256::new();
            let mut completed_parts: Vec<(i32, String)> = Vec::new();
            let mut part_number: i32 = 1;
            let mut uploaded = 0u64;
            let mut buffer = vec![0u8; CHUNK_SIZE];

            loop {
                let mut bytes_read = 0usize;
                while bytes_read < CHUNK_SIZE {
                    let n = file
                        .read(&mut buffer[bytes_read..])
                        .await
                        .map_err(|e| AppError::file_system(format!("读取文件失败: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    bytes_read += n;
                }

                if bytes_read == 0 {
                    break;
                }
                if part_number > 10_000 {
                    return Err(AppError::validation(
                        "S3 分块数超过 10000 的限制".to_string(),
                    ));
                }

                let chunk = &buffer[..bytes_read];
                hasher.update(chunk);

                let query = vec![
                    ("partNumber".to_string(), part_number.to_string()),
                    ("uploadId".to_string(), upload_id.clone()),
                ];
                let resp = match self
                    .do_request(
                        reqwest::Method::PUT,
                        Some(&full_key),
                        query,
                        reqwest::Body::from(chunk.to_vec()),
                        None,
                        Some(bytes_read as u64),
                    )
                    .await?
                {
                    S3Response::Ok(r) => r,
                    S3Response::NotFound => {
                        return Err(AppError::not_found("S3 bucket 不存在".to_string()));
                    }
                };
                let etag = resp
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string())
                    .ok_or_else(|| AppError::internal("S3 分块上传未返回 ETag".to_string()))?;
                completed_parts.push((part_number, etag));

                uploaded += bytes_read as u64;
                if let Some(cb) = progress.as_ref() {
                    cb(uploaded, file_size);
                }
                part_number += 1;
            }

            // CompleteMultipartUpload：S3 可能返回 200 但 body 是 <Error>，
            // 必须检查（aws-sdk 同样处理；漏检会静默丢对象）。
            let mut xml = String::from("<CompleteMultipartUpload>");
            for (num, etag) in &completed_parts {
                xml.push_str(&format!(
                    "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                    num,
                    Self::xml_escape(etag)
                ));
            }
            xml.push_str("</CompleteMultipartUpload>");

            let complete_body = match self
                .do_request(
                    reqwest::Method::POST,
                    Some(&full_key),
                    vec![("uploadId".to_string(), upload_id.clone())],
                    reqwest::Body::from(xml),
                    Some("application/xml"),
                    None,
                )
                .await?
            {
                S3Response::Ok(resp) => resp
                    .text()
                    .await
                    .map_err(|e| AppError::network(format!("读取 S3 完成分块响应失败: {}", e)))?,
                S3Response::NotFound => {
                    return Err(AppError::not_found("S3 bucket 不存在".to_string()));
                }
            };
            if let Ok(doc) = roxmltree::Document::parse(&complete_body) {
                if doc.root_element().tag_name().name() == "Error" {
                    return Err(Self::s3_xml_error(200, &complete_body));
                }
            }

            Ok(format!("{:x}", hasher.finalize()))
        }
        .await;

        if let Err(err) = upload_result {
            // 出错尽力中止分块上传，避免服务端残留未完成分块
            let _ = self
                .do_request(
                    reqwest::Method::DELETE,
                    Some(&full_key),
                    vec![("uploadId".to_string(), upload_id.clone())],
                    reqwest::Body::empty(),
                    None,
                    None,
                )
                .await;
            return Err(err);
        }

        if let Some(cb) = progress.as_ref() {
            cb(file_size, file_size);
        }
        upload_result
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
        let progress: Option<std::sync::Arc<DownloadProgressCallback>> =
            progress.map(std::sync::Arc::from);
        if let Some(cb) = progress.as_ref() {
            cb(0, total_size);
        }

        let parent = local_path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::file_system(format!("创建目录失败 {:?}: {}", parent, e)))?;
        let _temp_file = tempfile::Builder::new()
            .prefix(".download-")
            .tempfile_in(parent)
            .map_err(|e| AppError::file_system(format!("创建临时下载文件失败: {e}")))?;
        let temp_path = _temp_file.path().to_path_buf();

        let full_key = self.full_key(key);
        let resp = match self
            .do_request(
                reqwest::Method::GET,
                Some(&full_key),
                Vec::new(),
                reqwest::Body::empty(),
                None,
                None,
            )
            .await?
        {
            S3Response::Ok(resp) => resp,
            S3Response::NotFound => {
                return Err(AppError::not_found("S3 下载失败: 对象不存在"));
            }
        };

        let mut stream = resp.bytes_stream();
        let mut hasher = Sha256::new();
        let mut downloaded = 0u64;
        let mut buffer = Vec::with_capacity(64 * 1024);

        {
            let mut file = tokio::fs::File::create(&temp_path)
                .await
                .map_err(|e| AppError::file_system(format!("创建文件失败: {e}")))?;

            loop {
                buffer.clear();
                match stream.next().await {
                    Some(chunk) => {
                        let chunk = chunk
                            .map_err(|e| AppError::network(format!("读取 S3 响应失败: {}", e)))?;
                        buffer.extend_from_slice(&chunk);
                        file.write_all(&buffer)
                            .await
                            .map_err(|e| AppError::file_system(format!("写入文件失败: {e}")))?;
                        hasher.update(&buffer);
                        downloaded += buffer.len() as u64;
                        if let Some(cb) = progress.as_ref() {
                            cb(downloaded, total_size);
                        }
                    }
                    None => break,
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
        let full_key = self.full_key(key);

        match self
            .do_request(
                reqwest::Method::PUT,
                Some(&full_key),
                Vec::new(),
                reqwest::Body::from(data.to_vec()),
                None,
                Some(data.len() as u64),
            )
            .await?
        {
            S3Response::Ok(_) => Ok(()),
            S3Response::NotFound => Err(AppError::not_found("S3 bucket 不存在")),
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let full_key = self.full_key(key);

        match self
            .do_request(
                reqwest::Method::GET,
                Some(&full_key),
                Vec::new(),
                reqwest::Body::empty(),
                None,
                None,
            )
            .await?
        {
            S3Response::Ok(resp) => {
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| AppError::network(format!("S3 读取响应体失败: {}", e)))?;
                Ok(Some(bytes.to_vec()))
            }
            // NoSuchKey → Ok(None)（与 aws-sdk 版 is_no_such_key 语义一致）
            S3Response::NotFound => Ok(None),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<FileInfo>> {
        let full_prefix = self.full_key(prefix);

        let mut files = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut query: Vec<(String, String)> = vec![
                ("list-type".to_string(), "2".to_string()),
                ("prefix".to_string(), full_prefix.clone()),
            ];
            if let Some(token) = continuation_token.as_ref() {
                query.push(("continuation-token".to_string(), token.clone()));
            }

            let body = match self
                .do_request(
                    reqwest::Method::GET,
                    None,
                    query,
                    reqwest::Body::empty(),
                    None,
                    None,
                )
                .await?
            {
                S3Response::Ok(resp) => resp
                    .text()
                    .await
                    .map_err(|e| AppError::network(format!("读取 S3 列表响应失败: {}", e)))?,
                S3Response::NotFound => {
                    return Err(AppError::not_found("S3 bucket 不存在"));
                }
            };

            let (entries, truncated, next_token) = Self::parse_list_body(&body)?;

            for (key, size, last_modified, etag) in entries {
                // 跳过"目录"（以 / 结尾的虚拟目录）
                if key.ends_with('/') {
                    continue;
                }
                let last_modified = last_modified.unwrap_or_else(|| {
                    log::warn!("[CloudStorage::S3] Missing or invalid last_modified timestamp for key '{}', using epoch fallback", key);
                    DateTime::<Utc>::from(std::time::UNIX_EPOCH)
                });
                files.push(FileInfo {
                    key: self.relative_key(&key),
                    size,
                    last_modified,
                    etag,
                });
            }

            // 检查是否还有更多结果
            if truncated {
                match next_token {
                    Some(token) => continuation_token = Some(token),
                    None => {
                        // is_truncated=true 却没有 continuation token：
                        // 不带 token 重发只会拿到同一页（死循环），静默 break 则
                        // 返回截断列表（上层可能据此误判删除/上传）。如实报错。
                        // （🆕 2026-09 移植自上游 aa14a5e2b）
                        return Err(AppError::network(
                            "S3 列表被截断但未返回 continuation token，无法安全继续分页"
                                .to_string(),
                        ));
                    }
                }
            } else {
                break;
            }
        }

        // 按修改时间降序排列
        files.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
        Ok(files)
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let full_key = self.full_key(key);

        match self
            .do_request(
                reqwest::Method::DELETE,
                Some(&full_key),
                Vec::new(),
                reqwest::Body::empty(),
                None,
                None,
            )
            .await?
        {
            S3Response::Ok(_) => Ok(()),
            S3Response::NotFound => Ok(()), // 删除不存在的对象视为成功（S3 语义）
        }
    }

    async fn stat(&self, key: &str) -> Result<Option<FileInfo>> {
        let full_key = self.full_key(key);

        let resp = match self
            .do_request(
                reqwest::Method::HEAD,
                Some(&full_key),
                Vec::new(),
                reqwest::Body::empty(),
                None,
                None,
            )
            .await?
        {
            S3Response::Ok(resp) => resp,
            // HEAD 404 无 body，按 NotFound → None（与 aws-sdk 版 is_not_found 一致）
            S3Response::NotFound => return Ok(None),
        };

        let headers = resp.headers();
        let size: u64 = headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let last_modified = headers
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                chrono::DateTime::parse_from_rfc2822(s.trim())
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
            .unwrap_or_else(|| {
                log::warn!("[CloudStorage::S3] Missing or invalid last_modified timestamp for key '{}', using epoch fallback", key);
                DateTime::<Utc>::from(std::time::UNIX_EPOCH)
            });
        let etag = headers
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        Ok(Some(FileInfo {
            key: key.to_string(),
            size,
            last_modified,
            etag,
        }))
    }
}
