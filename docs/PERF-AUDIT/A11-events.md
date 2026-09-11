# A11 事件层专项审计（emit↔listen 配对）

> 撰写: 2026-09-11 08:17 CST | 基线 commit: ae1e6385 | 分支: main
> 范围: Rust 侧 225 个 emit 调用点 (idx-emits.txt) ↔ 前端 102 个 listen 调用点 (idx-listens.txt)
> 方法: 全量配对。所有动态事件名（`event_name`/`stream_event`/常量/模板串）逐一回源码解析为实际通道名；B/C/D 类逐点核实 file:line 与 payload 形状。

## 范围与方法

- **命名解析**: 索引中约半数 emit/listen 的事件名是变量。已全部回源码解析，关键映射:
  - `chat_v2_event_{sid}`（块级，chunk 合批 4KB/16ms + 序列号 + 4 触发点保序）/ `chat_v2_session_{sid}`（会话级）— chat_v2/events.rs:827/894/899
  - `{stream_event}_*` 后缀族 — stream_event 即上述通道名或 `essay_grading_stream_{id}` / `qbank_grading_stream_{id}` / `translation_stream_{id}` 等基础通道（model2_pipeline.rs:697 以 `chat_v2_event_` 前缀判断才推送前端）
  - `chat_translation_{request_id}`（translation/chat_popover.rs:166）、`anki_tool_result:{call.id}`（anki_executor.rs:293，Rust listen + 前端 emit 的回传 RPC）
  - `data-governance-sync-progress`（sync/emitter.rs:20）、`chat_v2_migration`（legacy_migration.rs:29）、`mcp-stdio-{sid}-message/-error/-closed`（stdio_proxy.rs:68）
- **口径排除**: mcp/client.rs 的 10 处 `event_emitter.emit(McpEvent::…)` 是进程内 tokio 事件总线，**不经过 IPC**，不计入四向分类（记入方法论备注）。`anki_tool_result:{id}` 为 Rust 侧 listen、前端 emit 的反向 RPC，配对健康（anki_executor.rs:299/333/350/367/384 四路径均 unlisten）。
- **生命周期审计**: 前端 102 个 listen 点抽查了全部非常规模式；通用 hook（useTauriEventListener、usePdfProcessingProgress、useMigrationStatusListener）与三个流式 hook（essay/qbank/translation）均为模范实现（disposed 标志 + 卸载清空 + 终态 cleanup）。发现的例外单列。

## 事件四向分类总表

以「唯一事件通道/通道族」为计数单位（模板族算一个），共约 50 族。

