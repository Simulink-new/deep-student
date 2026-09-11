# A7 dstu+local_api 数据传递审计

> 撰写: 2026-09-11 (CST) | 基线: main@ae1e6385 工作树 | 任务: task-012
> 方法遵循: docs/PERF-AUDIT/PLAN.md §3 七项 rubric + §6 产出格式

## 范围与方法

**路径清单** (全量精读: common/crud/list/content/node_converters/path_utils/exam_formatter + 8 个类型 handlers + local_api 全部 4 文件 + STUDY-DATA-API.md; 抽查: search/trash/folder handlers, export/, types.rs, 涉及的 vfs repo 层):

- `src-tauri/src/dstu/` — 54 个 `#[tauri::command]` (lib.rs 注册数已核实)。精读: `handlers/common.rs` (3268 行, 共享装配层), `handlers/{essay,exam,file,image,mindmap,note,textbook,translation}_handlers.rs`, `handler_utils/{crud,list_helpers,content_helpers,node_converters,path_utils}.rs`, `exam_formatter.rs`。抽查: `handler_utils/search_helpers.rs`, `trash_handlers.rs`, `folder_handlers.rs`, `export/mod.rs`, `types.rs`。
- `src-tauri/src/local_api/` — 3 个 Tauri command + 11 个 HTTP 端点, 4 文件全读 (`mod.rs` 383 / `queries.rs` 749 / `server.rs` 210 / `handlers.rs` 61)。
- 契约基准: `docs/STUDY-DATA-API.md` v1.1 (2026-08-30)。
- 上游 repo 层核实点: `vfs/repos/{translation_repo,essay_repo,textbook_repo,file_repo,review_plan_repo}.rs`, `vfs/ref_handlers.rs:1883`。

**点位分布实测** (grep 口径注明): dstu 直接 rusqlite 调用 (`.prepare/.execute/.query_row/.query_map`) 49 处 — common.rs 20 / search_helpers 12 / trash 6 / content_helpers 6 / list_helpers 3 / folder 2; serde 调用 (`to_string/from_str/from_value/to_value/json!`) 59 处 — types.rs 14 / search_helpers 10 / node_converters 10 / common 6 / content_helpers 4; `.clone()` 206 处。local_api: SQL 28 处 / `json!` 25 处, 无 clone 热点。

## 数据流摘要

**dstu (Learning Hub 后端)**: 前端 invoke `dstu_list` → `common.rs::dstu_list_folder_first` 按模式分支——根目录 = 文件夹 + folder_items 逐项单行反查 + 7 类"未分配"全捞 (各 1000 行内存过滤); 智能文件夹/收藏 = `list_resources_by_type_with_folder_path` 每行再查一次文件夹路径。行数据由 repo 层 JOIN 出来 (翻译 JOIN 全文 content_json), 经 `node_converters` 塞进 `DstuNode.metadata` (翻译带全文、essay 带批改结果), 整个 Vec 过 IPC JSON。内容读写走 `dstu_get_content/dstu_update` 整文档往返 (上限 1MB)。写操作全部"写 + 全行读回 + 双 emit"。大文件 (file/image/textbook 创建) 以 base64 String 嵌 options JSON 过 IPC; 多模态题目集 `dstu_get_exam_content` 逐页读盘 base64 化过 IPC。

**local_api**: brain 机器人 → 127.0.0.1 axum → 限流/鉴权/超时中间件 → handlers (统一 `spawn_blocking` 包裹, 无阻塞问题) → queries.rs 聚合 SQL → JSON 包裹。查询层质量整体良好 (批量 IN 补全、preview 截断 80 字符、聚合下推、零填充在 Rust 侧), 浪费集中在"借道现成 repo 宽查询"处 (essays 带批改全文、textbooks 带书签 JSON、stats 重复聚合)。

## 发现清单 (影响降序)

