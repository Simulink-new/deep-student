//! 流式期间周期性落盘器（P0-b 边界优化，2026-09-10）
//!
//! 取代前端 StreamingBlockSaver 每 5s 把**全量累积内容**回传
//! `chat_v2_upsert_streaming_block` 的 IPC 回声（长回答后半程每次几十上百 KB，
//! O(n²) 传输）。Rust 管线侧本就持有同一份累积内容，由本模块按
//! 时间闸 + 脏检查惰性落盘，防闪退语义不变（进程崩溃时 DB 中最多丢最后 5s）。
//!
//! 用法（ChatV2LLMAdapter / VariantLLMAdapter 一致）:
//! 1. 持有 `PeriodicBlockPersister` 字段;
//! 2. 每个 chunk 回调里 `if p.on_chunk_due() { 快照累积内容并调用 hook }`;
//! 3. 管线构造适配器后注入 hook（携带 DB 句柄 + session_id），
//!    hook 内部调用 block_actions::persist_streaming_block_internal。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 周期落盘钩子签名: (message_id, block_type, block_id, content)
pub(crate) type PeriodicPersistHook = Arc<dyn Fn(&str, &str, &str, &str) + Send + Sync>;

/// 与前端原 StreamingBlockSaver 防抖周期对齐
const PERSIST_INTERVAL: Duration = Duration::from_secs(5);

/// 流式 chunk 周期落盘器
///
/// 时间闸（5s）+ 脏检查（chunk 计数）双闸，均通过后才触发落盘。
/// chunk 回调本身是顺序到达的（单流任务内），无需额外加锁语义。
pub(crate) struct PeriodicBlockPersister {
    hook: Mutex<Option<PeriodicPersistHook>>,
    last_persist: Mutex<Instant>,
    /// 已收到的 chunk 总数（脏检查基准）
    chunk_count: Mutex<u64>,
    /// 上次落盘时的 chunk 计数
    persisted_chunk_count: Mutex<u64>,
}

impl PeriodicBlockPersister {
    pub(crate) fn new() -> Self {
        Self {
            hook: Mutex::new(None),
            last_persist: Mutex::new(Instant::now()),
            chunk_count: Mutex::new(0),
            persisted_chunk_count: Mutex::new(0),
        }
    }

    /// 注入落盘钩子（适配器构造后由管线调用）
    pub(crate) fn set_hook(&self, hook: PeriodicPersistHook) {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// 读取钩子（触发落盘时克隆一份使用）
    pub(crate) fn hook(&self) -> Option<PeriodicPersistHook> {
        self.hook.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// chunk 计数 +1，返回是否到达落盘时机（时间闸通过且有新内容）。
    ///
    /// 先判钩子：未注入时直接短路，零成本。
    pub(crate) fn on_chunk_due(&self) -> bool {
        if self.hook().is_none() {
            return false;
        }
        let count = {
            let mut guard = self.chunk_count.lock().unwrap_or_else(|e| e.into_inner());
            *guard += 1;
            *guard
        };
        {
            let mut last = self.last_persist.lock().unwrap_or_else(|e| e.into_inner());
            if last.elapsed() < PERSIST_INTERVAL {
                return false;
            }
            *last = Instant::now();
        }
        let mut persisted = self
            .persisted_chunk_count
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *persisted == count {
            return false; // 时间闸到了但无新内容
        }
        *persisted = count;
        true
    }
}

impl Default for PeriodicBlockPersister {
    fn default() -> Self {
        Self::new()
    }
}