| 事件通道 | 发射点 (代表) | 监听点 (代表) | 类别 | payload × 频率 | 处置建议 |
|---|---|---|---|---|---|
| chat_v2_event_{sid} | chat_v2/events.rs:944 等 14 处 + block_actions.rs:981/999 | TauriAdapter.ts:384/509、MultiAgentDebugPlugin、ThinkingBlockDebugPlugin、multiVariantTestPlugin:179 | A | BackendEvent，chunk 合批(4KB/16ms)×每 chunk | 不动 |
| chat_v2_session_{sid} | events.rs:1023、variant_handlers.rs:366 | TauriAdapter.ts:387/512、subagentEmbed.tsx:135 | A | 小 JSON×终态/状态变更 | 不动 |
| essay_grading_stream_{id} | essay_grading/events.rs:23(delta)/37/64/80 | useEssayGradingStream.ts:228 | A | delta×每 chunk；accumulated 仅终态一次 | 不动（PLAN 热点表已过时） |
| qbank_grading_stream_{id} | qbank_grading/events.rs:23/36/61/77 | useQbankAiGrading.ts:183 | A | 同上 | 不动（同上） |
| translation_stream_{id} | translation/events.rs:28/42/65/81 | useTranslationStream.ts:160 | A | delta×每 chunk | 不动 |
| chat_translation_{rid} | translation/chat_popover.rs:166-171 | TranslationPopover.tsx:327 | A | delta×每 chunk（4ba851bb） | 不动 |
| workspace_* 10 事件族 | workspace/emitter.rs:171-293、workspace_handlers.rs:968/985/1003/1049/1266、workspace_executor.rs:300、subagent_executor.rs:215 | workspace/events.ts:288-535(11 处)、sleepBlock.tsx:278/301/329、subagentTestPlugin、MultiAgentDebugPlugin、subagentEmbed | A/C | 小 JSON×每消息/状态变更；sleepBlock 每块 3 订阅→扇出 | 见发现 #17 |
| anki_tool_call / anki_tool_result:{id} | anki_executor.rs:315 / 前端 CardAgent.ts:1061/1085 | CardAgent.ts:872 / anki_executor.rs:299 | A | RPC 请求/结果×每次工具调用 | 不动 |
| backup-job-progress | backup_job_manager.rs:982(节流) | DataImportExport.tsx:523/719/1271、dataGovernance.ts:461 | A/C | 快照 JSON×节流限频 | 见发现 #19 |
| backup-jobs-resumable | lib.rs:674 | DataGovernanceDashboard.tsx:1380 | A | 小×启动一次 | 不动 |
| data-governance-migration-status | lib.rs:458/474/528/576 | useMigrationStatusListener.ts:148 | A | 小×迁移期间 | 不动 |
| data-governance-sync-progress | sync/emitter.rs:179、commands_sync.rs 19 处 | dataGovernance.ts:800 | A | 步骤进度小 JSON×每同步 ~20 步 | 不动 |
| cloud-sync-progress | cloud_storage/mod.rs:66 | CloudStorageSection.tsx:110 | A | 小×阶段级 | 不动 |
| textbook-import-progress | cmd/textbooks.rs:154/341 | LearningHubSidebar.tsx:761、resourceDropImport.ts:307 | A/C | 进度 JSON×每页/阶段 | 见发现 #20 |
| question_import_progress | commands.rs:1059/1128 | ExamSheetUploader.tsx:226、QuestionImportDebugPlugin.tsx:230 | A | 小×每 chunk（各有 session 过滤） | 不动 |
| csv_import_progress | commands.rs:1267 | CsvImportDialog.tsx:321 | A | 小×每行/阶段 | 不动 |
| mcp-test-progress | cmd/mcp.rs:72 | McpEditorSection.tsx:1776 | A | 微×每步 | 不动 |
| mcp-stdio-{sid}-message/-error/-closed | mcp/stdio_proxy.rs:78/84/100 | tauriStdioTransport.ts:226/246/262 | A | 日志行×每日志行（仅 stdio MCP 会话） | 不动 |
| mcp-bridge-request / …response:{corr}（4 对） | tools/mod.rs:448、streaming.rs:810 | mcpService.ts:1618-1651（request 侧）/ Rust 侧 scoped listen | A | RPC×每次桥调用 | 裸通道死发见发现 #11 |
| vfs-index-progress | index_handlers.rs:442/477/498/513 + 1061-1194(6 处)、indexing/mod.rs:2904/2983/3012/3100 | IndexStatusView.tsx:347 | A | 小 JSON×每资源 4 阶段 + 每 16 块嵌入批次 | 不动 |
| mm_index_progress | multimodal_handlers.rs:140/306 | IndexStatusView.tsx:442 | A | 小×每阶段/页 | 不动 |
| pdf_ocr_progress | pdf_ocr_service.rs 14 处 | usePdfProcessingProgress.ts:231、ExamSheetProcessingDebugPlugin(另一名) | A | 小×每页 | 不动 |
| menu 6 事件 | menu.rs:198 | menuEventBridge.ts:90 | A | 空×用户触发 | 不动 |
| canvas:ai-edit-request | canvas_executor.rs:703 | useCanvasAIEditHandler.ts:142 | A | 请求×每次 AI 编辑 | 不动 |
| anki_generation_event | enhanced_anki_service.rs 6 处、streaming_anki_service.rs 6 处 | CardAgent.ts:847、CardEngine.ts:605、TauriAdapter.ts:390/515 + debug×2 | C | 单卡/任务状态×每卡 | 见发现 #18 |
| media-processing-* / pdf-processing-* | pdf_processing_service.rs:2981+2990 / 3024+3033 / 3065+3074（连发）+678/689 | usePdfProcessingProgress.ts:205-228(6 通道全注册)、MediaProcessingDebugPlugin | C/D | 进度事件×每 tick；PDF 时**双名双处理** | 见发现 #5 |
| chat_v2_llm_request_body | model2_pipeline.rs:710 | TauriAdapter.ts:393/519（每 session 一个）、chatInteractionTestPlugin:440、multiVariantTestPlugin:163、attachmentPipelineTestPlugin:427 | D | **全量脱敏请求体×每次 LLM 调用** | 见发现 #1 |
| dstu:change:{path} + dstu:change | cmd/textbooks.rs:68+74、node_converters.rs:129+139、pdf_processing_service.rs:4065/4074 | dstu/api.ts:647（按 watch 注册二选一） | C | watch 事件×每文件变更，**双名连发** | 见发现 #12 |
| {stream_event}_web_search / _rag_sources / _memory_sources / _unified_sources | streaming.rs:498/510/522/553/567/578、chat_helpers.rs:161、tools/mod.rs:1050、model2_pipeline.rs:1452 | **无任何监听**（rag_sources 数据实际经消息字段/块事件到达前端） | B | 引用源数组（含 chunk_text 全文）×每次工具调用 | 见发现 #2 |
| {stream_event}_start / _id / _usage / _error / _reasoning / _cancelled / _safety_blocked、stream_error | model2_pipeline.rs:1634/1824/2098/2121/2109/2229/2233/2290/… | 仅 TemplateAIEngine（**死代码**，见 #13） | B | 小~中 JSON×每流 4-6 次（TemplateAI 已废弃后纯孤儿） | 见发现 #2 |
| chat_v2_request_audit | send_message.rs:355 | 仅 AttachmentOcrRequestAuditPlugin（debug） | B | 全量请求摘要×每条消息 | 见发现 #3 |
| deepseek_ocr_log | exam_engine.rs:247 函数，14 个调用点 | 仅 DeepSeekOcrDebugPlugin（debug） | B | {level,stage,page,message,data}×每页每阶段，无门控 | 见发现 #4 |
| mistake_status_update | commands.rs:911+941（for 循环内逐条） | **无** | B | 微×每错题 id | 见发现 #6 |
| session_management_change | session_executor.rs:1164 | **无** | B | 微×每次写操作工具调用 | 见发现 #7 |
| backup-import-progress | backup_job_manager.rs:989 | **无**（legacy） | B | ImportProgress×legacy 路径 | 见发现 #8 |
| notes-import-progress | cmd/notes.rs:1309 | **无** | B | 进度×导入期间 | 见发现 #9 |
| mcp_tools_changed | lib.rs:2122 | **无** | B | 微×MCP 工具变更 | 见发现 #10 |
| mcp-bridge 裸通道（response/tools-response/…无 corr 后缀） | 前端 mcpService.ts:1623/1631/1639/1647/1656 | **无**（Rust 只听 scoped） | B | RPC 响应×每次桥调用 | 见发现 #11 |
| TemplateAIEngine 8 通道（template_ai_stream_{sid}×7 后缀） | **无发射方**（Rust 全库无 template_ai_stream；template_ai.db 已标注废弃） | TemplateAIEngine.ts:32-236 | 反向 B | — | 见发现 #13 |
| canvas:note-updated | **无发射方**（前后端全库唯一引用是监听点） | NotesContext.tsx:582 | 反向 B | — | 见发现 #14 |
| tag_vector_status | **无发射方** | systemApi.ts:61 | 反向 B | — | 见发现 #15 |
| exam_sheet_progress | **无发射方**（导入流实际用 question_import_progress） | ExamSheetProcessingDebugPlugin.tsx:245 | 反向 B | — | 见发现 #16 |