| # | 位置 (file:line) | 模式 | 复杂度/量级估计 (假设注明) | 触发频率 | 修复草图 | 破坏风险 |
|---|---|---|---|---|---|---|
| D1 | vfs/repos/translation_repo.rs:61-84 + dstu/handler_utils/node_converters.rs:280-282 | 列表携带全文: `list_translations_with_conn` 无条件 JOIN resources 拉 `r.data` 全文 (搜索与不搜索都拉); `translation_to_dstu_node` 再把 sourceText/translatedText 复制进 metadata 过 IPC | 假设 50 翻译/库 × 平均 10KB 全文 = 每次列表多传输 ~500KB (JSON 序列化后再放大 ~1.1×); 与页大小线性 | 每次打开含翻译的文件夹/智能文件夹/收藏/搜索 (dstu_list, dstu_search) | repo 列表查询去掉 content JOIN (仅 search 分支保留 EXISTS 子查询); node metadata 剥离 sourceText/translatedText, 前端打开时走 dstu_get_content | 中: 前端若直接消费列表 metadata.sourceText 需补一次 get_content——B2/B4 核实消费点 |
| D2 | dstu/handler_utils/content_helpers.rs:189-199 | 预加载弃用: 文本类内容读取先 `VfsFileRepo::get_content` 全量加载 blob (盘读+base64 编码+String 分配), 再交给 `extract_file_text_with_strategy` (其策略是 OCR→extracted→解析, 前两者命中时 base64 完全没用) | 假设 50MB PDF: 白读 50MB 盘 + 分配 ~67MB base64 String 后丢弃; OCR 已覆盖的文档 100% 浪费 | 每次 dstu_get_content(textbook/file) — 聊天引用、学习中心打开文档 | 改两阶段: 先查 ocr_pages_json/extracted_text 列, 均空才加载 base64 回退; 或给策略函数传惰性 closure | 低: 纯内部, 返回值不变 |
| D3 | dstu/exam_formatter.rs:212-218 + handlers/common.rs:1647-1662 | 多模态题目集全量 base64 过 IPC + 同步 IO: `dstu_get_exam_content(is_multimodal=true)` 逐页 `std::fs::read` (async 上下文无 spawn_blocking) → base64 → ContentBlock Vec 过 IPC | 假设 10 页扫描卷 × 1MB/页 = ~13MB 单次 IPC payload; 数据随后又被前端送回聊天管线 (盘→Rust→IPC→前端→IPC→pipeline 三次搬运) | 每次向多模态模型附加题目集 | (a) fs::read 包 spawn_blocking (低风险); (b) 架构候选: 管线内直接按 exam_id 解析, IPC 只传引用 | (a) 低; (b) 高: 跨边界改 payload, 需前端+chat_v2 同步适配 |
| D4 | dstu/handlers/common.rs:196-239 + handler_utils/crud.rs:163-219 + list_helpers.rs:345-527 | 根目录列表 N+1 + 全捞后内存分页: folder_items 每项一次单行反查 (fetch_resource_as_dstu_node); 7 个 list_unassigned_* 各捞 1000 行内存 HashSet 过滤; 全部装完后才 sort+truncate (common.rs:304-331) | 假设根 30 items + 7×1000 行捞取 ≈ 40+ 查询/次; O(全库资源行) 内存装配, limit/offset 在最后一步 | 每次打开学习中心根目录 | (a) unassigned 改 SQL `NOT EXISTS (folder_items)`; (b) folder items 按类型分组批量 IN 查询; (c) 排序/截断下推 SQL | 低: 行为等价重构 |
| D5 | dstu/handler_utils/list_helpers.rs:121,164,195,226,247,263,284,304,333 | 智能文件夹路径 N+1: 每行 `get_resource_folder_path` → `ref_handlers.rs:1883 get_resource_path_internal` = folder_items 单查 + 逐级父链 O(depth) | 50 行/页 (默认 limit=50, types.rs:357) × (1+depth≈3) = 150-200 查询/次列表 | 每次智能文件夹/收藏列表 | 一次 JOIN (folder_items×folders) 批量解析建 HashMap, 或直接用 folder_items.cached_path 列 | 低 |
| D6 | dstu/handlers/common.rs:121-179 | 收藏模式 8 表全捞内存过滤: 对 8 种类型各走一次 list (叠加 D1 全文 + D5 路径 N+1), 再内存 retain metadata.isFavorite | 8 类型 × 全表行; notes/files 本有 is_favorite 列却不过滤 | 每次打开收藏视图 | 有列的类型 (notes/files/textbooks/exams 等) SQL WHERE is_favorite; 元数据类暂保留内存过滤但先修 D1/D5 | 低 |
| D7 | dstu/handler_utils/search_helpers.rs:619-708, 756-856 | 搜索 LIKE 全扫 + 双重 N+1: `search_by_index` 对 vfs_index_segments.content_text 做 `LIKE '%q%'` + GROUP BY (普通表, 前导通配无法走索引 = 全语料扫描); 每个 hit 再 get_resource + resolve_source_to_node 两次单查; search_all 8 类型串行 | 语料段数假设 1-5 万段 × 平均 1KB = 每次 keystroke 搜索全扫 10-50MB 文本; hits×2 次单查 | 每次全局搜索 (用户输入) | (a) resolve 批量化; (b) 中期: 该表已有 embedding/lance 基础设施, LIKE 是降级路径——上 FTS5 迁移 | (a) 低; (b) 中: 召回结果集会变 |
| D8 | dstu/handlers/common.rs:3095-3192 | dstu_refresh_path_cache 逐行 O(depth) 父链 + 无事务逐行 UPDATE: 每行 build_folder_path_with_conn (逐级查 folders) + 每行一条 autocommit UPDATE | 假设 1000 items × depth 3 ≈ 4000 查询 + 1000 次 fsync 级 autocommit | 手动触发/同步后 (低频但一次很重) | 一次查全 folders 建内存 map 算路径, 单事务批量 UPDATE | 低 |
| D9 | dstu/handlers/common.rs:2504-2540 | dstu_move_many 无事务 + 每项读回 + 每项双 emit: 循环内 move_item_to_folder (各自 autocommit) + get_resource_by_type_and_id 全行读回 + 2×emit_watch_event/项 (对比 delete_many/restore_many 已用事务) | 100 项 = ~200 查询 + 200 次事件序列化; 拖拽多选时 | 多选拖拽 | 仿 delete_many 事务化; 读回改查 folder_items.cached_path 构造轻量节点; emit 合批 | 中: emit 形状需 A11/B 核对消费端 |
| D10 | dstu/handler_utils/node_converters.rs:128-142 | watch 事件双发: 每个写操作 emit `dstu:change:{path}` + `dstu:change` 两次 (同 payload 序列化两遍) | 所有写操作事件量 ×2; DstuNode 为纯元数据 (无 content 字段, types.rs 核实), 单次不大 | 每次创建/更新/移动/收藏 | A11 emit↔listen 配对核实后, 若前端只听一种则去掉另一种 | 中: 需 A11 结论 |
| D11 | dstu/handlers/common.rs:600-689 + 各类型 handlers set_favorite (image:139-183, textbook:253-294, translation:219-260, essay:173-230, mindmap:149-190, exam:211-252) | 写后全行读回: 8+ 处"UPDATE 后 SELECT 全行再转 node"。essay/exam 读回含 preview_json/grading_result_json 大列 | 每操作 2 查询 + 大列反序列化; 单次 <1ms, 模式性问题 | 每次重命名/收藏/更新 | repo 的 set_favorite/update 返回受影响实体或轻量元数据; dstu_update 的 exam/translation/essay 分支复用 update 返回值 | 低 |
| D12 | dstu/trash_handlers.rs:240-501 | 回收站 8 表 × (limit+offset) 行捞取后内存归并排序分页: 每表 LIMIT limit+offset, 合并后才全局排序+slice | 默认 limit=100 → 装配 ~800 行返回 100; 翻页越深越浪费 | 每次打开回收站/翻页 | 各表 SQL ORDER BY updated_at DESC + 小 LIMIT 归并取 top-K (堆归并早停), 或 UNION ALL 单查询 | 低 |
| D13 | dstu/handler_utils/list_helpers.rs:130-183 | 笔记标签过滤内存分页循环: tags 是 JSON 列无法 SQL 过滤, 按 50-200/页循环拉取内存匹配, 上限 10000 轮 | 标签稀疏时全表分页扫描 + 每轮一次查询 | 按标签筛选笔记 | SQLite `json_each(tags)` 下推 EXISTS 过滤; 或 tags 物化关联表 (需迁移) | 低 |
| D14 | dstu/handler_utils/node_converters.rs:363-368, 475-484 | 列表节点 metadata 冗余: essay_to_dstu_node 带 gradingResult (LLM 批改全文 JSON, 单次数 KB); file_to_dstu_node 的 size/sha256 各重复两份 (node 字段+metadata) | 假设 50 essay × 5KB 批改结果 = 250KB/次列表冗余 | essay 智能文件夹/搜索列表 | 列表投影剥离 gradingResult (dstu_get 单查时保留); 重复字段去重 | 中: 前端若读列表 gradingResult 需核实 (B4) |
| D15 | dstu/handler_utils/list_helpers.rs:325 | MindMap 列表无分页: `list_mindmaps(vfs_db)` 全表, 不吃 limit/offset (其它 7 类型都有) | 全量 mindmap 元数据行 | 智能文件夹/收藏含导图时 | repo 加 LIMIT 参数与其它类型对齐 | 低 |
| D16 | dstu/handlers/{file,images,textbook}_handlers.rs:47/47/49 | 创建路径 base64 String 过 IPC: 整个文件内容嵌 DstuCreateOptions JSON (内存 1.33× + JSON 转义再放大) | 10MB 附件 ≈ 14MB+ IPC payload; 上限受 MAX_CONTENT_SIZE=1MB 之外的 file_data 路径约束 (创建路径无 1MB 校验) | 每次导入文件/图片/教材 | 候选: Tauri asset 协议/前端先写临时文件传路径; 跨边界大改 | 高: 前端适配面大, 列为候选不立即做 |
| L1 | local_api/queries.rs:628 + vfs/repos/essay_repo.rs:63-66 | essays/recent 借道宽查询: list_essays SELECT 含 grading_result_json (批改全文) 但端点只用 id/title/score/dimension/时间 | limit≤50 × 数 KB = 数百 KB 读出即弃 | 机器人每次拉近期作文 | 端点专用窄 SELECT (响应形状不变) | 低: 契约形状冻结不受影响 (内部装配层优化) |
| L2 | local_api/queries.rs:224-234 + vfs/repos/review_plan_repo.rs:837-838 | review/stats 重复聚合: get_stats_with_conn 内部已算 due_today/overdue (repo 用 local_today), handler 又各发一条 SUM 重算 | review_plans 全表聚合 ×2/次调用 | 机器人每次拉统计 | get_stats 接受 caller 传入 today 或拆出仅算所需列的窄查询 | 低: 形状不变 |
| L3 | local_api/queries.rs:464-495 | pomodoro/summary 两次范围扫描: day 聚合与 hour 聚合两条独立 SQL 扫同一范围 | 92 天上限 × 2 次扫描; 表量级小 (万级记录), 实际开销低 | 机器人每次拉番茄摘要 | 合并为一条 `GROUP BY day, hr` 后 Rust 侧归约 day 行 | 低: 形状不变 |
| L4 | local_api/queries.rs:554 + vfs/repos/textbook_repo.rs:434-437 | textbooks/progress 拉冗余列: list_textbooks 返回 tags_json/bookmarks_json 但端点不用 | 200 本 × bookmarks 假设 1-5KB = 数百 KB 弃用 | 机器人每次拉教材进度 | 端点专用窄 SELECT (id,file_name,page_count,last_page,last_opened_at,is_scanned) | 低: 形状不变 |
| L5 | local_api/queries.rs:660-703 | weakness/summary Rust 侧 tag 聚合: 两查询各自拉行后 HashMap 聚合 (未用 json_each 下推) | 错题行数假设 <5k, 单遍内存聚合可接受; 全表扫描 questions (attempt>0 且 is_correct=0) 无索引命中风险待验证 | 机器人每次拉薄弱点 | 可选: json_each SQL 聚合; 收益小, 列为可选 | 低 |

