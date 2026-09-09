/// 翻译事件发射器 - 负责发送 SSE 事件到前端
use tauri::{Emitter, Window};

use super::types::{
    TranslationStreamCancelled, TranslationStreamComplete, TranslationStreamData,
    TranslationStreamError,
};

/// 翻译事件发射器
pub struct TranslationEventEmitter {
    window: Window,
}

impl TranslationEventEmitter {
    /// 创建新的事件发射器
    pub fn new(window: Window) -> Self {
        Self { window }
    }

    /// 发送增量数据事件
    ///
    /// ★ 增量协议（2026-09）：负载只含本次新增文本 `delta`，
    /// 不再携带全量 accumulated 及派生计数（避免 O(n²) 传输）。
    ///
    /// # 参数
    /// - `session_id`: 会话 ID（用于事件作用域）
    /// - `delta`: 本次增量内容
    pub fn emit_data(&self, session_id: &str, delta: String) {
        let event_name = format!("translation_stream_{}", session_id);

        let payload = TranslationStreamData {
            event_type: "data".to_string(),
            delta,
        };

        if let Err(e) = self.window.emit(&event_name, payload) {
            eprintln!("❌ [Translation] 发送数据事件失败: {}", e);
        }
    }

    /// 发送完成事件
    pub fn emit_complete(
        &self,
        session_id: &str,
        id: String,
        translated_text: String,
        created_at: String,
    ) {
        let event_name = format!("translation_stream_{}", session_id);
        let payload = TranslationStreamComplete {
            event_type: "complete".to_string(),
            id,
            translated_text,
            created_at,
        };

        if let Err(e) = self.window.emit(&event_name, payload) {
            eprintln!("❌ [Translation] 发送完成事件失败: {}", e);
        }
    }

    /// 发送错误事件
    ///
    /// `accumulated`: 截止错误发生时的全量内容（权威值）
    pub fn emit_error(&self, session_id: &str, message: String, accumulated: String) {
        let event_name = format!("translation_stream_{}", session_id);
        let payload = TranslationStreamError {
            event_type: "error".to_string(),
            message,
            accumulated,
        };

        if let Err(e) = self.window.emit(&event_name, payload) {
            eprintln!("❌ [Translation] 发送错误事件失败: {}", e);
        }
    }

    /// 发送取消事件
    ///
    /// `accumulated`: 截止取消时的全量内容（权威值）
    pub fn emit_cancelled(&self, session_id: &str, accumulated: String) {
        let event_name = format!("translation_stream_{}", session_id);
        let payload = TranslationStreamCancelled {
            event_type: "cancelled".to_string(),
            accumulated,
        };

        if let Err(e) = self.window.emit(&event_name, payload) {
            eprintln!("❌ [Translation] 发送取消事件失败: {}", e);
        }
    }
}
