# A3 SQL 数据层审计 (vfs/repos + database + lance_vector_store)

> 撰写: 2026-09-11 | 基线: main @ ae1e6385 | 审计人: task-008
> 前置: PLAN.md 第 3 节 rubric / 第 6 节产出格式

## 范围与方法

| 范围 | 文件数 | 行数 | SQL 执行点 |
|------|--------|------|-----------|
| src-tauri/src/vfs/repos/ | 23 | 28,083 | 760 |
| src-tauri/src/database/ | 2 | 8,938 | 457 |
| src-tauri/src/lance_vector_store.rs | 1 | 4,491 | 107 (含 lance API 调用) |
| 合计 | 26 | 41,412 | ~1,324 |

方法: 全量普查 + 热点精读。
1. awk 扫描 `for/while` 循环体内 25 行窗口出现 `.execute/.query_row/.query_map` → 36 个候选区 (repos 24 + database 12)，逐个精读核实真伪。**核实结果: 约 1/3 为真** (N+1 / 逐行写 / RMW 放大)，其余为 query_map 结果迭代、参数拼装循环或已有事务包裹的误报。
2. grep 普查 `SELECT *` → 仅 13 处且集中在 vfs_embedding_dims / vfs_index_units 等小配置表，无实害。
3. grep 普查 `serde_json::from_str` 行映射 → 31 处，实害集中在"列表查询把大 JSON 列整列拉出后逐行解析"(见发现 #2-#5)。
4. 索引核对: migrations/ 共 42 个 SQL 文件、480 条 CREATE INDEX (444 去重)，逐表对照高频 WHERE/ORDER BY 列。
5. lance_vector_store.rs 全文精读 (向量检索/预热/迁移/删除路径)。
6. 抽样验证: 最高影响发现 (#1-#8, #12) 均由主审复读源码行确认，非仅凭子报告。

分工: repos 由 3 个并行精读代理覆盖，database/ 与 lance_vector_store.rs 由主审精读，Top 发现主审亲核。

## 数据流摘要

三条主链:
1. **VFS 资源链**: 前端列表/详情 → vfs repos (SQLite via VfsDatabase) → `VfsXxx` 结构 (含 extracted_text/preview_json/ocr_pages_json 等大 TEXT 列) → IPC JSON → 前端 store。**列表与详情共用同一宽 SELECT** 是本层最大数据放大源。
2. **向量链**: 双写设计——SQLite 为权威 (rag_documents/chunks + vfs_embedding 状态表)，lance 为检索层 (kb_chunks_{dim} / chat_v2_{dim} 表)。检索走 lance 原生 ANN+FTS (带 limit 与 pushdown filter，设计良好)；但每次检索额外付出 ~12 次 settings 表查询 + 2 次 create_index ensure 往返 + 1 次 settings 双写 (FTS 版本记录)。启动预热把最多 100K 条全维向量灌入一个**从未被读取**的内存缓存。
3. **错题/制卡链**: chat 持久化 (append_mistake_chat_messages_with_context) 单事务批量 UPSERT，设计合格；mistakes 域索引覆盖极全 (480 条 DDL)。回收站清理 (purge_*) 是逐项独立事务的重灾区。

## 发现清单 (影响降序, 上限 25)

| # | 位置 (file:line) | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|----------------|---------|---------|---------|
| 1 | lance_vector_store.rs:517-598 (new()@423-437, cache_cap=100_000@431) | 启动预热扫描把最多 10 万条全维 embedding (假设 1024 维 ≈ 4KB/条 → 400MB RSS) 灌入 emb_cache——**全仓库无任何检索路径读取该缓存**(仅 add/delete/metrics 写入与逐出, grep emb_cache 全量 16 处无 get-for-search)。扫描 query() 无列投影，text 大列一并读出仅丢弃 | O(cap×行宽); 假设 10 万行×(4KB emb+1KB text) ≈ 500MB 磁盘读 | 每次 LanceVectorStore::new()——5 个调用点: 启动维护(lib.rs:785)、cleanup 命令(commands.rs:1984)、统计/优化命令(1513/1540)、迁移(3479) | 删除 spawn_warmup_scan 或给 emb_cache 接上真实消费方；至少 query().select() 只读 embedding+chunk_id | 低(纯删代码，缓存无读者) |
| 2 | file_repo.rs:603-609, 639-645, 683-689; attachment_repo.rs:1489-1497, 1506-1514 | files 表列表查询 SELECT 固定含 extracted_text + preview_json + ocr_pages_json 三个大 TEXT 列；列表视图只需 id/名称/大小/时间 | 假设 50 个 PDF、extracted_text 100KB-1MB/个 → 每次列表拉数十 MB 并逐行 serde 解析，O(N×大列) | 每次文件/附件列表打开与翻页 (folder 视图、选择器、回收站) | 列表路径瘦身 SELECT (3 个大列出列)，详情 get_by_id 按需取；VfsFile 拆 summary/detail 两态 | 中(前端若消费列表中大字段需同步适配) |
| 3 | textbook_repo.rs:1265-1315; file_repo.rs:1428-1447; exam_repo.rs:1403-1431 (调用方 multimodal/page_indexer.rs:879,907,924 逐页调用) | OCR 单页保存 = 读出整本 ocr_pages_json → 解析 → 改 1 页 → 整包重序列化 UPDATE | O(P²) 字节: 假设 300 页×2KB/页，累计读写 ≈ 90MB 序列化+WAL 放大 | 每次 OCR/多模态索引流水线跑一份文档 (逐页) | 批量接口 save_ocr_pages_with_conn (textbook_repo.rs:1332 已存在) 收尾一次落盘，或每 N 页刷一次 | 低 |
| 4 | folder_repo.rs:2514-2546 (build_tree_recursive_all) | 侧栏树构建 N+1: 每个文件夹单独一条 get_folder_items_all_with_conn SELECT；且每层递归全量重扫 all_folders 切片 | 假设 300 文件夹 → 301 条 SELECT + O(N²)≈4.5 万次比较/次构建 | 每次侧栏树加载/刷新 (dstu_folder_get_tree) | 一次 `SELECT ... FROM folder_items WHERE deleted_at IS NULL` 全取后按 folder_id 内存分组建树 (单查询+O(N)) | 低 |
| 5 | translation_repo.rs:68-71 (同型 671-674, 921-927); exam_repo.rs:73-75 (同型 910-911, 1302-1304); essay_repo.rs:62-64 (同型 120-127) | 列表查询整列拉内容 JSON: translation LEFT JOIN resources 取 r.data 全文 (source+translated); exam 取 preview_json (整卷预览)+metadata_json; essay 取 grading_result_json (完整批改结果)，row_to_* 逐行 serde 解析 | 假设 100 条×10-500KB → 1-25MB/次列表+逐行解析 | 高频: Learning Hub 各列表/文件夹视图/回收站/搜索 | 同 #2: 列表瘦列，内容详情接口按需 (get_translation_content 类接口已存在) | 中 |
| 6 | lance_vector_store.rs:2470-2538 (load_rrf_config), 2881-2887, 1012-1103 (ensure_wide_table), 2469-2481 | 每次混合检索的固定税: load_rrf_config 8 次 get_setting + fts_prefilter 1 次 + ensure_wide_table 内 should_rebuild_fts/build_fts_index_builder 再 3-4 次 get_setting + 2 次 lance create_index ensure 往返 + record_fts_version 触发 save_setting **双写** (mod.rs:3546-3560 vfs+legacy 两表) | ~12 次 SQLite 点查 + 1 次双写 + 2 次 lance 元数据往返/每次 RAG 查询 | 每次带 RAG 的对话轮 | rrf/fts 配置进程内缓存 (写时失效); FTS 版本仅在真正 rebuild 后记录; Table 句柄与"索引已确保"标志缓存 | 低 |
| 7 | note_repo.rs:556-566 (scoped 652-661) | 笔记搜索 WHERE 含 EXISTS (resources.data LIKE '%q%')——每个候选笔记把全文从磁盘读出做前缀通配 LIKE，无法用索引 | 每次搜索读全部活跃笔记正文; OCR 逐页笔记用户语料可达数十 MB | 每次带关键词笔记搜索 | FTS5 虚表 (questions 已有 questions_fts 先例)，或搜索限标题列 | 高(需迁移+行为对齐) |
| 8 | todo_repo.rs:987-992 (reorder_items); folder_repo.rs:261-295 (execute_reorder_in_batches ≤100 走逐行分支) | 拖拽重排序循环逐行 UPDATE，函数内无 BEGIN/SAVEPOINT——N 次隐式事务 = N 次 journal fsync，中途失败留半重排状态 | N=列表长度 (数十); 数十次 fsync/次拖拽 | 每次拖拽排序 | 外层包一个事务 (对照 todo_repo.rs:1448-1459 停靠重排的正确写法)；>100 分支已用 CASE WHEN 单语句，统一即可 | 低 |
| 9 | lance_vector_store.rs:2113-2157 (list_all_chat_message_ids); 2032-2087 (existing_chat_message_ids); 457-493 (count_lance_rows_sync); 344-413 (summarize_library) | 全表扫描无列投影: 只需要 message_id/count，却 query().execute() 读全部列 (embedding 4KB/行 + text)。list_all 被 CleanupEmbeddings 用于孤儿检测 | 假设 10 万聊天向量×(4KB+1KB) ≈ GB 级读/次清理; count_lance_rows_sync 全行流式仅为计数 | 清理命令触发; count/summarize 被统计命令调用 | query().select(["message_id"]) 列投影; 计数用 lance count_rows (count_chat_embeddings:2103 已是正确写法) | 低 |
| 10 | file_repo.rs:1462-1481; note_repo.rs:1222-1225; attachment_repo.rs:2372-2383; textbook_repo.rs:946-963 | 清空回收站逐项独立事务: 每文件/笔记/附件/教材各自 BEGIN IMMEDIATE…COMMIT，N 次 fsync；attachment purge 入口还先 list 全宽列 (含 #2 三大列) 只为拿 id，且硬编码 LIMIT 1000 超出静默漏删；file 路径每项先 get_file 全宽列回读 | 假设回收站 100 项 → 100 次 fsync + 100 次大列全读 | 用户清空回收站 | 外层单事务 + SELECT id only + 修 1000 上限 (分页循环) (对照 mindmap_repo.rs:1195 合规写法) | 中(purge 链需回归测试) |
| 11 | database/mod.rs:4939-4944 (delete_sub_library) | 删分库循环逐文档 DELETE rag_document_chunks——N+1; 且只删 SQLite chunks，**绕过 lance 侧 delete_chunks_by_document_id**，kb 表向量成为孤儿 | N 文档×2 语句; 孤儿向量永久残留 lance 表污染后续检索 | 删除含文档的分库 | `DELETE ... WHERE document_id IN (SELECT id FROM rag_documents WHERE sub_library_id=?1)` + 接入 lance 清理 | 中 |
| 12 | resource_repo.rs:640-644 (update_resource_data_with_conn); 183-190 (create_or_reuse get_by_hash 拉 data 列只为 .id); 517-579 (list_by_type/search 固定含 data 列) | 为比较 hash 把旧全文整行拉出 (导图 JSON 可达 MB 级——与已修"导图快照放大"同域双重放大)；去重命中路径同理；资源列表同病 | 每次导图保存 2×全量读 (叠加 mindmap_repo.rs:468-525 重复取旧文+二次哈希); 假设 100KB-1MB/次 | 每次导图保存/内容重复保存/chat 资源列表工具 | SELECT hash 单列比较; SELECT id for hash 命中; 列表窄列化 | 低 |
| 13 | mindmap_repo.rs:468-525 (update_mindmap) | 内容更新链重复计算: get_resource_with_conn 拉旧全文 → compute_hash(新) → update_resource_data_with_conn 内部再 get 全文一次 + 再 compute_hash 一次 (resource_repo.rs:637-644)，外加 normalize 全量 JSON DOM 往返 | 每次保存: 旧内容 2×读、新内容 2×SHA-256、1×DOM roundtrip | 每次导图编辑保存 + LLM 构建导图每轮工具调用 | update_resource_data 加变体: 调用方传入已算 hash，跳过二次取行与二次哈希 | 低 |
| 14 | review_plan_repo.rs:396-422 (list_due_reviews); question_repo.rs:1090-1119 (list) | COUNT(*) + SELECT 双查询: 同一 WHERE (含 FTS MATCH/json_each 标签子查询) 执行两遍 | 到期集合 O(due) 扫两遍; 假设活跃用户 5k-5 万计划，到期占比高时每次复习页加载 2×全扫 | 复习页加载 + local_api study-data 端点 (可能被外部轮询); 题库每次翻页/搜索 | 取 limit+1 行推断 has_more 替代精确 COUNT (前端语义需同步)；或 COUNT(*) OVER() 窗口一次完成 | 中(local_api v1.1 契约冻结, total 字段形状不能动) |
| 15 | folder_repo.rs:2116-2142 (get_all_resources) | 逐资源 ≥4 查询: title 查两次 (build_resource_info 与 build_resource_path_with_conn 各一次同参查询)、每资源各跑一次向上递归 CTE 算文件夹路径 (同文件夹兄弟重复计算)、resource_id 再一条 | 假设子树 50 资源 → ≥200 条查询 + 50 次 O(深度) CTE | 每次聊天上下文注入 (dstu_folder_get_all_resources) | 按类型 IN 批量取 title/resource_id; folder_path 按 folder_id 记忆化; 删重复 title 查询 | 低 |
| 16 | resource_repo.rs:443-455 (decrement_refs_with_conn) | 循环逐 id decrement_ref_with_conn (UPDATE+SELECT 回读成对, autocommit 无事务) | 假设消息引用 10 资源 → 20 条独立事务语句 | chat 消息发送/编辑/删除/回滚的资源清理 | 一条 UPDATE ... WHERE id IN (...) 包事务；回读改 RETURNING 或省略 | 低 |
| 17 | folder_tree_helper.rs:163-231; path_cache_repo.rs:113/156/194/251/313/342/394 | ensure_table_exists 类函数在每次读写/失效前执行 3 条 CREATE TABLE/INDEX IF NOT EXISTS DDL——运行期纯重复 schema 检查 | 每次文件夹 rename/move 的失效路径固定 +3 条语句; rebuild 启用则每条 set_path +3 | 每次文件夹重命名/移动/删除 | DDL 移到 VfsDatabase 初始化/迁移执行一次; 三阶段批量失效合并为单条带子查询的 DELETE | 低 |
| 18 | database/mod.rs:3185-3206 (append_mistake_chat_messages_with_context) | 同一事务内对 mistakes.updated_at 冗余写 3 次 (3186 updated_at+last_accessed, 3195 updated_at 再写, 3201 无消息分支再写)，另每次调用跑 pragma_table_info 检查 stable_id 列 (2820) | 每轮对话保存 +2 条冗余 UPDATE + 1 次 schema 内省 | 每次聊天持久化 flush | 合并为单条 UPDATE；列存在性检查缓存到 OnceLock | 低 |
| 19 | database/mod.rs:3427-3452 (backfill_turn_metadata 同款 241-264, 每次调用必跑@3192); repair_unpaired_turns:3418-3452 | 孤儿 assistant 绑定循环: 每个 assistant 一次含 NOT EXISTS 相关子查询的候选查询 (每次全量扫该 mistake 的 user+assistant 行) | 稳态 2 条空 SELECT/次保存 (可忽略); 首次回填/修复 U×A 次 O(U+A) 扫描 | 每次保存(空扫) / 修复工具调用(病态) | 稳态短路: 先 COUNT 未配对数，为 0 直接返回 | 低 |
| 20 | blob_repo.rs:398-436 (cleanup_unreferenced) | 循环逐 blob remove_file+DELETE+INSERT 队列无事务; 入口 SELECT WHERE ref_count=0 无索引全表扫 (blobs 含全部 PDF+渲染页) | O(无引用 blob 数) 次独立写; 全表扫 | 维护/清理操作 (低频) | 包事务 + blobs(ref_count) 部分索引 WHERE ref_count=0 | 低 |
| 21 | review_plan_repo.rs:1004-1038 (get_calendar_data) | WHERE DATE(rh.reviewed_at) 对每行做函数计算 (非 sargable)，idx_review_history_time 无法用于范围 | O(全部历史行) 每次渲染; 假设 1 万+ 历史记录 | 日历热力图打开 | reviewed_at 与 'YYYY-MM-DDT00:00:00' 前缀范围比较，或落 reviewed_date 生成列+索引 | 中(本地时区语义需保持) |
| 22 | exam_repo.rs:1024-1044 (update_import_state/save_checkpoint, 调用方 question_import_service.rs:3100 约 20 处) | 导入 checkpoint 每批次把含 page_blob_hashes/待处理队列的完整状态 JSON 序列化 UPDATE 整列，状态随页数增长 | 假设 100 页×20 次写入×(P×80B) → 数 MB 累计序列化+WAL 放大, O(B×P) | 每次题目集导入会话 | 仅阶段边界或每 N 页写 checkpoint | 低 |
| 23 | database/mod.rs:5796-5799 (enhanced_anki_list_document_sessions) | 每次调用尝试 `ALTER TABLE document_tasks ADD COLUMN source_session_id` (失败静默丢弃)——运行期 DDL 尝试 | 每次会话列表 1 次 schema 写尝试+解析 | 每次制卡任务页加载 | 列检查结果缓存，或移入启动迁移 | 低 |
| 24 | mindmap_repo.rs:1037-1065 (purge 版本资源); essay_repo.rs:1029-1042 (purge 孤儿资源) | 循环内每资源 COUNT(引用)+可能 DELETE——N+1 (均有外层事务包裹，非 rubric-3 违规，纯语句数) | 每版本资源 ≤4 语句; 构建期快照数百版本 → 单次 purge 千级语句 | 永久删除/清空回收站 (低频) | 两条 COUNT 合并为一次 GROUP BY 汇总，DELETE 用 IN 批量 | 低 |
| 25 | lance_vector_store.rs:2159-2247 (rows_to_retrieved) | 检索结果水合: 每个唯一 document 一次 SELECT active_revision 点查 (函数内 memoize, 跨调用不复用); metadata_json 每行解析两次 (2206 取 revision + 2226 再解析一次) | 每次检索 ≤ fetch_limit (≤1000) 行 → ≤500 唯一 doc 点查 + 2×行数 JSON 解析 | 每次带 RAG 的对话轮 | doc→revision 用一次 IN 批查; metadata 解析结果复用 (一次 from_str 取两个用途) | 低 |

死代码备忘 (不计入发现): database/mod.rs:2720 fetch_chat_history_summary 全仓库零调用 (拉 image_base64/content 全列)；folder_repo.rs:226 execute_delete_in_batches 零调用; path_cache_repo.rs:721 `let _title = get_resource_title_with_conn(...)` 结果被丢弃。rebuild_all/rebuild_folder 链 (path_cache_repo) 目前无生产调用方，一旦接线即成 #4/#15 同款 N+1。

## 缺索引清单

总体结论: **索引覆盖异常完善**——migrations/ 480 条 CREATE INDEX (444 去重)，vfs 各资源表的 WHERE/ORDER BY 列基本全覆盖 (含大量 device 同步前缀索引与部分索引)。实缺仅 3 处，均为低频路径:

| 表 | 缺失索引 | 受影响查询 | 量级 | 迁移要求 |
|----|---------|-----------|------|---------|
| blobs | 部分索引 `(ref_count) WHERE ref_count = 0` | blob_repo.rs:398+ 清理入口 SELECT | blobs 行数 = 全部 PDF+渲染页，全表扫 | 新增 Refinery V 脚本, IF NOT EXISTS 幂等 |
| mindmap_versions | 复合 `(mindmap_id, created_at DESC)` | mindmap_repo.rs:408-414 取最新自动快照 (当前: mindmap_id 过滤后内存排序) | 每导图版本数 ≤百级, 收益小 | 同上 |
| translations | 单列 `created_at DESC` | 列表 ORDER BY created_at (vfs schema 只有 (is_favorite, created_at) 复合, 旧 mistakes 库有 idx_translations_created) | 列表排序; 行数千级, 收益小 | 同上; 注意勿与 UNIQUE 约束冲突 |

结构性说明 (非索引可解): review_plans 到期排序 `next_review_date ASC, is_difficult DESC, consecutive_failures DESC` 混合方向无单一 B-tree 可覆盖 (发现 #14 的根源之一); notes/exam/essay 搜索的 resources.data LIKE 前缀通配 (发现 #7) 只能 FTS5 解。

## 最小数据传递方案

理想形态 (功能不变前提下):
1. **列表/详情分离**: 所有资源列表查询 (file/attachment/translation/exam/essay/note/question/resource) 改为窄 SELECT (id/title/类型/时间/状态 + 小型 JSON 标量)，大 TEXT 列 (extracted_text/preview_json/ocr_pages_json/data/grading_result_json/r.data) 仅详情接口按需单行取。这一条消掉发现 #2/#5/#10/#12 的主要字节量——是本层"最少数据传递"的核心。
2. **OCR 流水线批量落盘**: page_indexer 内存累积页 OCR，按批调已存在的 save_ocr_pages* 批量接口 (发现 #3)，消除 O(P²)。
3. **树构建单查询**: 文件夹树/资源注入改为 1-2 条全量 SELECT + 内存分组/记忆化路径 (发现 #4/#15)。
4. **向量层去税**: 删无人读的 emb_cache 预热 (发现 #1); rrf/fts 配置进程内缓存; lance 全表扫描全部加列投影或 count_rows (发现 #6/#9)。
5. **写路径事务化**: 重排序/清空回收站/引用递减统一外层事务 (发现 #8/#10/#16)。
6. **统计口径**: 分页 total 改 limit+1 推断或窗口函数 (发现 #14)，SQLite 权威数据不再双算。
7. 每次改动独立 commit: 列瘦身后若前端确有列表消费大字段的页面，同步改调详情接口 (过 tsc)。

## 不动清单

- **rag 双写架构** (SQLite 权威 + lance 检索)——设计意图，动它牵动迁移/回滚链。
- **save_setting 双写** (vfs app_settings + legacy mistakes.settings, mod.rs:3546 注释标明的过渡期迁移)——有明确退役计划，本审计只缓存其读侧 (发现 #6)。
- **local_api v1.1 stable 端点契约** (list_due_reviews 的 total/has_more 字段形状, local_api/queries.rs)——#14 的修复必须保持响应形状。
- **schema 变更全部走 Refinery V 脚本** (V{YYYYMMDD}__*.sql, 旧 CURRENT_DB_VERSION=41 系统已冻结禁止递增, manager.rs:79-101); 缺索引清单 3 项均按此新增, 存量数据不动。
- **旧迁移代码** (manager.rs migrate_to_version 906-2559, ensure_compatibility)——仅为未升级用户保留, 标记废弃, 不做性能改造。
- **purge 链的软删除/引用计数语义** (blob 引用计数、folder_items 级联、rag FK CASCADE)——事务合并可以，删除顺序与语义不能变。
- 他人未提交的脏文件 (工作树共享; git status 中 repos/ 若有他人改动, 实施时只 add 自己改的文件)。
