# A2 chat_v2 handlers+tools 数据传递审计

> 撰写: 2026-09-11 | 基线 commit: ae1e6385 | 对应 PLAN task-007
> 范围: `src-tauri/src/chat_v2/handlers/` (15 文件) + `src-tauri/src/chat_v2/tools/` (35 文件) + 数据装配源头 `chat_v2/repo.rs` / 结果消费腿 `chat_v2/context.rs`（各取与 handlers/tools 直接相关部分）
> 不含: pipeline/events/prompt/persistence（task-006 / A1 负责）

## 范围与方法

方法: 热路径全量精读（load_session / manage_session / send_message / block_actions / search_handlers 全文，repo.rs 关键装配函数），其余签名级扫描 + 3 个并行子任务（serde Value 中转链全量 grep、clone 分布统计、35 个 tool executor 大 payload 逐个核查）。关键发现均回读源码行号实证；来自子任务而未逐一回读的行号已尽量抽验（抽验 5 处全部命中）。

**路径勘误**: 任务书所指 `src-tauri/src/tools/executor.rs` 不存在；工具执行器 trait 层实际就是 `src-tauri/src/chat_v2/tools/executor.rs`（636 行，已全文精读）。`src-tauri/src/tools/` 只有 mod.rs 与 web_search.rs。

**80 个命令分类覆盖**（idx-commands.txt 实测 81 处定义，含 skills.rs 5 个）:

| 类别 | 命令数 | 覆盖深度 |
|------|--------|----------|
| 会话列表/读取 (manage_session, load_session) | 15 | repo 装配 SQL 逐条读 |
| 消息发送/重试/编辑 (send_message) | 5 | 全文精读 |
| 块操作 (block_actions) | 7 | 全文精读 |
| 工作区 (workspace_handlers) | 19 | 全签名 + 8 个数据装配命令精读 |
| 变体 (variant_handlers) | 5 | 数据流扫描（全量加载点 + clone 点） |
| 搜索/标签 (search_handlers) | 6 | 全文精读（结论: 健康） |
| 快照导入导出 (snapshot_handlers) | 3 | 精读 |
| 分组/审批/迁移/OCR/ask_user/canvas | 15 | 签名级（轻量 CRUD 或显式用户动作） |
| 技能 (skills.rs) | 5 | 签名级 |

**量化基线**（本审计实测）: tools/ serde 调用 621 处（chatanki 104 / qbank 77 / builtin_resource 75 / template 63…），handlers/ 41 处；.clone() 计数 top: chatanki_executor 367、send_message.rs 71、template 64、variant_handlers 61、manage_session 56。

## 数据流摘要

**工具腿（每 LLM 工具调用）**: LLM → `ToolCall.arguments: Value`（整体一次 parse，无重复）→ executor 执行 → `ToolResultInfo { input: Value, output: Value }` → 四个出口: (a) `save_tool_block` clone input/output → `to_string` → SQLite UPSERT（稍后 save_results 再 UPSERT 一次，双写）；(b) 事件 `json!({"result": output})` 包装 emit 前端；(c) `context.rs` 转 LLM 消息时 `data_json: output.clone()` + `content: to_string(output)` 双份；(d) 大 payload executor 把 base64/整文件直接塞 output → 同时进 LLM 上下文、事件流、SQLite 三处放大。

**handlers 腿**: 会话列表 = sessions 表分页投影（健康）；会话打开 = `load_session_full` 4 条批量 SQL 但全字段（含每块 tool_input/tool_output/citations 3 个 JSON 列逐块 parse）→ Tauri IPC 整包再序列化；流式期间前端每 ~5s 回调 `chat_v2_upsert_streaming_block` 回传**累积全量内容**（代码注释自认 O(n²) IPC 税）；retry/edit/branch/delete 类命令反复全量加载消息列表。

## 发现清单（影响降序，25 条）

