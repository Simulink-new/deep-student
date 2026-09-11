# A8 memory+essay+qbank 数据传递审计

> 撰写: 2026-09-11 08:06 CST | 基线 commit: 8792bbb8 (工作树含未提交改动, 本审计三模块文件无脏改) | 任务: task-013

## 范围与方法

- `src-tauri/src/memory/` (11 文件, 6,693 行, 36 命令) — **全量精读** service.rs / handlers.rs / storage_trait.rs / reranker.rs / llm_decision.rs / audit_log.rs / compressor.rs / evolution.rs / auto_extractor.rs / category_manager.rs / config.rs
- `src-tauri/src/essay_grading/` (6 文件, 2,918 行, 20 命令, 4 emit) — 全量精读 pipeline.rs / events.rs / mod.rs / types.rs(事件结构体)
- `src-tauri/src/qbank_grading/` (4 文件, 772 行, 2 命令, 4 emit) — 全量精读
- 消费端: `src/essay-grading/useEssayGradingStream.ts`、`src/hooks/useQbankAiGrading.ts`、`src/api/memoryApi.ts`、`src-tauri/src/chat_v2/tools/memory_executor.rs`(chat 工具路径)
- 参照实现: `src-tauri/src/translation/` + commit 4ba851bb/8792bbb8
- 交叉核实: `src-tauri/src/vfs/indexing/mod.rs`(index_resource 守卫)、`src-tauri/src/vfs/repos/essay_repo.rs`(投影)

## 数据流摘要

**essay/qbank 流式批改**：前端 hook 生成 `stream_session_id` → invoke `essay_grading_stream`/`qbank_ai_grade`（请求体含全文/图片 base64）→ 后端 `StreamingLLMPipeline` SSE 流 → 每 chunk 走 `emit_data(delta)`（**已是增量协议**，commit 8792bbb8）→ 前端拼接 delta 展示 → 终态事件（complete/error/cancelled）携带权威全量整体替换 → 后端一次性持久化（essay: 单条 INSERT；qbank: SAVEPOINT 内 3 条 UPDATE）。

**memory 写路径**：IPC 命令 / chat 工具 `builtin-memory_*` → 每次新建 `MemoryService`（仅 Arc clone，廉价）→ Fact 类型 smart-write = 内容 embedding API（去重检索）→ LanceDB hybrid_search → LLM 决策 API → 写 SQLite note → **内联 await** `index_immediately`（再次 embedding）→ 后台 spawn 画像刷新（list 15 条）+ 分类刷新（每分类 1 次 LLM）+ 自进化（节流）。

**memory 读路径**：检索**不是**全表拉取——`generate_embedding(query)`（每次检索一次 embedding API，无查询侧缓存；`search_with_embedding` 变体供 unified_search 复用）→ lance_store hybrid_search（向量+FTS 混合，folder/resource_type 过滤，retrieval_k=3×top_k）→ 候选逐条回表（每候选 ~3 次单查）→ 标签加权 + 时间衰减 → 可选 rerank API。文本匹配仅作混合检索的 FTS 半边。

## 发现清单（影响降序）