**未列条的已知但不展开**: exam/translation copy 克隆 preview_json/全文 (复制语义本就要求, 频率低); `dstu_restore_many` 发空占位节点 (common.rs:2443, 语义噪音非性能); 所有命令 info 级全量日志 (日志量大但非性能主因); export Binary 路径 base64 过 IPC (export/mod.rs:137-146, 已有 FilePath 大文件旁路)。

## 最小数据传递方案

dstu 理想形态 (按数据只需移动一次的原则):

1. **列表只传卡片, 不传正文**: 列表投影 = id/name/type/时间/size/favorite/preview_type, 全文字段 (翻译 sourceText/translatedText、essay gradingResult) 一律剥离, 打开时走 `dstu_get_content` 单文档往返。配套 repo 层去掉列表查询的 content JOIN (翻译) —— 这是本模块最大的一笔"不需要移动的数据"。
2. **路径批量解析**: folder_items→folders 一次 JOIN 建 map (或信任 cached_path 列), 消灭每行 O(depth) 父链查询; 根目录装配改为"文件夹 + 各类型 SQL 端 NOT EXISTS/LIMIT"三段式, 分页排序下推 SQL。
3. **内容读取惰性化**: blob/base64 只在 OCR 与 extracted_text 均为空时加载 (D2); 多模态题目集图片在聊天管线内按需解析, IPC 传引用 (D3b, 架构候选)。
4. **写操作单趟化**: set_favorite/rename/update 返回轻量元数据 (受影响 id + 新值), 不再全行读回; 批量移动事务化 + 事件合批。
5. **搜索走索引**: 全局搜索内容召回从 LIKE 全扫迁到 FTS5/lance (已有基础设施), resolve 批量化。
6. **local_api**: 11 端点形状冻结下, 只做内部投影收窄 (L1/L4) 与聚合去重 (L2/L3), 查询层已基本是"最小形状"。