| # | 位置(file:line) | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|-----------------|----------|----------|----------|
| 1 | tools/attachment_executor.rs:248-268, 391-404 | 图片附件/解析失败文档把**原始 base64 整串**放 tool_result content，无上限 | 假设 2MB 照片→2.7MB base64≈70万 token 文本进 LLM 上下文；同串再进事件+SQLite | 每次附件类工具调用 | 图片走多模态 image block 通道或返回 resource_id+尺寸+摘要，text content 只留占位 | 中: 多模态注入路径需核实 provider 侧；LLM 行为变化 |
| 2 | tools/image_generation_executor.rs:446 | 生图结果 = 完整 base64 data URL 塞进 output（图已存 VFS，sourceId 在手） | 假设 1024²PNG 1-2MB→1.4-2.7MB 文本 | 每次生图 | output 只回 sourceId+prompt 摘要+尺寸；前端已有事件通道拿图 | 低: 前端渲染走事件，LLM 只需知道成功 |
| 3 | tools/builtin_retrieval_executor.rs:247-271, 339-355 | 检索结果内联图片 base64 出现**两遍**（url/imageUrl + imageCitation markdown 各一份） | 单图成本×2；RAG 命中多图时线性放大 | 每次 RAG 检索带内联图 | imageCitation 只存 resource_id 引用，UI 侧解析 | 低 |
| 4 | tools/builtin_resource_executor.rs:1148-1198 | `resource_read` 不传 page_start 时**全量**返回文档内容（OCR/extracted_text 无大小上限） | 假设 300 页 PDF OCR 文本 0.6-1.2MB | 每次读资源 | 默认改首页+返回总页数，或加硬上限(如 64KB)+截断提示（同文件 unified_search snippet 截 200 字符已有先例: 1679） | 中: LLM 依赖全文时需自行翻页；工具参数协议形状不变 |
| 5 | tools/executor.rs:381-405 + context.rs:279/285/359/366-368 | 同一 tool output 每调用被 clone 1 次 + `to_string` 3 次（save_tool_block / 事件包装 / LLM content），且 data_json 与 content **双份**进 LLM 消息 | 每工具调用 output 全量 ×4 次搬运；输出越大放大越狠（与 #1/#2/#4 叠加） | 每次 LLM 工具调用（每消息几十次） | 一次 to_string 后复用；data_json 若 provider 侧无人消费则删（**待验证** llm_manager 消费点） | 中: data_json 可能是 provider 协议字段 |
| 6 | tools/qbank_executor.rs:1273-1287 | DOCX 导出把**整个 docx base64** 进 tool_result（file_size 已单列，base64 冗余） | 假设 50 题试卷 30-100KB→base64 40-130KB | 每次导出 | 返回路径/file_size 即可，前端经 VFS 取文件 | 低 |
| 7 | handlers/block_actions.rs:499-608 + 前端 toolCall.ts:430/485/641, TauriAdapter.ts:3265, workspace/events.ts:504 | 流式防闪退保存: 前端每 ~5s 把**累积全量 content** 回声 IPC；后端把 tool_input/output_json **parse 成 Value 再序列化回字符串**入库（无意义往返）；另 ensure_message_exists 整行读 4 个 JSON 列、append_block_id 被调用两次 | 传输 O(L²/interval)（注释自认）；假设 60s 流产出 10KB、12 次保存≈660KB IPC；每次保存 2 次整行读+1 次 UPSERT | 流式期间每 5s×每块 | (a) tool_input/output_json 字符串直接入库不 parse；(b) 前端改增量或改走已有 `persist_streaming_block_internal`(617) 管内路径 | 中: (b) 需前端 B3 同步改 4 个调用点；命令签名可不变 |
| 8 | handlers/block_actions.rs:413-451 | 按 documentId 找 anki 块: `SELECT tool_output_json WHERE block_type='anki_cards'` **无过滤全表**逐行 JSON parse 比对 documentId，命中后逐卡 from_value | 全库 anki 块全文 parse；假设 100 个 anki 块×每块 50 卡 JSON≈每调用数百次 parse + MB 级字符串分配 | 每次前端按文档取卡片 | SQL 下推 `json_extract(tool_output_json,'$.documentId')=?`（SQLite JSON1）或冗余列+索引 | 低: 纯后端 |
| 9 | tools/executor.rs:342-465（各 executor 返回时调用）+ pipeline save_results | 工具块**双写**: save_tool_block 先 UPSERT 全量 input/output，save_results 稍后再 UPSERT 覆盖 | 每工具调用 2 次全量序列化+2 次 UPSERT；MB 级 output 时 SQLite 写放大×2 | 每次工具调用 | 防闪退版只存 status/摘要列，或 buffer 到 save_results 单写 | 中: 闪退恢复粒度下降，需权衡 |
| 10 | handlers/send_message.rs:603-641, 652-664, 678-698（retry）; 1057-1146（edit_and_resend 同型） | 全量加载会话消息（4 个 JSON 列全 parse）**只为取 id 列表**；再逐条 get_message 重读 meta（N+1 重复查询）；再逐条 DELETE 循环 | 假设 500 消息会话: 1 次全量 parse + N 次单行读 + N 条 DELETE | 每次重试/编辑重发 | (a) `SELECT id ORDER BY timestamp,rowid` 投影；(b) resource_ids 从第一次已加载结果提取；(c) `DELETE WHERE id IN (...)` 批删 | 低: 纯后端内部 |
| 11 | chat_v2/repo.rs:1430-1533 + handlers/load_session.rs | 会话打开全量返回所有块所有字段（content+tool_input+tool_output+citations 逐块 parse）→ IPC 整包序列化 | 假设 500 消息×3 块×tool_output 数 KB-MB（与 #1/#2 落库的 base64 联动）= 打开秒级 | 每次打开会话 | 源头截断（#1/#2/#4/#6 修好后自然收缩）；可选: 首屏只带最近 N 条消息的块+惰性加载 | 中: 前端需支持惰性加载才可改形状 |
| 12 | tools/builtin_retrieval_executor.rs:118-121 | top_k 无 clamp（对比 builtin_resource 的 MAX_SEARCH_TOP_K=50） | LLM 传 top_k=1000 时 1000 条 chunk snippet 全回 | 每次检索 | `.min(50)` clamp | 低 |
| 13 | tools/qbank_executor.rs:332/549/618/735/792/849/982/1108/1310/1451/1606/1787 | preview_json 每次操作 `from_value`→内存改→`to_value` 全量往返（含全部 pages/cards） | 假设 50 题×多页: 每操作整包 Value 树拷贝 | 每次 get/answer/grade | 增量更新单题字段，或至少 to_value 只序列化变更页 | 低: 内部数据流 |
| 14 | tools/builtin_resource_executor.rs:2138-2599 | 思维导图编辑: DB 串→parse 成 Value→改→to_string 回写→**再从 DB 读回刚写的同一串**建版本快照 | 整张导图 JSON ×(1 parse+1 write+1 re-read) | 每次导图编辑 | 写后直接用内存中的串建快照，去掉回读 | 低 |
| 15 | tools/template_executor.rs:229-259 | 每次成功构建 output_for_display + output_for_model **两棵完整 JSON 树**，且 `result.clone()` 后整体替换 output（克隆的 model 版白白拷贝） | 双树构造≈2× CPU/内存；clone 复制一份 | 每次 template 工具调用 | display 树延迟构造（仅在需要 emit/落库时）；避免中间 clone | 低: 分流设计本身是好模式，保留 |
| 16 | handlers/send_message.rs:353-360 | 每次发送无条件构造并 emit `chat_v2_request_audit`（唯一消费者是 debug-panel 插件） | payload 小（计数+元数据）但每发必走序列化+IPC | 每次发送消息 | debug 面板打开时才 emit（前端 toggle 或环境变量门控） | 低 |
| 17 | handlers/manage_session.rs:1152-1159, 1334-1379 | branch: 逐块 get_block_with_conn（N+1，事务内）；每块 tool_input/output Value 深拷贝+递归 ID 重映射 | 假设 200 块会话: 200 次单行查询 + 全块 JSON 深拷贝 | 每次用户分支操作 | 复用 `get_session_blocks_with_conn`（已有 JOIN 批量版）一次取齐 | 低 |
| 18 | handlers/manage_session.rs:687-703, 780-801 | 清空回收站/删会话: 为收集 context_snapshot 资源 ID **全量加载所有消息**（含 meta/variants 4 列 parse） | 假设回收站 50 会话×100 消息: 5000 行全列 parse 只为抠 resource_ids | 每次清空回收站/删会话 | 只 `SELECT meta_json` 单列，或 SQL json_extract 收集 | 低 |
| 19 | handlers/manage_session.rs:1418-1441 + repo.rs:1982-2008 | chat_v2_save_session（注释自述流式期间高频）: 每次先 get_session + load_session_state 双读、clone 合并、全列 UPSERT；repo 的 update_session_skill_state_v2 内部又读一遍 state | 状态行小（KB 级）但每 5s 3 读 1 写 | 流式期间周期性 | 状态缓存于 ChatV2State 内存，脏标记批量刷盘；或至少去掉 update_session_skill_state 的内部重读 | 中: 闪退丢最近草稿的窗口变大，需 flush 时机设计 |
| 20 | handlers/manage_session.rs:1068-1089 | rebuild_session_skill_state: 全量加载消息（4 列 parse）只为从尾部找最后一个 skill snapshot | 全会话行 parse，reverse 扫描只是省了遍历 | 删消息/恢复会话时 | SQL `ORDER BY rowid DESC LIMIT` 逐行探查 meta_json 直至命中 | 低 |
| 21 | handlers/snapshot_handlers.rs:125-131 | 分页导出每页**重新全量加载所有消息**再内存切片 | 假设 1000 消息×20 页: 20 次全量 parse | 每页导出 | messages 查询加 SQL LIMIT/OFFSET（排序键与现有 ORDER BY 一致） | 低 |
| 22 | chat_v2/repo.rs:1027-1127 + 1430-1457 | row_to_message 每行 4 个 JSON 列 parse、row_to_block 每行 3 个；`without_skill_runtime_contents` 再做一次克隆变换 | 每会话打开 500 消息+1500 块≈4500 次 JSON parse | 每次全量加载（含 #10/#17/#18 触发点） | 修好上游减少全量加载次数后此项自然缓解；可选 `&raw` str 直通需改类型（不推荐） | 低 |
| 23 | tools/qbank_executor.rs:270-277, 672-685, 1180, 1697-1749; tools/arg_utils.rs:3-24 | 字符串→枚举走 `from_value(json!(s))` 双跳；`from_str(&format!("\"{}\"",t))` 每题 4 次构造+parse；coerce_json_array 对字符串化数组二次 parse 兜底 | 单次微开销（堆分配），但遍布 qbank 导入路径每题×4 | 每次 qbank 过滤/导入 | 枚举写 `FromStr` 直转；format!-parse 换 match 查表 | 低 |
| 24 | tools/academic_search_executor.rs:734-739, 810 | 工具输出 papers 含完整重建摘要（无截断），上限 50 篇 | 假设 50 篇×500 词≈100KB | 每次学术检索 | 摘要截 300 字符（同文件 856-860 的 snippet 截断是现成先例） | 低 |
| 25 | handlers/workspace_handlers.rs:498-513 | workspace_list_all 对每个工作区逐个 is_member_or_creator_session（N+1 权限检查） | 工作区数量级小（<50），实际影响低 | 每次列工作区 | membership 批量查询 | 低 |