| # | 位置(file:line) | 模式 | 复杂度/量级估计(假设) | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|----------------------|----------|----------|----------|
| 1 | memory/service.rs:1387-1429,1471; handlers.rs:832-882; chat_v2/tools/memory_executor.rs:812-894; auto_extractor.rs:94-121 | Fact smart-write 每条 3 次网络往返（去重 embedding + LLM 决策 + 内联索引 embedding），且 batch 循环**串行** await | 每条 ≈3 次 API × 0.3-2s ≈ 1-6s；batch 10 条 ≈ 15-60s；auto-extract 每轮对话每候选同链路（假设：候选 1-5 条/轮） | 每次 memory_write_smart / write_batch / chat 每轮(auto) | ① batch 条目以并发度 2-3 并行（幂等键已防重入）；② 单条路径把内联 `index_immediately` 改 spawn（mark_pending 兜底已存在，SLA 影响可接受）；③ 去重 embedding 与单 chunk 索引 embedding 复用（同文本） | 低（纯后端时序）；②会使 write-then-search 有毫秒级窗口，需确认前端无"写后立刻搜"硬依赖 |
| 2 | memory/service.rs:1471,1224,1251,1272,1287(内联索引) vs handlers.rs:884-893 + memory_executor.rs:896-904(再触发) | **双重索引**：write_smart 内联 await 索引后，handler/工具层又对同一 resource_id spawn `trigger_immediate_index`/`index_resource_immediately`；`VfsFullIndexingService::index_resource`(vfs/indexing/mod.rs:1966-2058) 无"已索引跳过"守卫（步骤 0 只复用 units 表同步） | 每 Fact batch 条目多 1 次全量 embedding API + 1 次 LanceDB 重写（假设：笔记 0.5-8KB → 每次 0.2-1s + API 费用） | 每个 write_batch 的每条 Fact | 在 index_resource 入口加守卫：index_state==indexed 且内容 hash 未变则直接返回——单点修复覆盖全部双触发路径 | 低；需保证 UPDATE 路径 hash 变化正确失效 |
| 3 | memory/config.rs:64-70 + service.rs:670-685 + category_manager.rs:78-103,130-188 | balanced 档分类刷新条件 `total % 5 == 0` 在总数恰为 5 的倍数且**不变**时（UPDATE 不改总数）每次写都命中 → 每次写触发全分类刷新 = get_tree 递归查询 + 每分类 list_shallow(50) N+1 + **每分类 1 次 LLM 摘要调用** | 假设 7 个分类（6 种子+1 用户文件夹）→ 每次写 ≈7 次 LLM + ~30-60 查询；持续到总数离开倍数点 | 每次 spawn_post_write_maintenance（每次写/删） | 把 modulo 条件改为单调写计数器（自上次刷新后 ADD 数 ≥5）或复用 evolution 的进程级时间戳节流模式 | 低；纯节流逻辑，输出物（分类文件）不变 |
| 4 | memory/service.rs:778-803 | 检索候选逐条回表：每 lance 候选 `get_note_by_resource_id` + `is_note_in_memory_root` + `get_note_folder_path` ≈3 次单查 | retrieval_k=3×top_k（默认 top_k=5→15 候选，rerank 时 2×）→ 每次检索 ~30-45 查询（本地 SQLite，~5-20ms 总量，非瓶颈但次数多） | 每次 memory_search（chat 工具每调一次） | 一次 `WHERE resource_id IN (...)` 批量取 notes + 一次批量取 location，内存过滤 root 归属 | 低；注意 dedupe/seen 逻辑等价迁移 |
| 5 | memory/service.rs:1972-1993 (list_internal) | 分页取 note_ids 后逐条 `get_note` + `get_note_folder_path`（2 查询/条） | limit=100 → ~201 查询/次；被 memory_list/画像刷新/evolution 扫描/export/anki 复用 → 全模块放大器 | 每次列表/每 200 条扫描页/每次画像刷新 | 单条 JOIN SQL（notes + folder_items + 递归 CTE 拼 folder_path）一次取回全部列 | 中：SQL 重写需保序（updated_at DESC）与 tags 解析等价；建议保留旧函数对照测试 |
| 6 | memory/handlers.rs:582-605 (memory_export_all); handlers.rs:939-999 (memory_to_anki_document) | export: list(500) + 逐条 read = ~501 查询，且 **500 条硬上限静默截断**（功能缺口）；anki: list(≤1000) + 逐条 read ≈1001 查询 + 全量文档字符串一次 IPC | 假设 500 条 × 平均 1KB → ~0.5MB payload + 500 查询（export 低频但用户感知慢） | 用户触发导出/制卡 | 批量 IN(...) 取内容；export 增加分页/条数参数；anki 的 purpose 过滤下推 SQL（tags LIKE） | 低（export 分页需前端 memoryApi.ts:280 同步适配） |
| 7 | src/essay-grading/useEssayGradingStream.ts:239-246; src/hooks/useQbankAiGrading.ts:190-195 | 前端流式 setState 字符串拼接：每 chunk `prev.gradingResult + delta` 全串拷贝 + 每 chunk 一次 React 状态更新/重渲染 | 假设结果 8KB / ~2000 chunk → 累计 ~8-16MB memcpy + 2000 次 setState（打字机 UI 主要卡点之一） | 每次批改流 | ref 缓冲区 + rAF/50ms 定时 flush 到 state（chat 流式 UI 已有同类模式）；终态事件整体替换不变 | 中低：需保证 complete 前最后一次 flush 与超时计时器 reset 语义不变 |
| 8 | qbank_grading/pipeline.rs:386-452 (build_prompts) | 题目全量进 prompt 无上限：question.content/options/answer/explanation + 当前答案 + 最近 5 次作答**完整 user_answer** 均无字符上限（essay 侧有 1000/8000/50000 上限，qbank 一处没有） | 假设主观题答案 1-3KB × 5 历次 + 解析 2KB → prompt 可达 10-20KB，token 成本与延迟线性放大 | 每次 AI 评判/解析 | 对历次答案截断（如每条 ≤500 字，保留最新一条全量）；题目/解析设 5K/2K 上限 | 低；截断策略需保留最新作答全量以免评判失真 |
| 9 | essay_grading/mod.rs:62 + essay_grading/pipeline.rs:85-86 | 批改请求 base64 图片列表全量 clone ×2（mod.rs `request.clone()` 整个 GradingRequest；pipeline.rs 再 clone image_base64_list） | 假设 3 张照片 base64 ≈6-12MB → 每次批改 ~12-24MB 额外 memcpy（不影响网络，只耗内存/拷贝） | 每次多模态批改 | mod.rs 在 move 前先打日志（session_id/round 提前取），request 按值传入；pipeline 内用 `request.image_base64_list.take()` 或引用 | 低；纯 move 语义调整 |
| 10 | memory/reranker.rs:19-33,52-61 | 每次 search_with_rerank 取 `get_model_assignments` 两次（new() 一次、rerank_api() 一次）；MemoryToolExecutor.get_service 在 ctx 缺 lance_store 时每次 `VfsLanceStore::new`（memory_executor.rs:73-78） | 2 次配置读取/检索（若 assignments 走 DB 则 2 查询）；lance store 打开为重操作但通常 ctx 已注入 | 每次 memory_search | reranker 构造时缓存 assignments 结果复用；确保 ExecutionContext 始终携带 vfs_lance_store（A2 范围） | 低 |
| 11 | essay_grading/pipeline.rs:186-199,208-215 | 持久化组装：accumulated.clone() ×3（DB json、complete 事件、响应）+ `parsed_score_json` 刚序列化成 String 又 parse 回 Value（dimension_scores 字段） | 一次性 O(n)，结果 ~4-10KB → 微秒-毫秒级；属清洁度问题非瓶颈 | 每次批改完成 | parsed_score 直接 `serde_json::to_value` 复用；clone 改借用/移动（emit_complete 后不再用 accumulated） | 极低 |
| 12 | essay_grading/mod.rs:217-221 (essay_grading_get_rounds) | 每轮次单独 `get_essay_content`（N+1）；get_round 同模式但单条无碍 | 轮次通常 1-5 → 2-10 查询/次 | 打开会话详情 | get_rounds_by_session_with_conn 内 JOIN resources 取 data 一并返回（_with_conn 变体已存在，易加） | 低 |
| 13 | essay_grading/events.rs:23-34; qbank_grading/events.rs:23-33 | 每 SSE chunk 一次 tauri emit 无合批（delta 已增量，但事件次数 = chunk 数） | 假设 5KB 结果 / ~2000 chunk → 2000 次 IPC 事件（每次 ~几十字节 + 事件循环开销） | 每次批改流 | 可选：仿 chat_v2 `flush_session_chunk_events` 做 16-33ms 合批 flush（带序号保序） | 中：需前端拼接逻辑兼容合批 delta（直接 concat 即可，但需回归打字机节奏） |
| 14 | memory/service.rs:1298-1322 | 不可达死代码：第二个 `if memory_type == MemoryType::Study` 分支（1242 处已 return），700 行函数的克隆噪音 | 零运行时成本 | — | 删除该分支（唯一收益是可读性/克隆面积） | 极低 |

