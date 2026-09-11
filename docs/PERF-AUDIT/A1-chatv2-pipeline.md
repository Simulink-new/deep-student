# A1 chat_v2 管线数据传递审计

> 撰写: 2026-09-11 08:05 CST | 基线 commit: ae1e6385 | 分支: main
> 任务: PERF-AUDIT task-006 (chat_v2 pipeline/events/prompt/persistence)
> 代码只读审计, 本文档为唯一产出

## 范围与方法

**范围** (PLAN.md §4 task-006):
- `src-tauri/src/chat_v2/pipeline/` 全部 14 文件 (重点 prompt.rs / persistence.rs / tool_loop.rs / multi_variant.rs / history.rs / llm_adapter.rs / periodic_persist.rs / helpers.rs / summary.rs / compaction.rs)
- `src-tauri/src/chat_v2/events.rs` (2010 行, 全读 emit 路径)
- 管线直接调用的存储层: `chat_v2/repo.rs` (4404 行, 按调用点抽查), `chat_v2/context.rs` (用户内容装配), `chat_v2/vfs_resolver.rs` (历史解引用), 根级 `chat_v2/pipeline.rs` (编排入口)
- 不含 handlers/ 与 tools/ (task-007 负责); 但 39 个 emit 点中位于 handlers/tools 的, 为完成专项核实 (a)(b) 做了覆盖面检查, 不深入

**方法**: 大文件按"命令入口→组装→存储/发出→返回"装配路径读 (Grep 定位 + Read offset/limit); emit 面用 `docs/PERF-AUDIT/idx-emits.txt` 的 chat_v2 39 点逐一归类; 专项 a/b/c/d 按 PLAN.md §5 已知热点逐条核实。代码零修改。

**假设口径** (量级估计共用): 典型会话 50 条历史消息(上限 DEFAULT_MAX_HISTORY_MESSAGES=50, constants.rs:22)、每条 1-4 块; 工具链 R=3 轮; 回答正文 8-40KB; 多模态图片单张 base64 0.5-2MB; 多变体 2-3 个模型。

## 数据流摘要

单变体主链 (pipeline.rs:300 execute → execute_internal): **加载历史**(history.rs: 每请求全量重读会话消息+逐消息查块+逐引用解引用 VFS) → **并行检索**(retrieval.rs, 本次未深审) → **构建 system prompt**(prompt.rs: 每请求重建, 含 2 次从零构造 Memory 服务栈读取画像/承诺) → **LLM 流式调用**(llm_adapter.rs: chunk 累积 + emit_chunk 进合批队列 + 5s 周期落盘) → **工具循环**(tool_loop.rs: 每轮全量 clone 历史 + 重建技能消息 + 工具前后各一次中间全量保存) → **终态保存**(persistence.rs save_results: 全量重写消息+全部块) → **后处理**(自动记忆提取再构造 Memory 栈 ×2)。

事件面: chunk 类事件 (content/thinking/tool_call_preparing/anki_cards/paper 进度) 全部经 `emit_chunk → buffer_chunk` 进按 session 的合批缓冲 (4096B / 16ms 双闸, events.rs:740-743), flush 前置于一切非 chunk 事件保证序列号连续; 非 chunk 事件 (start/end/error/variant/会话级终态) 逐条直发——本就是低频单发, 设计合理。

多变体链 (multi_variant.rs): 共享检索一次 → 但每变体独立 `load_variant_chat_history` (整段历史+VFS 解引用重做 N 遍) + 独立构建 system prompt (每变体再构造一次 Memory 栈) + fan-out 时对 options / user_context_refs (含 base64 图片) 做每变体一次深拷贝。

## 发现清单 (影响降序)

