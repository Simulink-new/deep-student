# A9 I/O 模块数据传递审计

> 撰写: 2026-09-11 08:14 CST | 基线 commit: ae1e6385 | 分支: main
> 审计员串行完成（未派生子代理），代码只读

## 范围与方法

| 模块 | 行数 | 采样策略 |
|------|------|----------|
| src-tauri/src/multimodal/ (8 文件) | 6,553 | 全量精读；活路径延伸到 vfs/multimodal_service.rs + vfs/handlers/multimodal_handlers.rs + 前端调用链（核实 A4 遗留线索） |
| src-tauri/src/ocr_adapters/ (10 文件) | 2,763 | 全量精读 5 个核心文件，其余结构扫描 |
| src-tauri/src/translation/ (5 文件) | 940 | 快速过（对照样板，不深审） |
| src-tauri/src/providers/ | 2,507 | 结构扫描 + 重点段精读（请求构建/图片/base64/usage） |
| src-tauri/src/adapters/ (gemini-openai-converter.rs) | 2,435 | 结构扫描 + 请求构建与图片转换精读 |

交叉验证工具: 对每个疑似问题 grep 全仓调用方确认活性（死代码与活路径分开记录）。

**两个前提修正**（任务假设 vs 代码事实）:
1. **"PaddleOCR 本地进程 IPC"不存在**——全仓无 PaddleOCR 子进程/stdin-stdout 代码。三条 OCR 路径全部为云端 REST（AI Studio job API `paddleocr_api.rs`、SiliconFlow OpenAI 兼容 `paddle.rs`）或进程内系统 API（WinRT/Vision，`system_ocr/`）。问题关闭。
2. **multimodal 模块 ~57% 是死代码**——真正在跑的索引/检索路径在 `vfs/multimodal_service.rs`（批量设计，质量较好），而非本模块内逐页模式的 `page_indexer.rs`。

## 数据流摘要

### multimodal（重点）

**活路径（唯一在跑的链路）**:
```
IndexStatusView.tsx:711 / useExamSheetProgress.ts:31
  → invoke vfs_multimodal_index_resource (multimodal_handlers.rs:278)
  → VfsMultimodalService.index_resource_by_source_with_progress (multimodal_service.rs:510)
      ├─ SELECT preview_json + resource_id（业务表）
      ├─ 逐页: VfsBlobRepo::get_blob_path(1 SELECT) + tokio::fs::read + BASE64.encode
      │    → 全部页面的 base64 String 同时驻留 Vec<VfsMultimodalPage>        ← 发现#3
      ├─ embed_batch_with_progress（批 8 页/次 API 调用）
      │    每批: 取模型配置+解密(每批一次) → 6 连 base64 拷贝 → reqwest        ← 发现#4/#8
      ├─ write_chunks 一次性批量写 LanceDB（好设计）
      └─ 维度清理 + embedding_dim_repo 计数
  ├─ emit mm_index_progress（小 payload，按批发，正常）
```
base64 六连拷贝链（活路径，每页图片各一次）:
`BASE64.encode`(multimodal_service.rs:618) → `Into<String>` clone(:172-177) → `mm_inputs` iter clone(:204) → `prepare_inputs_with_fallback` 无条件 `input.clone()`(embedding_service.rs:360) → `From<&MultimodalInput>` format! data URL(types.rs:565) → reqwest `.json()` 序列化(rag_extension.rs:248)。

**死路径**: `page_indexer.rs`(1,760 行，仅 `AttachmentPreview` 类型被 multimodal_service.rs:24 引用)、`retriever.rs`(724 行，mod.rs 未声明)、`vector_store.rs`(973 行，全仓零引用，mod.rs:27 注释"llm_manager 依赖"已失效)、`reranker_service.rs`(307 行，零引用)。死代码里沉淀着最重的反模式（每页 `table_names()` 全目录列举 + delete + add、每页 6-8 次 SQL、exam `mm_indexed_pages_json` O(n²) 读改写 page_indexer.rs:1181-1222）——所幸不在跑。

**A4 遗留线索核实**（"前端有 image_base64 入参命令"）: 坐实存在——`vfs_multimodal_index` 命令（multimodal_handlers.rs:115）入参 `pages[].image_base64`。但 grep 前端全仓：唯一包装 `vfsRagApi.ts:545 → multimodalRagService.ts:238 vfsIndexResource()` **零调用者**（chatApi.ts:176 是注释掉的死代码）。当前无实际流量，属休眠的 IPC 大 payload 陷阱。