## 不动清单

- **local_api v1.1 全部 11 个 stable 端点的请求/响应形状** (四方会话依赖的稳定接口, docs/STUDY-DATA-API.md): health / review/due / review/stats / review/upcoming / review/heatmap / todos / pomodoro/summary / textbooks/progress / exams/recent / essays/recent / weakness/summary。含统一包裹 `{ok,data,serverTime}`、错误码枚举、限流 429+Retry-After、零填充/非零填充语义、`avgReviewSecondsPerItem`/`workDurationSeconds` 口径。L1-L4 均为内部装配层优化, 不触碰形状。
- local_api 鉴权 (常量时间比较)、限流 (60/min)、5s 超时、spawn_blocking 结构——已达标, 不动。
- `dstu_watch`/`dstu_unwatch` 空壳命令 (common.rs:2554-2564)——前端可能仍 invoke, 删除需 B2 核实 invoke 点。
- watch 事件双发 (D10) 暂不动——先等 A11 emit↔listen 配对结论。
- `MAX_CONTENT_SIZE=1MB` 等输入上限与 1MB 语义 (对外可观察行为)。
- DstuNode 字段集 (children/child_count 等保留字段)——前端类型对齐归 B2/B4。
- 既有 SQLite schema——D13 的 tags 物化若做需带迁移, 默认走 json_each 免迁移路线。