| # | 位置(file:line) | 模式 | 复杂度/量级估计(假设) | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|----------------------|----------|----------|----------|
| 1 | pipeline/tool_loop.rs:212 | 每工具轮 `ctx.chat_history.clone()` 全量深拷贝历史(含 tool_output JSON、OCR 注入文本) | O(R×H) 拷贝; 50 条历史×平均 2KB 文本×3 轮 ≈ 300KB/请求; 工具输出大时(检索结果 10-50KB/轮)可达 MB 级 | 每工具轮 | 历史构建为 `Arc<Vec<LegacyChatMessage>>`, 每轮 `Arc::clone` + 仅追加本轮消息( Cow 或新 Vec 只含增量) | 低: 内部数据结构, LLM 请求组装逻辑不变 |
| 2 | pipeline/multi_variant.rs:405-409, 429-434 (+283, 2363-2366) | Arc 包裹后每变体 `(*arc).clone()` 深拷贝 options(含 mcp_tool_schemas 全部 schema + skill_contents)与 user_context_refs(含 base64 图片 formattedBlocks)——Arc 形同虚设 | 3 变体×图片消息(2 张×1MB) ≈ 6MB 额外拷贝; 纯文本时为 schemas/skill_contents 数十-数百 KB×N | 每次多变体发送(含每变体重试) | 签名改 `Arc<SendOptions>` / `Arc<Vec<SendContextRef>>` 真共享; 或 formattedBlocks 的 base64 字段改 `Arc<str>` | 中: execute_single_variant 系列签名变更, 需过 cargo check; 行为不变 |
| 3 | pipeline/persistence.rs:101-306 + tool_loop.rs:983,1216 | 每工具轮 2 次 save_intermediate_results: 重写用户消息(attachments+context_snapshot 重新序列化, :171-183) + 全量块 SELECT 保 anki_cards(:191-195) + 全量块逐个重写(:263-267) | 用户消息写 2R+2 次/请求; 块重写 Σ≈O(R²) 块次(每轮重写此前全部块, 含全量 content/tool_output); 块 SELECT 2R+1 次 | 每工具轮×2 | 中间保存改增量: 只 upsert 本轮新增/变更块; 用户消息仅在 immediate 与 final 各写一次; anki_cards 保护改为一次 SELECT 后随轮缓存 | 中: 防闪退语义需保持(工具结果落盘时机不变); 事务边界重排需测试 |
| 4 | pipeline/history.rs:33 + repo.rs:905-929 | 全会话消息 SELECT 无 SQL LIMIT, meta_json/variants_json 全列反序列化后才在内存截 50 条(:76-86) | 500 条会话 → 450 条白白反序列化(含 meta JSON) 每请求; 假设长会话 meta 平均 1-8KB | 每次发送 | SQL 侧 `ORDER BY timestamp DESC LIMIT n` 再反转; compaction 视图需同 SQL 适配(tail_start 过滤) | 中: compaction 伪消息注入点需同步改造; 排序键(timestamp, rowid)必须保持 |
| 5 | pipeline/history.rs:98 + repo.rs:1236-1260 | N+1: 每条历史消息一次 `get_message_blocks_with_conn` | 50 消息 = 50 次 prepare+query/请求; SQLite 本地每次 ~50-200µs, 合计 ~5-10ms + 全块行(含未用字段)反序列化 | 每次发送 | 单查询 `WHERE message_id IN (…)` 或按 session JOIN 后按 message_id 分组 | 低: 纯查询合并, 结果集不变 |
| 6 | prompt.rs:33-114, 121-162 + persistence.rs:973-984, 1043-1055 + multi_variant.rs:1620-1650 | Memory 服务栈(VfsLanceStore+VfsMemoryStorage+MemoryService+MemoryCategoryManager)每请求构造 2 次(画像+承诺), 自动记忆提取再 2 次(门前+spawn 内); 多变体每变体再 1 次; 分类摘要/画像/承诺/会话主题每请求重复读取 | VfsLanceStore::new 本身轻(lance_store.rs:120-133, 惰性连接), 但 privacy 配置+分类文件+承诺表每请求 4-6 次重复读; 多变体×N | 每次发送(×变体数) | Pipeline 持有 `OnceCell<MemoryService>` + 分类摘要 TTL 缓存(或 memory 写路径失效钩子) | 低: 读侧缓存, 数据源不变; 注意隐私模式切换需失效 |
| 7 | prompt.rs:220-256 + context.rs:759-780, 874-897 | 多模态路径注入块构建两遍(get_combined_user_content 与 get_content_blocks_ordered 各调一次 build_injected_context_blocks), 图片 base64 克隆 3 次(fallback 收集 :774 `base64.clone()`、有序块、multimodal_parts 映射) | 2 张 1MB 图 ≈ 3MB 冗余拷贝/发送; 纯文本时仅双遍历开销 | 每次带图发送 | 构建一次 blocks 后派生: 文本 fallback 从同一 blocks 抽取, 图片字段改 `Arc<str>` | 低: prompt.rs 单函数内重排 |
| 8 | pipeline/history.rs:158-171, 438-558 | 历史用户消息 context_snapshot 每请求重新解引用: 每条消息每引用一次 VFS 资源 SELECT + data JSON parse + 文本格式化; 输出确定性 | 50 条历史×2 引用 = ~100 次资源查询+重复格式化/请求; OCR 文本注入内容大时格式化字符串重复构建 | 每次发送(多变体×N) | 解析结果(text)缓存于消息 meta 冗余字段或会话级 LRU(resource 版本/hash 失效); 或至少多变体共享一次解析 | 中: 缓存失效正确性(资源被编辑后需刷新); 首选多变体共享(无失效问题) |
| 9 | periodic_persist.rs:21,59-84 + block_actions.rs:617-665 + llm_adapter.rs:214-246 | 周期落盘每 5s 快照全量累积正文 UPSERT(get_accumulated_content 全串克隆+整行重写) | DB 写放大 O(n²/5s): 40KB 回答 60s ≈ 12 次写共 ~240KB; IPC 税已消除(进程内), 仅剩 DB 量 | 流式期间每 5s | 可选: 增量 journal 表(block_id,seq,delta) + 流结束合并; 或仅在增量>50% 时写。**收益中低, 防闪退语义敏感** | 高: 崩溃恢复路径, 建议先不动或仅做写量埋点 |
| 10 | pipeline.rs:392-444 | 每请求 `get_api_configs().await` 拉全部 API 配置线性扫描解析 1 个模型显示名; 兜底还可能 `select_model_for` | 配置数十条×每请求; 与 A6(llm_manager) 重叠, 此处记录管线侧调用面 | 每次发送 | llm_manager 增加按 id 直查/配置版本缓存; 或前端传 display_name 缓存 | 低: 读侧; 与 A6 协调 |
| 11 | tool_loop.rs:214-238 + helpers.rs:557-577 | 每工具轮 `load_effective_session_skill_state`(DB 查 session_state_v2) + 重建 transient skill messages | R 轮 = R 次重复查询+重建; 技能状态轮间通常不变 | 每工具轮 | 会话请求内缓存 skill state, 技能工具执行后失效重载 | 低: 注意技能运行时可变(需失效点) |
| 12 | persistence.rs:723-727 | save_results 终态再全量 SELECT 块(anki_cards 保护), 与发现 3 的重复读同源 | 1 次全块读/请求(终态) | 每次发送 | 与发现 3 一并改: anki_cards 块集在首次中间保存时缓存 | 低 |
| 13 | block_actions.rs:499-608 (核实记录) | 旧命令 `chat_v2_upsert_streaming_block` 仍注册仍可收全量 content; 前端 toolCall.ts:430/485/641、workspace/events.ts:504 一次性终态调用(工具块), 非周期回声 | 命令存活但入口断(见专项 b); 前端调用为每工具块 1 次 | 每工具块完成 | 前端这些一次性调用可改走专用终态命令(带 status); 命令本体保留待 B3 确认后再决定删留 | 低: B3/B4 协同 |