### ocr_adapters

纯适配层（prompt 构建 + 响应解析），无进程 IPC、无 SQL、无 emit。每页结果 `OcrPageResult` 同时持有 `region.text + region.raw_output + markdown_text`——同一份识别文本 ×3 复制（mod.rs Glm4v:202-220 / GenericVlm:280-298、deepseek.rs:105-120、paddle.rs:137-155）。消费方为 llm_manager/exam_engine、cmd/ocr.rs（配置/测试命令，payload 小）、chat_v2/handlers/ocr.rs（system_ocr 原始字节直传，无 base64）。system_ocr 的 `spawn_blocking` 用法正确（system_ocr/mod.rs:35），跨线程 1 次必要拷贝。

### translation（对照样板）

4 命令（mod.rs:34/104 + chat_popover.rs:304/314）、5 emit（events.rs emit_data/complete/error/cancelled + chat_popover emit_event）。**形态即修复参照**（commit 8792bbb8 + 4ba851bb）:
- chunk 事件只携带 `delta`（events.rs:28-39、chat_popover.rs:56 `Chunk { delta }`）——O(n) 传输；
- 终态（complete/error/cancelled）一次性携带全量 `accumulated` 作为权威值（chat_popover.rs:269）；
- 事件名会话级作用域（`translation_stream_{session_id}` / `chat_translation_{request_id}`），无跨会话串扰；
- 回调内 `accumulated.push_str` + 结束时 1 次 clone——可接受。
对照：essay_grading/pipeline.rs:107 等处仍是每 chunk `.clone()` 全量（A8 已记录），照此样板改即可。

### providers / adapters

协议适配层（OpenAI / OpenAIResponses / Anthropic / Gemini 的请求构建 + SSE 解析），非配置装配层——配置读取+解密发生在 llm_manager 每次调用时（见发现#8 跨模块注记）。两个模块共享同一图片处理函数模式:
- `providers/mod.rs:1697 create_base64_payload`（Anthropic 路径，:1401 消费）
- `adapters/gemini-openai-converter.rs:1857 image_url_to_inline_data`
均: data: URL 拆串复制 / 远程 URL → `fetch_binary_with_cache`（utils/fetch.rs:99）缓存命中 `.cloned()` 整份字节 + 重新 base64 编码。**对话历史中的每张远程图片每轮请求都重走 clone+编码**。fetch.rs 本身: `reqwest::blocking::Client`（同步，10s 超时）+ 无上限 `HashMap` 内存缓存。

### base64 膨胀专项（×4/3 放大点全录）

| # | 位置 | 形态 | 活性 |
|---|------|------|------|
| B1 | vfs/multimodal_service.rs:618/656 | blob Vec<u8> → BASE64.encode → String | 活（索引入口） |
| B2 | multimodal_service.rs:172-177 | &String → `impl Into<String>` clone 入 MultimodalInput | 活 |
| B3 | multimodal_service.rs:204 | `inputs.iter().map(clone)` 整批再拷 | 活 |
| B4 | embedding_service.rs:360 | fallback 检查路径无条件 `input.clone()`（有效图片也拷） | 活 |
| B5 | types.rs:561-575 | `From<&MultimodalInput>` format! data URL（拼新 String） | 活 |
| B6 | rag_extension.rs:219/248 | json! Value 装配 + reqwest `.json()` 序列化 | 活 |
| B7 | providers/mod.rs:1697-1717 | 缓存命中 bytes `.cloned()` + STANDARD.encode | 活（Anthropic） |
| B8 | gemini-openai-converter.rs:1874-1878 | 同上（Gemini） | 活 |
| B9 | gemini-openai-converter.rs:411 | `openai_body.clone()` 深拷整个含图 Value | 活（Gemini） |
| B10 | multimodal_handlers.rs:26 | IPC 入参 `image_base64`（整书 ×4/3 过 JSON） | 休眠（零调用） |
| B11 | page_indexer.rs:1245/1388、retriever.rs:579 | 同 B1 形态 | 死代码 |
| B12 | embedding_service.rs:391/446 | ImagePayload `to_string()`/tasks `clone()` | 死逻辑（VLSummary 模式已禁用） |

