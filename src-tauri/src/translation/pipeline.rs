use serde_json::json;
/// 翻译管线 - 核心业务逻辑
use std::sync::Arc;

use crate::utils::streaming_llm_pipeline::StreamingLLMPipeline;

// 重新导出供 chat_popover.rs 等调用方使用
pub use crate::utils::streaming_llm_pipeline::StreamStatus;

use crate::database::Database;
use crate::llm_manager::{ApiConfig, LLMManager};
use crate::models::AppError;
// ★ VFS 统一存储（2025-12-07）
use crate::vfs::database::VfsDatabase;

use super::events::TranslationEventEmitter;
use super::types::{TranslationRequest, TranslationResponse};

/// 翻译管线依赖
pub struct TranslationDeps {
    pub llm: Arc<LLMManager>,
    pub db: Arc<Database>, // 主数据库（配置/设置读取）
    pub emitter: TranslationEventEmitter,
    pub vfs_db: Arc<VfsDatabase>, // ★ VFS 数据库（必需，唯一存储）
}

/// 运行翻译管线
pub async fn run_translation(
    request: TranslationRequest,
    deps: TranslationDeps,
) -> Result<Option<TranslationResponse>, AppError> {
    // 0. 输入验证：检查空文本
    if request.text.trim().is_empty() {
        return Err(AppError::validation("翻译文本不能为空".to_string()));
    }

    // 0.1 输入验证：检查文本长度（防止超大文本导致 API 超时或 OOM）
    const MAX_TEXT_CHARS: usize = 100_000; // 100K 字符上限
    let text_char_count = request.text.chars().count();
    if text_char_count > MAX_TEXT_CHARS {
        return Err(AppError::validation(format!(
            "翻译文本过长（当前 {} 字符，最大 {} 字符）",
            text_char_count, MAX_TEXT_CHARS
        )));
    }

    // 1. 构造翻译 Prompt
    let (system_prompt, user_prompt) = build_translation_prompts(&request)?;

    // 2. 获取翻译模型配置并解密 API Key
    let config = deps.llm.get_translation_model_config().await?;
    let api_key = deps.llm.decrypt_api_key(&config.api_key)?;

    // 3. 流式调用 LLM
    let mut accumulated = String::new();
    let stream_event = format!("translation_stream_{}", request.session_id);

    let stream_status = stream_translate(
        &config,
        &api_key,
        &system_prompt,
        &user_prompt,
        &stream_event,
        deps.llm.clone(),
        |chunk| {
            accumulated.push_str(&chunk);
            deps.emitter.emit_data(&request.session_id, chunk);
        },
    )
    .await?;

    if matches!(stream_status, StreamStatus::Cancelled) {
        deps.emitter
            .emit_cancelled(&request.session_id, accumulated.clone());
        return Ok(None);
    }

    // 🔧 P0-06 修复：移除后端的 VFS 记录创建，由前端统一管理
    // 原因：前端通过 Learning Hub 创建空翻译文件后，后端再创建会导致双写（孤儿记录）
    // 现在只返回翻译结果，前端通过 DSTU adapter 的 updateTranslation 更新记录
    let now = chrono::Utc::now().to_rfc3339();

    println!("✅ [Translation] 翻译完成，由前端管理存储");

    // 5. 发送完成事件（不再创建新记录，只返回翻译结果）
    deps.emitter.emit_complete(
        &request.session_id,
        request.session_id.clone(), // 使用 session_id 作为临时 ID，前端会用实际 node ID
        accumulated.clone(),
        now.clone(),
    );

    Ok(Some(TranslationResponse {
        id: request.session_id.clone(), // 使用 session_id，前端会忽略此值
        translated_text: accumulated,
        created_at: now,
        session_id: request.session_id,
    }))
}

/// 语言 code → 全名映射，确保 LLM 精确理解目标语言
pub(crate) fn lang_full_name(code: &str) -> &str {
    match code {
        "zh-CN" | "zh" => "Simplified Chinese (简体中文)",
        "zh-TW" => "Traditional Chinese (繁體中文)",
        "en" => "English",
        "ja" => "Japanese (日本語)",
        "ko" => "Korean (한국어)",
        "fr" => "French (français)",
        "de" => "German (Deutsch)",
        "es" => "Spanish (español)",
        "ru" => "Russian (русский)",
        "ar" => "Arabic (العربية)",
        "pt" => "Portuguese (português)",
        "pt-BR" => "Brazilian Portuguese (português brasileiro)",
        "it" => "Italian (italiano)",
        "vi" => "Vietnamese (tiếng Việt)",
        "th" => "Thai (ไทย)",
        "hi" => "Hindi (हिन्दी)",
        "tr" => "Turkish (Türkçe)",
        "pl" => "Polish (polski)",
        "nl" => "Dutch (Nederlands)",
        "sv" => "Swedish (svenska)",
        "la" => "Latin (Latina)",
        "el" => "Greek (Ελληνικά)",
        "uk" => "Ukrainian (українська)",
        "id" => "Indonesian (Bahasa Indonesia)",
        "ms" => "Malay (Bahasa Melayu)",
        "auto" => "auto-detected language",
        other => other,
    }
}