## 专项核实结论

### a) chunk 合批覆盖面 (39 emit 点逐类)

**走队列合批 (高频路径全覆盖 ✅)**:
- `events.rs` 的 `emit_chunk`(:1101) / `emit_chunk_with_meta`(:1115) 是唯一 chunk 入口, 内部 `buffer_chunk`(:960) 进按 session 缓冲(同流拼接复用序号, 4096B/16ms 双闸惰性 flush)。
- 管线全部高频流均经此: llm_adapter.rs:267/388/401/648/679/723/752/776/840/929 (content/thinking/args-delta), variant_adapter.rs(经 variant_context.rs:655/712), tools/chatanki_executor.rs:5260 (anki_cards 流), tools/paper_save_executor.rs:140 (NDJSON 进度流)。
- pipeline/ 目录内 **零** 直接 `window.emit` (grep 证实), 所有发射经 emitter。

**直发但属低频单发 (设计合理, 非 chunk 合批目标)**:
- events.rs 其余 12 个 emit 点 = start/end/error/preparing/skill_audit/variant_start/variant_end + 会话级 6 种 (stream_start/complete/error/cancelled/save/summary) —— 每块/每请求 1 次, 且发射前 `flush_pending_chunks` 保序 (events.rs:944-951, 1023-1024)。
- handlers 9 处: block_actions.rs:981/999 (anki_cards start/end, **已正确前置 flush_session_chunk_events + next_session_sequence_id**, :968-969), send_message.rs:355 (request_audit, 载荷为计数+ID 无正文), variant_handlers.rs:375 (variant_deleted, 用户动作), workspace_handlers.rs:968/985/1003/1049/1266 (子代理重试/状态, 事件级)。
- migration 3 处: 迁移进度事件, 每 item 1 次, 有界。
- tools 5 处: anki_executor:315 / canvas_executor:703 / session_executor:1164 / subagent_executor:215 / workspace_executor:300 —— 每工具调用 1 次的通知事件。
- workspace/emitter.rs 8 处: 多代理协作生命周期事件 (joined/left/status/document), 事件级非流级。

