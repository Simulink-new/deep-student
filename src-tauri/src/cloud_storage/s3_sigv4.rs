//! S3 SigV4 签名辅助（2026-09-10 手写替代 aws-sdk-s3）
//!
//! 零新增依赖：HMAC-SHA256 用 sha2 手写（FIPS 198-1），URI 编码手写，
//! XML 解析由 s3.rs 侧用 roxmltree 完成。
//!
//! 关键设计：**canonical query 与实际请求 URL 共用同一个编码器**
//! （[`encode_query`]）——SigV4 签名与请求字节级一致是兼容性的生命线。
//! 刻意不用 form 风格编码（space→'+'）：S3 按 RFC3986 解码 query，
//! '+' 必须发 %2B、空格必须发 %20，否则含 '+' 的 continuation token
//! 与含空格的 prefix 会被服务端误解码。
//!
//! payload 一律 `UNSIGNED-PAYLOAD`（HTTPS 下 AWS/R2/OSS/MinIO 均支持），
//! 使 PUT 可流式上传无需预读全文计算哈希。

#![cfg(feature = "cloud_storage_s3")]

use sha2::{Digest, Sha256};

/// 未签名的 payload 哈希占位（配合 HTTPS 传输）
pub(crate) const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// HMAC-SHA256（FIPS 198-1 标准结构；避免为此引入 hmac crate）
pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        key_block[..32].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    let out = outer.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&out);
    result
}

/// 小写十六进制编码
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// SigV4 URI 编码：仅保留未保留字符 `A-Za-z0-9-._~`，其余按 UTF-8
/// 字节百分号大写编码。`encode_slash=false` 保留 '/'（canonical path 用，
/// S3 规范要求 path 单次编码且不归一化）。
pub(crate) fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 把 query 键值对编码为规范串（键值均 uri_encode，按编码后的键值对排序）。
/// 实际请求 URL 与 canonical query 都用它——同一编码器，杜绝分歧。
pub(crate) fn encode_query(pairs: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    encoded.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    encoded
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

/// 当前 UTC 时间的 SigV4 双格式时间戳：
/// 返回 (x-amz-date `YYYYMMDDTHHMMSSZ`, date-stamp `YYYYMMDD`)
pub(crate) fn amz_date_now() -> (String, String) {
    let now = chrono::Utc::now();
    (
        now.format("%Y%m%dT%H%M%SZ").to_string(),
        now.format("%Y%m%d").to_string(),
    )
}

/// SigV4 请求签名器（静态凭证）
pub(crate) struct SigV4Signer {
    pub(crate) access_key: String,
    pub(crate) secret_key: String,
    pub(crate) region: String,
}

/// 待签名请求的描述（与实际请求一一对应构建，避免二次推导）
pub(crate) struct RequestToSign<'a> {
    /// HTTP 方法（大写）
    pub(crate) method: &'a str,
    /// 规范化后的 URL（path 已编码、query 已由 encode_query 生成）
    pub(crate) url: &'a reqwest::Url,
    /// 额外要签名的头（如 content-type: application/xml）
    pub(crate) content_type: Option<&'a str>,
    /// payload 哈希（本实现恒为 UNSIGNED-PAYLOAD）
    pub(crate) payload_hash: &'a str,
    pub(crate) amz_date: &'a str,
    pub(crate) date_stamp: &'a str,
}

impl SigV4Signer {
    /// 生成 Authorization 头值。
    ///
    /// host 从 url 提取（非默认端口时含端口），与 reqwest/hyper 实际发送的
    /// Host 头一致；签名的头集合 = host + 两个 x-amz-* + 可选 content-type。
    pub(crate) fn authorization_header(&self, req: RequestToSign<'_>) -> String {
        let host = Self::host_of(req.url);
        let mut header_pairs: Vec<(&str, String)> = vec![
            ("host", host),
            ("x-amz-content-sha256", req.payload_hash.to_string()),
            ("x-amz-date", req.amz_date.to_string()),
        ];
        if let Some(ct) = req.content_type {
            header_pairs.push(("content-type", ct.to_string()));
        }
        header_pairs.sort_by(|a, b| a.0.cmp(b.0));

        let canonical_headers: String = header_pairs
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k, v.trim()))
            .collect();
        let signed_headers: String = header_pairs
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join(";");

        // canonical path/query 直接取自规范化 URL，与实际请求字节一致
        let canonical_uri = req.url.path().to_string();
        let canonical_query = req.url.query().unwrap_or_default().to_string();

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            req.method,
            canonical_uri,
            canonical_query,
            canonical_headers,
            signed_headers,
            req.payload_hash
        );

        let scope = format!("{}/{}/s3/aws4_request", req.date_stamp, self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            req.amz_date,
            scope,
            hex_encode(&Sha256::digest(canonical_request.as_bytes()))
        );

        let k_date = hmac_sha256(
            format!("AWS4{}", self.secret_key).as_bytes(),
            req.date_stamp.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key, scope, signed_headers, signature
        )
    }

    /// 从 URL 提取签名用 host（非默认端口时含端口，与 hyper Host 头一致）
    fn host_of(url: &reqwest::Url) -> String {
        let host = url.host_str().unwrap_or_default().to_string();
        match url.port() {
            Some(p) => format!("{}:{}", host, p),
            None => host,
        }
    }
}