**四向计数**: A 类健康 **24** 族 | B 类孤儿发射 **10** 族（另反向死监听 **4** 处） | C 类重复监听 **7** 族 | D 类高频全量 **3** 族。

**对 PLAN.md §5 热点表的修正**:
- 「essay/qbank O(n²) accumulated 全量 emit 未修」→ **已修**。essay pipeline.rs:100 与 qbank pipeline.rs:171 均传 chunk delta；accumulated 仅出现在终态（error/cancelled/complete 各一次，合法权威值）。三个流（essay/qbank/translation）事件层已是健康增量协议。
- 「chat chunk 合批覆盖面待核实」→ 直发旁路（block_actions.rs:968、model2 legacy 无 hook 路径）均正确调用 `flush_session_chunk_events` 或经 hook 回调，未发现绕过合批且仍高频的活路径。

## 发现清单（影响降序）

| # | 位置 | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
|---|---|---|---|---|---|---|
| 1 | model2_pipeline.rs:710；TauriAdapter.ts:1453-1474 | D+C: 每次 LLM 调用把**全量脱敏请求体**（system+全历史+工具 schema，随会话变长线性增长）emit 回前端；前端 `rawRequests` 数组按轮追加永不裁剪；N 个打开会话 = N 个 adapter 各收全量再按前缀丢弃 | IPC: O(prompt)×每轮；前端内存: O(Σ_rounds prompt) ≈ 长会话 MB 级（假设 20 轮会话平均 prompt 50-200KB → 单消息累计 1-4MB）。附带每次 `to_string_pretty` 全量序列化写 info 日志（model2_pipeline.rs:670） | 每次 chat 消息（无条件） | a) rawRequests 只留最近 1-2 轮或截断 body；b) emit 改为按需拉取（前端请求时 invoke 取回，debug 用）；c) 至少把 emit 与 pretty 日志挂到 debug 开关 | 中：前端「查看请求」功能依赖该数据；改拉取需前端同步适配 |
| 2 | streaming.rs:498/510/522/553/567/578、chat_helpers.rs:161、tools/mod.rs:1050、model2_pipeline.rs:1452、2109/3358、1634/1824/2098/2121/2229/2233/2290 等 | B: legacy 流协议后缀族（sources 带 chunk_text 全文；start/id/usage/error/reasoning/cancelled/safety_blocked/stream_error）**全前端零监听**——rag/web_search 来源数据实际经消息字段与块事件送达；TemplateAI（唯一后缀消费者）已死 | 每次 RAG/搜索工具调用浪费 O(引用块文本总量) 序列化+派发（假设 5-50 引用×0.5-2KB chunk_text）；每流另有 4-6 个小控制孤儿事件 | 每次带检索的 LLM 调用 | 删除或收敛为 hook 内回调；sources 若确需旁路通道则等前端接入后再启用 | 低：无消费者，删除无行为变化；保留字段路径即可 |
| 3 | send_message.rs:355；消费仅 AttachmentOcrRequestAuditPlugin.tsx:100 | B: 每条消息 emit 全量请求审计 payload，仅 debug 面板用 | O(request)×每消息（与 #1 同源但独立通道） | 每条消息 | 挂 debug 开关或并入 #1 的按需拉取 | 低：debug 功能加开关即可 |
| 4 | exam_engine.rs:247-280（14 调用点：297/321/468/475/482/489/1140/1246/1264/1280/1289/1304/1312/1350） | B/D: `emit_deepseek_debug` 无任何门控，OCR 期间逐页逐阶段向无消费者通道发射，`data` 字段可携带整页解析结果 | O(pages×stages)×(message+data)；假设 20 页×4 阶段×1-10KB = 80-800KB/次 OCR 白发 | 每次 DeepSeek OCR（含 PDF 索引自动 OCR） | 加 debug-enabled 标志（读全局 debug 配置）或彻底删除 emit 只留 log | 低：仅 debug 面板可见性 |
| 5 | pdf_processing_service.rs:2981+2990、3024+3033、3065+3074；usePdfProcessingProgress.ts:205-228 | C/D: PDF 进度**双名连发**（media-processing-* + pdf-processing-* 各一次）且前端 6 通道全注册、`handleProgress/handleCompleted` 对 PDF 每 tick 跑两遍（store 双写+双日志） | 每 tick 2×IPC+2×handler；假设 PDF 100 页×多阶段 ≈ 数百 tick → 数百次冗余事件+日志 | 每个 PDF 处理全程 | Rust 删 legacy 三连发；前端 hook 删 3 个 legacy 监听（迁移已完成，事件带 mediaType 字段可区分） | 低：需确认无其他 legacy 消费者（debug 插件同样双听，可同步删） |
| 6 | commands.rs:911+941 | B: `mistake_status_update` 循环内逐 id emit，前端零监听 | 微 payload×每更新错题；纯浪费但量小 | 每次试卷卡片更新/重命名 | 删除两处 emit（或改为将来真正需要时再加监听） | 无：零消费者 |
| 7 | session_executor.rs:1164 | B: 写操作工具成功后 emit `session_management_change`，前端零监听 | 微×每次会话写工具 | 每次会话管理写操作 | 删除，或在前端侧边栏需要刷新时接入 | 无 |
| 8 | backup_job_manager.rs:989 | B: legacy `backup-import-progress`，已被 job 化的 backup-job-progress 取代 | ImportProgress×legacy 导入路径每次进度 | legacy 导入时 | 确认 emit_legacy_progress 无调用后删除 | 低 |
| 9 | cmd/notes.rs:1309 | B: `notes-import-progress` 前端零监听 | 进度×导入期间 | 笔记导入时 | 删除或接入 UI | 无 |
| 10 | lib.rs:2122 | B: `mcp_tools_changed` 前端零监听（推测为 MCP 工具列表缓存失效通知，未接线） | 微×MCP 工具变更 | MCP 服务器重连/刷新 | 删除，或前端 MCP 服务接入做缓存失效 | 无 |
| 11 | mcpService.ts:1623/1631/1639/1647/1656 | B: 桥响应**双名回发**——裸通道（无 corr 后缀）零监听，Rust 只听 scoped `mcp-bridge-response:{corr}` | 每次桥调用 1 个死事件（Tauri 前端 emit 同样过 IPC 广播） | 每次 MCP 桥往返 | 删除 5 处裸通道 emit，只留 scoped | 低：grep 确认裸通道零消费者 |
| 12 | cmd/textbooks.rs:68+74（node_converters.rs:129+139 同型） | C: 每次文件变更**双名连发** scoped + global；scoped watcher 场景下 global 那次无人听 | watch 事件×2×每文件变更 | 学习库文件写入期间 | 按 watcher 注册形态单发；或仅保留 global 一种 | 低：dstu/api.ts 二选一注册，删除另一路不影响 |
| 13 | TemplateAIEngine.ts:29-236 + templateAiStore | 反向 B: 整引擎死代码——Rust 无 template_ai_stream 发射方，前端无外部实例化引用，template_ai.db 已废弃 | 8 个监听器永不触发；死代码维护负担 | — | 删除 TemplateAIEngine.ts + templateAiStore.ts + 相关类型 | 低：零引用，tsc 即可验证 |
| 14 | NotesContext.tsx:582 | 反向 B: `canvas:note-updated` 全库唯一引用是监听点，无发射方（画布更新走 invoke） | 1 个常驻悬空监听 | — | 删除该 useEffect 块 | 无 |
| 15 | systemApi.ts:61 | 反向 B: `tag_vector_status` 无发射方 | 1 个悬空监听 | — | 删除或接回后端状态推送 | 无 |
| 16 | ExamSheetProcessingDebugPlugin.tsx:245 | 反向 B: debug 插件听 `exam_sheet_progress`，真实导入流用 question_import_progress | debug 事件永不触发 | — | 改听 question_import_progress 或删除 | 无 |
| 17 | sleepBlock.tsx:278/301/329（subagentEmbed.tsx:135 同型） | C: 每个睡眠/子代理块实例挂 3 个 workspace 全局订阅，N 块会话对每个 workspace 事件跑 N×3 个 handler（各自早退过滤） | O(块数)×每 workspace 消息的 handler 派发；块数多时浪费明显但单 handler 便宜 | 多代理会话期间 | 收敛为会话级单一订阅+按 block_id 分发（workspace/events.ts 已有会话级总线可复用） | 中：UI 行为需回归验证 |
| 18 | enhanced_anki_service.rs×6 + streaming_anki_service.rs×6 → CardAgent.ts:847、CardEngine.ts:605、TauriAdapter.ts:390/515 | C: `anki_generation_event` 生产路径 3 个处理器逐卡全收（CardAgent 与 CardEngine 并存 + TauriAdapter 有 claimAnkiEventOwnership 仲裁） | 3×handler×每卡；单卡 payload 小，主要是指派/过滤浪费 | 每次流式制卡 | 明确单一属主（CardForge 场景 CardAgent、聊天场景 TauriAdapter），其余不订阅 | 中：制卡与聊天两场景需分别回归 |
| 19 | DataImportExport.tsx:523/719/1271 + dataGovernance.ts:461 | C: `backup-job-progress` 4 个监听点并存，各自 jobId 过滤；Rust 侧已节流所以量级无害；719 处监听在异步 handler 中创建，组件卸载中途无 useEffect 兜底（终态路径有 unlisten） | 有界：随任务终态释放；最坏任务挂起则常驻 | 每次备份/导入任务 | 719 处挪入 useEffect 或记录到 ref 于卸载时清理；其余不动 | 低 |
| 20 | LearningHubSidebar.tsx:761 + resourceDropImport.ts:307 | C: `textbook-import-progress` 两处监听均无 file/job 级过滤，并发导入（侧栏+拖拽同时）时互相串进度 | 小概率 UI 串扰；事件量本身小 | 教材导入期间 | payload 已含 file_name，前端按当前导入文件过滤 | 低 |
| 21 | TauriAdapter.ts:1489-1495 | 前端小项: 每个会话事件都向 window 派发 `chatanki-debug-lifecycle` DOM CustomEvent（携带全 payload detail），无消费者开关 | O(事件数)×DOM 派发；长会话事件多时持续开销 | 每个会话事件 | 挂 debug 开关（如 localStorage flag） | 低 |
| 22 | model2_pipeline.rs:670（#1 伴生） | 每次调用 `serde_json::to_string_pretty(&sanitized)` 全量序列化仅为打 info 日志 | O(prompt) CPU×每轮 | 每次 LLM 调用 | 降为 debug! 级别 + 截断，或复用 emit 的序列化结果 | 低 |
| 23 | model2_pipeline.rs:1934/2271 等（无 hook 分支） | D 结构性残留: 无 hook 调用方每 chunk 直发无合批；当前活调用方（chat_v2/essay/qbank/translation）都注册 hook，唯一无 hook 的 TemplateAI 已死 | 当前实际影响≈0；新增流式调用方若忘注册 hook 会退化为逐 chunk IPC | — | 在函数签名/文档上强制 hook 或为直发路径复用 SESSION_CHUNK_BUFFERS | 低 |
| 24 | usePdfProcessingProgress.ts:103-110/184-187（#5 伴生） | 每 tick 2-3 条 console.log（含 store 前后状态快照对象） | 长处理时控制台噪声+对象分配 | 每个 media/pdf tick | 降为 debugLog 或删除 | 无 |
| 25 | mcp/stdio_proxy.rs:78-100 | 边缘观察: stdio MCP 每行 stdout/stderr 都过 IPC 发 `-message/-error`；仅在 tauriStdioTransport 会话期间，日志量大的 MCP 服务器（如 verbose 模式）会形成事件流 | O(日志行数)；假设 verbose 服务器 1000 行/分钟 = 1000 事件/分钟 | stdio MCP 会话期 | 行级合并/环形缓冲，或只转发 error | 低：调试可见性降低 |