补充核实（不计入发现）：memory 检索**无查询侧 embedding 缓存**——同一 query 短时间重复检索各付一次 embedding API；`search_with_embedding` 已供 unified_search 单次生成多处复用，chat 工具路径未利用（A6/A12 决策是否值得加 LRU）。audit_log.rs:111-114 入库前统一截断 100 字符，无写放大。compressor.rs 有 300 字阈值 + 200 字输出上限。evolution.rs:47-87 进程级时间戳节流（balanced 30min/aggressive 15min），全表扫描复用 #5 的 list N+1，随 #5 修复自动受益。auto_extractor 输入截断 1500+1500 字符，llm_decision prompt 有界（10 条 × 100 字预览）。

## essay/qbank delta 化改动清单（后端+前端消费点）

**状态：已由 commit `8792bbb8 "perf(stream): 三管线流式 emit 改增量 delta 协议——O(n²) 传输降为 O(n)"` 完成，PLAN.md 第 5 节该热点条目已过时（快照时间 2026-09-11 00:10 早于该 commit）。本次审计逐行核实前后端对齐，无残余 O(n²) 线上传输。**

后端（协议：data 只带 delta；error/cancelled 带权威 accumulated；complete 带权威 grading_result/feedback）：
- `src-tauri/src/essay_grading/types.rs:177-235` — GradingStreamData{delta} / Complete{grading_result} / Error{accumulated} / Cancelled{accumulated}
- `src-tauri/src/essay_grading/events.rs:23-90` — emit_data(delta) / emit_complete / emit_error / emit_cancelled
- `src-tauri/src/essay_grading/pipeline.rs:98-101` — on_chunk 直接把 chunk 作为 delta 发出；107/128/211 的 `accumulated.clone()` 属终态权威全量（设计内，非热点）
- `src-tauri/src/qbank_grading/types.rs:82-120`、`events.rs:23-87`、`pipeline.rs:169-172` — 同构；181/189/204/213/231/244/255/339/363 均为一次性终态负载