**好模式（无需改，供推广参照）**: fetch_executor.rs（max_length/start_index 分页 + 1MB 硬顶）；template 双通道（display/model 分流）；chatanki（tool_result 只回紧凑 status，卡片走事件流，MAX_REF_TEXT_BYTES 上限）；search_handlers（FTS5+limit）；snapshot 导出 IPC 层分页。

## 最小数据传递方案

1. **工具结果单次成形**: executor 产出即区分 `display`(事件/DB) 与 `model`(LLM) 两个视图，`model` 视图在源头限宽——图片一律 resource_id 引用、长文默认首页+翻页参数、检索/导出带 clamp。output 从产生到 LLM 只序列化一次（to_string 复用，data_json 待验证后去重）。
2. **防闪退保存走管内**: 流式块持久化统一走 `persist_streaming_block_internal`（进程内，无 IPC），前端 5s 回声调用点全部退役；工具块 JSON 字符串直通 DB 不 parse。
3. **消息加载按需投影**: id-only 查询服务 retry/edit/branch 的"找位置/删尾巴"场景；resource_id 收集走单列 SELECT 或 json_extract；skill snapshot 重建走尾部 LIMIT 探查。
4. **会话打开分两段**: 元数据+最近 N 条消息的块先行，更早块按滚动惰性拉取（前端虚拟列表本就只渲染可视区）。
5. **SQLite 写路径**: 工具块单写（save_results 唯一落点或防闪退版降级为摘要），anki 查询走 json_extract 索引。

