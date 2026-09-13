use crate::models::ChatMessage;
use serde_json::Value;
use std::collections::HashMap;

fn provider_icon_for_origin(origin: &str) -> &'static str {
    match origin {
        "memory" => "memory",
        "web_search" => "search",
        "tool" => "tool",
        "graph" => "graph",
        _ => "rag",
    }
}

fn infer_provider_info(
    origin: &str,
    metadata: Option<&HashMap<String, String>>,
    fallback_label: &str,
    document_id: &str,
) -> (String, String, String) {
    let provider_id = metadata
        .and_then(|meta| {
            meta.get("library_name")
                .or_else(|| meta.get("library"))
                .or_else(|| meta.get("subject"))
                .or_else(|| meta.get("source_type"))
                .map(|v| v.to_string())
        })
        .unwrap_or_else(|| format!("{}:{}", origin, document_id));

    let provider_label = metadata
        .and_then(|meta| {
            meta.get("library_name")
                .or_else(|| meta.get("library"))
                .or_else(|| meta.get("subject"))
                .map(|v| v.to_string())
        })
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback_label.to_string());

    let icon = provider_icon_for_origin(origin).to_string();

    (provider_id, provider_label, icon)
}

fn augment_source_metadata(
    origin: &str,
    metadata: Option<&HashMap<String, String>>,
    file_name: &str,
    document_id: &str,
    source: &mut Value,
) {
    if let Value::Object(ref mut obj) = source {
        let (provider_id, provider_label, provider_icon) =
            infer_provider_info(origin, metadata, file_name, document_id);
        obj.insert("origin".into(), Value::String(origin.to_string()));
        obj.insert("provider_id".into(), Value::String(provider_id));
        obj.insert("provider_label".into(), Value::String(provider_label));
        obj.insert("provider_icon".into(), Value::String(provider_icon));
        obj.insert("provider_group".into(), Value::String(origin.to_string()));
    }
}

// A11#2: emit_unified_sources 整函数已删——零调用且唯一职责是发 {}_unified_sources 孤儿事件(前端零监听)

/// 构建错题场景的上下文
///
/// 参数：
/// - subject: 学科
/// - ocr_text: OCR识别的题目文本
/// - tags: 标签
/// - mistake_type: 错误类型
/// - user_question: 用户问题
/// - additional_docs: 附加文档内容
///
/// 返回：上下文哈希表
pub fn build_mistake_context(
    subject: &str,
    ocr_text: &str,
    tags: &[String],
    mistake_type: &str,
    user_question: &str,
    additional_docs: Option<&str>,
) -> HashMap<String, Value> {
    let mut context = HashMap::new();

    context.insert("subject".to_string(), Value::String(subject.to_string()));
    context.insert("ocr_text".to_string(), Value::String(ocr_text.to_string()));
    context.insert(
        "tags".to_string(),
        Value::Array(tags.iter().map(|tag| Value::String(tag.clone())).collect()),
    );
    context.insert(
        "mistake_type".to_string(),
        Value::String(mistake_type.to_string()),
    );
    context.insert(
        "user_question".to_string(),
        Value::String(user_question.to_string()),
    );

    if let Some(docs) = additional_docs {
        context.insert(
            "additional_documents".to_string(),
            Value::String(docs.to_string()),
        );
    }

    context
}

/// 构建回顾分析的上下文
///
/// 参数：
/// - subject: 学科
/// - consolidated_input: 综合输入内容
/// - overall_prompt: 整体提示（可选）
///
/// 返回：上下文哈希表
pub fn build_review_context(
    subject: &str,
    consolidated_input: &str,
    overall_prompt: Option<&str>,
) -> HashMap<String, Value> {
    let mut context = HashMap::new();

    context.insert("subject".to_string(), Value::String(subject.to_string()));
    context.insert(
        "consolidated_input".to_string(),
        Value::String(consolidated_input.to_string()),
    );

    if let Some(prompt) = overall_prompt {
        context.insert(
            "overall_prompt".to_string(),
            Value::String(prompt.to_string()),
        );
    }

    context
}

/// 将图片合并到聊天历史的最后一条用户消息中
///
/// 参数：
/// - history: 聊天历史（可变引用）
/// - imgs_base64: 本轮新增的图片（base64格式）
/// - pin_imgs: 固定的图片（可选）
///
/// 返回：合并的图片总数
pub fn merge_images_into_user_message(
    history: &mut Vec<ChatMessage>,
    imgs_base64: &[String],
    pin_imgs: Option<&[String]>,
) -> usize {
    // 查找最后一条用户消息
    if let Some(last_user_idx) = history.iter().rposition(|m| m.role == "user") {
        let mut last_user_msg = history[last_user_idx].clone();

        // 获取现有图片
        let mut merged_images = last_user_msg.image_base64.clone().unwrap_or_default();

        // 添加本轮新增图片
        for img in imgs_base64 {
            if !merged_images.contains(img) {
                merged_images.push(img.clone());
            }
        }

        // 添加固定图片
        if let Some(pin_images) = pin_imgs {
            for pin_img in pin_images {
                if !merged_images.contains(pin_img) {
                    merged_images.push(pin_img.clone());
                }
            }
        }

        // 更新消息
        if !merged_images.is_empty() {
            last_user_msg.image_base64 = Some(merged_images.clone());
            history[last_user_idx] = last_user_msg;
            return merged_images.len();
        }
    }

    0
}

fn truncate_snippet(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let trimmed = input.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut result = String::with_capacity(max_chars + 1);
    for (idx, ch) in trimmed.chars().enumerate() {
        if idx >= max_chars {
            result.push('…');
            break;
        }
        result.push(ch);
    }
    result
}