**未覆盖清单: 无。** 唯一二类绕过 emitter 直写 `chat_v2_event_{session}` 通道的是 block_actions.rs:981/999, 已带 flush+占号, 顺序安全。结论: 合批机制覆盖了全部高频 chunk 路径, 无漏网 (PLAN.md §5 该项核实为 ✅)。

### b) streamingBlockSaver 回声链路

**IPC 回声已断, 链路残骸仍在 (死代码链)**:
- 断点在**调用入口**: `scheduleBlockSave` 在整个 src/ 无生产调用方 (仅接口声明 autoSave.ts:264、类方法 :368、文档 BLOCK_RENDERING_GUIDE.md:315); eventBridge.ts:899/1083 注释明言"删除此处每 5s 全量内容回传"。原 autoSave.ts:368 行号漂移是因删调用后行数偏移, 方法本体仍在。
- 残骸: `StreamingBlockSaverImpl` 类完整存活 (autoSave.ts:303-502, 含防抖定时器+全量累积 Map+cleanup 定时器常驻), 回调仍接线 (TauriAdapter.ts:621 → executeUpsertStreamingBlock → invoke); Rust 命令 `chat_v2_upsert_streaming_block` 仍注册 (lib.rs:1255)。**运行时零开销** (无入口), 属维护负担; 建议待 B3 确认后整链删除 (前端类+接线+Rust 命令可保留给 toolCall.ts 的一次性终态调用——见下)。
- 前端仍有 5 处直接 invoke 该命令: toolCall.ts:430/485/641、workspace/events.ts:504 —— 从 Rust 侧签名看是**工具块终态一次性保存** (带 tool_output_json, 每工具块 1 次), 不是周期全量回声; 性质待 B3 最终确认。
- **替代链路已就位**: Rust 侧 `PeriodicBlockPersister` (periodic_persist.rs, 5s 时间闸+脏检查) → `persist_streaming_block_internal` (block_actions.rs:617) 进程内全量 UPSERT, 接线于 tool_loop.rs:188 与 multi_variant.rs:813/1091。O(n²) IPC 税已消除; 残余 O(n²/5s) DB 写放大见发现 9。

### c) prompt.rs: 每请求全量重建?

**是, 且重建范围超出 prompt.rs 本身**:
1. `build_system_prompt` (prompt.rs:9-26) 每请求执行: canvas 笔记读取 → 用户画像 (load_user_profile, :33-114, 从零构造 Memory 栈 + 读全部分类摘要文件) → 未完成承诺+会话主题 (load_pending_promises_and_context, :121-162, **再构造一遍** Memory 栈) → prompt_builder 格式化。画像/承诺/主题是低频变动数据, 无任何缓存。
2. 历史侧 "prompt 重建" 更大: load_chat_history 每请求全量重读 DB + 逐消息查块 + 逐引用解引用 VFS (发现 4/5/8); 多变体模式每变体重来一遍。
3. 工具循环内: 每轮 clone 全部历史 (发现 1) + 重建技能瞬态消息 (发现 11); system_prompt 字符串本身每请求只构建一次并按引用传递 (pipeline.rs:705→713), 这点无浪费。
4. 可缓存部分明确存在: 分类摘要/画像 (TTL 或写失效)、会话技能状态 (请求内缓存)、历史解引用文本 (确定性输出)、API 配置→模型名解析 (发现 10)。