translation / ocr_adapters 模块内无 base64 点。

## 发现清单（影响降序）

| # | 位置(file:line) | 模式 | 复杂度/量级估计(假设注明) | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|--------------------------|----------|----------|----------|
| 1 | providers/mod.rs:1697-1721 + utils/fetch.rs:104-111（Anthropic :1401 消费） | 远程图片每请求全量 clone+重编码 | 假设 5 张 1MB 图 × 20 轮对话：每轮 5×(1MB clone+1.33MB 编码+1.33MB 序列化)≈18MB 瞬时/轮，累计 ~366MB 分配 + 数十 ms CPU/轮 | 每次 LLM 请求含图片 URL（含历史回放，每轮） | 缓存值从 `Vec<u8>` 改为编码后 String（或直接缓存 AnthropicImageSource JSON 片段），LRU 上限 | 低：纯缓存内容变更，协议不变 |
| 2 | utils/fetch.rs:9-10,113-119 | blocking HTTP 跑在 async 线程 + 无上限内存缓存 | 未命中图片阻塞 tokio worker 至 10s；缓存无淘汰，每张不同远程图永占 RAM（假设 10 张 2MB=20MB 且只增不减） | 每个新图片 URL 一次阻塞；缓存常驻 | 改 async client + `tokio::spawn_blocking` 包裹；缓存加容量/TTL 淘汰 | 低 |
| 3 | vfs/multimodal_service.rs:585-631 | 整书所有页 base64 同时驻留内存再嵌入 | 假设 200 页教材 ×1MB/页 PNG：base64 后 ~267MB 常驻整个嵌入期（网络 API 分钟级）；而每批只用 8 页 | 每次多模态索引任务 | prepare 阶段只收集 blob_hash 描述符，批内(8 页)临期再 read+encode——峰值从 O(全书) 降到 O(8 页)，~25× | 中：服务内部签名改动，纯后端 |
| 4 | B2-B6 链（multimodal_service.rs:172/204 + embedding_service.rs:360 + types.rs:565 + rag_extension.rs:248） | base64 六连拷贝 | 每页 ~6×1.33×文件大小瞬时分配；假设 1MB/页 ×200 页 = ~1.6GB 累计分配（GC 压力，非同时驻留） | 每页每次索引 | base64 用 `Arc<str>` 传递；fallback 检查改借用式（返回引用枚举而非 clone Vec）；data URL 只拼一次；`prepare_inputs_with_fallback` 无图时不 clone | 低-中：模块内签名改动 |
| 5 | multimodal/{page_indexer,retriever,vector_store,reranker_service}.rs | 死代码 ~3,764 行（57%），内含最重反模式 | 每行都进 cargo 编译；审计/维护误导（mod.rs:27 "llm_manager 依赖"注释已失效）；若被复活即引入每页 table_names+delete+add、每页 6-8 SQL、O(n²) JSON 读改写 | 编译期常驻 | 删除四文件 + mod.rs 清理；`AttachmentPreview` 挪入 types.rs（唯一活引用 multimodal_service.rs:24） | 低：已 grep 验证零活引用 |
| 6 | vfs/handlers/multimodal_handlers.rs:26,46,115-176 | 休眠的 image_base64 IPC 大入参（A4 线索） | 若被调用：整书 ×4/3 base64 单次 invoke JSON（200 页 ~267MB）+ Rust 侧再反序列化一份 | 当前零调用（前端唯一包装 vfsIndexResource 无调用者） | 删除命令，或入参改 blob_hash 列表（后端已有自读路径 vfs_multimodal_index_resource） | 低：无调用者 |
| 7 | adapters/gemini-openai-converter.rs:411 + 1869 | Gemini 请求三段转换：Value 深拷 → 类型化 → 再序列化 | 假设含 3 张图的请求 body 5MB：clone(5) + 反序列化(5) + inline data.to_string()(4) + serialize(4) ≈ 18MB/请求 | 每次 Gemini 请求 | `build_gemini_request` 接收 `Value` 所有权原地改写（去掉 :411 clone）；或直接从 &Value 构建 GeminiRequest 跳过中间 OpenAIRequest | 低 |
| 8 | llm_manager/rag_extension.rs:205-206 + embedding_service.rs:586/689 | 每批嵌入重复取配置+解密 API key | 200 页书 = 25 批 × (配置 SELECT + AES 解密)；文本嵌入路径同模式 | 每个 API 批次 | 索引任务开始时取一次配置沿调用链下传；或配置加 settings 版本号缓存 | 低（跨模块，与 A6 协同） |
| 9 | ocr_adapters/mod.rs:202-220,280-298 + deepseek.rs:105-120 + paddle.rs:137-155 | 识别文本三重复制（text+raw_output+markdown_text） | 假设 grounding 响应 ~30KB/页 ×50 页文档：4.5MB vs 必要 1.5MB；raw_output 无条件存全量响应 | 每页 OCR | raw_output 仅在解析降级路径保留；非 grounding 模式 region.text 与 markdown_text 二选一 | 低-中：先 grep 消费方是否读 raw_output |
| 10 | vfs/multimodal_service.rs:598 | 每页 1 次 blob 路径 SELECT（N+1 轻量） | 单行索引查询 ×N 页；200 页 = 200 次查询（每次 ~10μs 级） | 每次索引 | 一次 `WHERE hash IN (...)` 批量取路径 | 低 |
| 11 | embedding_service.rs:483-484,508 | info 级日志打印 base64 前缀/500 字摘要 | 每页 2-3 条 info；日志体积与 CPU 小但持续刷屏 | 每页（该路径目前因 VLSummary 禁用而死；若复活即生效） | 降为 debug 级 | 低 |
| 12 | translation/*（样板记录，非问题） | delta 协议 + 终态全量 + 会话级事件名 | O(n) 传输；essay/qbank 照此改造可消除 O(n²)（essay_grading/pipeline.rs:107/128/187/211、qbank events.rs:61/77 仍全量 clone） | — | 作为 A8 修复的参照实现（commit 8792bbb8/4ba851bb） | — |

## 最小数据传递方案

该审计域的理想形态：

1. **图片只编码一次，只传引用**: 索引管线全程传 `blob_hash`；到批内(8 页)才 read+encode；base64 以 `Arc<str>` 单份共享到 HTTP 序列化那一刻。全管线图片字节的"物质化次数"从 6 → 2（encode + serialize，理论下限）。
2. **远程图片缓存编码产物而非原始字节**: fetch.rs 缓存 `String`(base64) 或目标协议片段，容量上限 + 淘汰；多轮对话同图零重复编码。
3. **大 payload 不过 IPC**: 图片数据永远走"后端自读 blob"路径（现有 vfs_multimodal_index_resource 即是）；跨 IPC 只传 id/hash/摘要。删除 vfs_multimodal_index 的 base64 入参形态，杜绝后端→前端→后端往返的结构可能性。
4. **流式一律 delta 协议**: translation 样板（chunk=delta、终态=权威全量）推广到 essay/qbank。
5. **OCR 结果单份文本**: 消费端要什么给什么，raw_output 只在调试/降级路径存在。
6. **配置与解密按任务粒度**: 一次索引/一次会话取一次，不按批次。
7. **死代码归零**: multimodal 四文件删除后，该模块从 6,553 行降到 ~2,800 行，审计面与编译面同步收敛。

## 不动清单

| 项 | 原因 |
|----|------|
| translation 全模块 | 已是 delta 样板（对照基准），动它无收益 |
| system_ocr（windows.rs/macos.rs/mod.rs） | spawn_blocking 正确、单次必要拷贝、SSRF 无关；WinRT 同步 API 别改 async |
| VfsMultimodalService 的批量写/无空窗替换/维度清理（multimodal_service.rs:280-320） | 设计良好（单次 write_chunks + cleanup），勿在优化中误伤 |
| embedding_chunker.rs | 分块逻辑 O(n) 且被 rag_extension 依赖，无数据传递问题 |
| `mm_index_progress` 事件协议 | 小 payload 按批发，前端已依赖（改 payload 需 B 侧同步，收益为零） |
| ocr_adapters 的 DeepSeek grounding 字节扫描解析（deepseek.rs:164+） | 手写 O(n) 扫描无回溯，正确且快 |
| fetch.rs 的 SSRF 防护逻辑（is_safe_remote_url/is_blocked_ip） | 安全边界，优化并发/缓存时必须原样保留 |
| providers 各 adapter 的 pending_tool_calls 共享状态结构 | 流式工具调用聚合所需；重构缓存时不要顺手"清理" |