## 最小数据传递方案（事件层理想形态）

1. **删除全部孤儿发射**（发现 #2/3/4/6/7/8/9/10/11 + 死监听 #13-16）——约 40+ 个 emit 调用点与 4 处死监听直接移除，零行为损失。这是事件层最大单项收益：Rust 侧 emit 点预计从 225 降至 ~170。
2. **请求体回声改按需拉取**（#1）: `chat_v2_llm_request_body` 与 `chat_v2_request_audit` 合并为一个 debug 模式开关；开启时才 emit，且前端 `rawRequests` 只保留最近一轮（或 body 换 hash+logFilePath 引用，点开时 invoke 拉全文）。附带删 pretty 日志（#22）。
3. **单名单听**: PDF 双名连发与 dstu:change 双名连发收敛为单通道（media-processing-* / dstu:change:{path}），前端删除 legacy 监听。
4. **保持不动的合批/节流机制**: chat_v2 chunk 合批（4KB/16ms + 序列号保序）、backup 节流、essay/qbank/translation delta 协议——这些已是事件层理想形态，作为其他模块改造的模板。
5. **订阅收敛**: workspace 事件改会话级单订阅+块级分发；anki_generation_event 确立单一属主。
6. **该改 pull 的**: `backup-jobs-resumable`、`tag_vector_status` 类状态查询天然适合 invoke 拉取而非事件（前者保留启动一次推送即可）。

## 不动清单

- `chat_v2_event_{sid}` / `chat_v2_session_{sid}` 双通道 + 合批 + 序列号协议（前端乱序缓冲依赖序列号语义）
- essay/qbank/translation/chat_translation 四条 delta 流协议（2026-09 刚落地）
- `anki_tool_call` / `anki_tool_result:{id}` RPC（Rust listen 生命周期已验证完整）
- backup-job-progress 节流参数与 4 监听点（各自 jobId 过滤正确）
- workspace 事件常量与 workspace/events.ts 会话级总线（只收敛 sleepBlock 类重复订阅，不动协议）
- mcp-stdio-* / mcp-bridge scoped 通道（MCP 调试与桥接依赖）
- vfs-index-progress / mm_index_progress / pdf_ocr_progress 的粒度（每 16 块/每页，与 UI 进度条粒度匹配）
- 前端通用监听 hook 的 disposed/cleanup 模式（useTauriEventListener 等为模范实现，勿动）