预期收益量级: 单轮重工具会话的 tool_results 从最坏数 MB 文本降到数十 KB；流式 IPC 从 O(L²) 降到 O(L)；500 消息会话的 retry/branch/delete 类操作从"全量 parse×多次"降到 id 投影单查。

## 不动清单

- **LLM 工具参数协议冻结**: 各 executor 接受的参数形状（`resource_read` 的 path/page_start、`fetch` 的 url/max_length/start_index 等）是 LLM 提示词层的契约，只可加可选参数（如默认截断开关），不可改名/改必填/改语义。工具**输出**可瘦身（LLM 只需可读结果），但 anki/qbank/template 前端渲染依赖的 display 通道字段不动。
- **事件载荷中前端已消费字段**: `chat_v2_event_{session_id}` 上 tool_call end 的 result（template display 树、anki 卡片）形状不动——那是前端渲染契约（B3 范围对齐）。
- **`chat_v2_request_audit` 事件本身保留**（debug-panel 诊断用途），只做开关门控不做删除。
- **DB schema 不动**（本审计所有修复草图均无迁移；若实施 #8 的 documentId 冗余列方案才需迁移，优先选 json_extract 无迁移路径）。
- **需前端同步改的消费点**（若实施对应条目）: #7 → `src/features/chat/plugins/events/toolCall.ts:430/485/641`、`src/features/chat/adapters/TauriAdapter.ts:3265`、`src/features/chat/workspace/events.ts:504`、`src/api/chatV2Api.ts:17`；#11 惰性加载 → chat store 的 load_session 消费侧（B3）；#1/#2 若改多模态通道 → 前端无需动（后端 provider 层内改）。
- **save_tool_block 防闪退语义**: 完全去掉双写前需确认闪退恢复 UX 可接受降级；保守方案是只缩 payload 不减次数。
- **chatanki_executor 的卡片事件流与 MAX_REF_TEXT_BYTES=10MB 内部上限**: 现状即正确，不动。