前端消费点（共 2 个文件，全部监听 `essay_grading_stream_{sid}` / `qbank_grading_stream_{sid}`，无其他订阅者）：
- `src/essay-grading/useEssayGradingStream.ts:236-247` data 拼 delta；`:259` complete 用 grading_result 整体替换；`:274,:295` error/cancelled 用 accumulated 兜底替换
- `src/hooks/useQbankAiGrading.ts:190-195` data 拼 delta；`:204` complete 替换；`:221,:235` error/cancelled 兜底替换
- 取消命令对：`invoke('cancel_stream')`（essay, useEssayGradingStream.ts:360）/ `invoke('qbank_cancel_grading')`（qbank, useQbankAiGrading.ts:298）

**残余可选跟进**（非必须，见发现 #7/#13）：前端 setState 拼接改 ref 缓冲 + rAF flush；后端 emit 合批。二者均为体验微调，不动协议。

## 最小数据传递方案

- **批改流**（已达成 + 微调）：线上只传 delta（一次）+ 终态权威全量（一次）；持久化一次写。剩余可省的是 IPC 事件次数（合批）与前端全串拷贝（ref 缓冲）。
- **memory 检索**：查询 embedding 一次生成多处复用（unified_search 模式推广到 chat 工具聚合场景，或小型 LRU）；候选回表批量化（#4）；folder_path 随批取。
- **memory 写**：Fact 每条最多 1 次去重 embedding + 1 次决策 + 1 次索引 embedding（当前 3 次中索引与去重对单 chunk 同文本可合一）；batch 并行化让总时长从 Σ 变 max；索引去重守卫消灭第二遍。
- **维护链**（画像/分类/进化）：从"每次写全量重算"收敛为"脏标记 + 单调阈值/时间窗触发"；分类刷新的 LLM 调用按脏分类增量（当前每分类无条件重算）。
- **列表/导出**：单 JOIN 批量取回（notes+content+folder_path 一查询），export 真分页。

## 不动清单

- local_api v1.1 stable 端点契约（PLAN 全局约束；本三模块的 memory 命令被 data_gov/local_api 侧引用处不改返回形状）
- delta 事件协议本身（8792bbb8 已定，终态带权威全量的兜底语义是丢包/错序保险，不可去掉）
- memory smart-write 的 LLM 决策/去重语义（ADD/UPDATE/APPEND/DELETE/NONE + 置信度降级 + 幂等键）——功能核心，只动执行时序与并发
- essay 评分解析（PP-2 属性顺序正则、M-058 模式权威 max）与 S-014/M-064 竞态/不完整流防护
- qbank SAVEPOINT 三表原子写入（questions/submissions/正确性计数）
- audit_log 100 字符截断与 record_search_hits 不触碰 updated_at 的设计
- memory_config 表与 tags 编码（`_hits:`/`_last_hit:`/`__cat_*__` 系统前缀）——既有数据兼容