/// 领域预设 prompt 模板
fn domain_system_prompt(domain: &str) -> &str {
    match domain {
        "academic" => 
            "You are an expert academic translator specializing in scholarly papers, theses, and research articles. \
             Translate with precision, maintaining academic register and discipline-specific terminology. \
             Preserve citation formats (e.g. [1], (Author, Year)), mathematical notation, and abbreviations. \
             Ensure terminological consistency throughout. Only output the translated text.",
        "technical" => 
            "You are a professional technical translator specializing in software documentation, engineering, and IT content. \
             Keep code snippets, variable names, command-line examples, and API references untranslated. \
             Preserve markdown/HTML formatting. Translate technical terms accurately using industry-standard vocabulary. \
             Only output the translated text.",
        "literary" => 
            "You are a literary translator with expertise in creative writing. \
             Prioritize natural fluency and emotional resonance over literal accuracy. \
             Preserve rhetorical devices, metaphors, rhythm, and the author's unique voice. \
             Adapt cultural references when necessary for the target audience. Only output the translated text.",
        "legal" => 
            "You are a certified legal translator. \
             Translate with absolute precision using standard legal terminology in the target language. \
             Preserve the exact structure of clauses, articles, and numbered sections. \
             Do not paraphrase or simplify legal language. Only output the translated text.",
        "medical" => 
            "You are a medical translator with expertise in clinical and biomedical texts. \
             Use standard medical terminology (ICD/MeSH terms where applicable). \
             Preserve drug names, dosages, anatomical terms, and abbreviations accurately. \
             Only output the translated text.",
        "casual" | "conversation" => 
            "You are a friendly translator for everyday conversations and social media content. \
             Use natural, colloquial language that sounds native. \
             Adapt idioms, slang, and cultural expressions appropriately. Only output the translated text.",
        _ => 
            "You are a professional translator. Translate the given text accurately while preserving its tone, style, and formatting. Do not add explanations or notes. Only output the translated text.",
    }
}

/// 构造翻译 Prompt
pub fn build_translation_prompts(
    request: &TranslationRequest,
) -> Result<(String, String), AppError> {
    // System Prompt: 优先使用用户自定义，否则根据领域选择预设
    let mut system_prompt = if let Some(override_prompt) = &request.prompt_override {
        if !override_prompt.trim().is_empty() {
            override_prompt.clone()
        } else {
            domain_system_prompt(request.domain.as_deref().unwrap_or("general")).to_string()
        }
    } else {
        domain_system_prompt(request.domain.as_deref().unwrap_or("general")).to_string()
    };

    // 注入风格控制（当领域已是 casual 时跳过，避免重复指令）
    let domain_str = request.domain.as_deref().unwrap_or("general");
    if domain_str != "casual" && domain_str != "conversation" {
        if let Some(formality) = &request.formality {
            let style_instruction = match formality.as_str() {
                "formal" => {
                    "\n\nUse formal, polite language suitable for business or academic contexts."
                }
                "casual" => "\n\nUse casual, conversational language.",
                _ => "",
            };
            system_prompt.push_str(style_instruction);
        }
    }

    // 注入术语表
    if let Some(glossary) = &request.glossary {
        if !glossary.is_empty() {
            system_prompt.push_str(
                "\n\nGlossary (you MUST use these exact translations for the specified terms):",
            );
            for (src, tgt) in glossary {
                system_prompt.push_str(&format!("\n- \"{}\" → \"{}\"", src, tgt));
            }
        }
    }

    // User Prompt: 使用全语言名称
    let src_name = lang_full_name(&request.src_lang);
    let tgt_name = lang_full_name(&request.tgt_lang);

    let user_prompt = if request.src_lang == "auto" {
        format!(
            "Please translate the following text to {}:\n\n{}",
            tgt_name, request.text
        )
    } else {
        format!(
            "Please translate the following text from {} to {}:\n\n{}",
            src_name, tgt_name, request.text
        )
    };

    Ok((system_prompt, user_prompt))
}

/// 流式翻译（核心逻辑）
pub(crate) async fn stream_translate<F>(
    config: &ApiConfig,
    api_key: &str,
    system_prompt: &str,
    user_prompt: &str,
    stream_event: &str,
    llm: Arc<LLMManager>,
    on_chunk: F,
) -> Result<StreamStatus, AppError>
where
    F: FnMut(String),
{
    let messages = vec![
        json!({ "role": "system", "content": system_prompt }),
        json!({ "role": "user", "content": user_prompt }),
    ];

    let max_tokens = crate::llm_manager::effective_max_tokens(
        config.max_output_tokens,
        config.max_tokens_limit,
    );

    /// 翻译专用的 HTTP 错误映射
    fn translate_error_handler(status: u16) -> Option<&'static str> {
        match status {
            401 => Some("API 密钥无效或已过期，请检查设置"),
            403 => Some("API 访问被拒绝，请检查账户权限"),
            429 => Some("请求过于频繁，请稍后重试"),
            500..=599 => Some("翻译服务暂时不可用，请稍后重试"),
            _ => None,
        }
    }

    StreamingLLMPipeline::stream(
        config,
        api_key,
        messages,
        stream_event,
        llm,
        0.3,
        max_tokens,
        "翻译",
        Some(translate_error_handler),
        on_chunk,
    )
    .await
}