### d) persistence.rs: 全量重写还是增量?

**全部为全量重写, 无增量路径**:
- 写语义: `create_message_with_conn` / `create_block_with_conn` 均 `INSERT … ON CONFLICT(id) DO UPDATE` 全列覆盖 (repo.rs:1162-1170); 块的 content/tool_output 整列重写。
- 自动保存频率与写入量 (单变体, R=3 工具轮假设):
  | 时机 | 次数 | 每次写入量 |
  |------|------|-----------|
  | 流式周期落盘 (5s 闸) | 每 5s | thinking+content **全量累积正文** (发现 9) |
  | save_user_message_immediately (pipeline.rs:489) | 1 | 用户消息+块 (attachments+context_snapshot 序列化) |
  | save_intermediate_results (工具前 :983 + 工具后 :1216) | 2R | 用户消息**再写** + 全部已累积块重写 + 全块 SELECT (发现 3) |
  | save_results 终态 | 1 | 用户消息第 2R+2 次写 + 消息 meta(usage/sources/tool_results/snapshot) + 全部块终态 + 再一次全块 SELECT (发现 12) |
- 净效果: 用户消息每请求被完整序列化+写入 2R+2 次; 助手块被重写 ΣO(R²) 块次; 全块 SELECT 2R+1 次。事务包裹正确 (BEGIN IMMEDIATE), 原子性无问题——问题是重复搬運。

## 最小数据传递方案

该管线理想形态 (功能全保留):
1. **历史读一次**: SQL LIMIT + 块单查询 JOIN; 解析产物 (`Arc<Vec<LegacyChatMessage>>` + 解引用文本) 会话级缓存, 新消息只追加。历史数据每请求只从 DB 移动一次 (现在: 1 + N块查询 + N变体遍)。
2. **工具轮零拷贝**: 不可变前缀共享, 每轮仅构造增量消息 (本轮工具结果); 技能状态请求内缓存。
3. **保存增量**: 用户消息首末各一次; 中间保存只 upsert 新块; anki_cards 块集缓存一次。终态 save_results 保持全量 (终态语义)。
4. **多变体真共享**: `Arc<SendOptions>` / `Arc<Vec<SendContextRef>>` / 共享一次历史解析与 system prompt 静态部分 (画像/承诺), 仅模型相关部分按变体构建。
5. **低频数据缓存**: Memory 栈/分类摘要 OnceCell + 写失效; 模型名解析直查。
6. **图片单拷贝**: 注入块构建一次, base64 `Arc<str>` 贯穿 fallback/ordered/multimodal_parts。
7. 保持不动: chunk 合批队列、5s 防闪退落盘 (进程内已是最小 IPC 形态, DB 写放大待埋点后再议)。

## 不动清单

- **事件协议**: `chat_v2_event_{session}` / `chat_v2_session_{session}` 通道名、sequence_id 连续性语义、前端 chunkBuffer.ts 的 4ms/4096 攒批假设 (改队列参数需与前端联动, 属 A11/B3 范围)
- **flush 保序触发点**: emit/emit_session 前置 flush (events.rs:944/1023) 与 block_actions.rs:968 的绕过协议——顺序正确性依赖它们
- **periodic_persist 5s 语义**: 防闪退承诺 (最多丢 5s); 发现 9 的改造高风险, 建议仅观测
- **save_results 终态全量**: 终态一次性写全量是正确的收敛点; 只削减中间重复
- **INSERT OR REPLACE (ON CONFLICT UPDATE) 语义**: 改写机制收益小于迁移风险, 优化目标是"调用次数"不是"写入机制"
- **local_api v1.1 端点**: 本模块不涉及, 但 compaction/历史裁剪改动不得改变其可见数据
- **handlers/ 与 tools/ 内部**: 留给 task-007 (本文仅核实其 emit 覆盖面与 block_actions 命令存活性)
- **前端 StreamingBlockSaver 死代码清理**: 需 B3 确认 toolCall.ts 三处调用性质后统一裁决, A1 不单方面建议删除
