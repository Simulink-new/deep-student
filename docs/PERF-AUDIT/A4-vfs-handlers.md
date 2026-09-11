# A4 vfs handlers 数据传递审计

> 撰写: 2026-09-11 08:05 CST | 基线 commit: ae1e6385 | 任务: task-009 (campaign-2)
> 审计员通读 + 3 路子代理全文精读; 关键行号经人工抽查与 idx-emits.txt 交叉验证

## 范围与方法

**路径清单**(代码只读):
- `src-tauri/src/vfs/handlers/` 全部 13 文件: file/attachment/index/pdf/ref/mindmap/note/ocr/multimodal/resource/debug/todo/pomodoro
- 索引链路: `vfs/index_handlers.rs`(根级)、`vfs/index_service.rs`、`vfs/indexing/`(mod.rs 4568 行等)
- `src-tauri/src/vfs/pdf_processing_service.rs` (4488 行, 全文精读)
- 辅助: `vfs/todo_handlers.rs`、`vfs/ref_handlers.rs`(根级)、`vfs/ocr_storage_handlers.rs`、`vfs/types.rs`(类型形状)
- 前端调用侧抽查(定频率): `api/vfsFileApi.ts`、`api/vfsRagApi.ts`、`hooks/usePdfLoader.ts`、`features/learning-hub/apps/views/*`、`components/QuestionBankEditor.tsx`、`services/multimodalRagService.ts`

**不审**(边界): `vfs/repos/` 与 `vfs/database/` 的 SQL 实现细节归 A3(本文涉及 SQL 时只从"handler 契约/返回形状"角度记录); `lance_store.rs` 归 A3; `multimodal_service.rs` 归 A9。

**命令分组覆盖**(129 个 `#[tauri::command]`, idx-commands.txt 口径):

| 分组 | 文件 | 命令数 | 审计方式 |
|---|---|---|---|
| 文件/资源 CRUD | file_handlers(5) resource_handlers(5) ocr_storage_handlers(5) | 15 | 通读 |
| 附件 | handlers/attachment_handlers.rs | 9 | 子代理全文 |
| 引用/路径 | 根级 ref_handlers(3) + handlers/ref_handlers(2) | 5 | 子代理全文 |
| 索引/搜索 | handlers/index_handlers(25) + 根级 index_handlers(8) + indexing/(服务层) | 33 | 子代理全文 |
| PDF/媒体命令 | handlers/pdf_handlers.rs | 10 | 通读 |
| PDF 流水线服务 | pdf_processing_service.rs | (非命令) | 子代理全文 |
| 多模态 | handlers/multimodal_handlers.rs | 5 | 通读 |
| todo/pomodoro | todo_handlers.rs (pomodoro 为 re-export) | 26 | 通读 |
| 导图/笔记/OCR透视/调试 | mindmap(11) note(6) ocr_handlers(3) debug_handlers(7) | 27 | 通读+子代理 |

emit 点 26 处(idx-emits.txt)全部覆盖: handlers/index_handlers 10 + multimodal 2 + indexing/mod 4 + pdf_processing_service 10。

**量化假设基线**(下文"假设"统一指): 教材 PDF 20-100MB/200-300 页; 页位图 150DPI A4 PNG 1-3MB(压缩后 JPEG 100-300KB); 库内 1000-5000 资源; 文件夹 500 项; 题库 100-1000 题; OCR 文本 2-5KB/页。

## 数据流摘要

**读取链(最大流量)**: 前端打开 PDF/图片 → `invoke(vfs_get_attachment_content)` → 后端读 blob 文件/resource.data 整体 base64 编码 → JSON 字符串过 IPC → 前端 `atob`/dataURL 再解码。字节流被搬运 4-5 次(磁盘→Vec→base64 String→JSON→JS string→decode), 放大 1.33×+。同族命令还有 `vfs_get_file_content`、`vfs_get_blob_base64`、`vfs_get_pdf_page_image`(逐页)。讽刺的是 `vfs_get_blob_pdfstream_url` + `pdfstream://` 自定义协议已存在且只回一个路径字符串, 但内容命令全部仍走 base64。聊天引用解析(`vfs_resolve_resource_refs`)是叠加态: 每条引用返回全文 OCR + 逐页 base64, 一次发送 5-30MB JSON。

**写入链**: 前端 FileReader → 整文件 base64 → `vfs_upload_file` 参数 → 后端 decode → `content.clone()` ×2(spawn_blocking 载荷 + 文档解析) → blob 存储。`vfs_upload_attachment_by_path`(后端直读磁盘)已存在但内部仍 base64 编码一圈再委托。上传后异步触发 PDF 流水线。

**索引/事件链**: PDF 流水线(渲染→压缩→OCR→向量)逐页 emit 进度, 但**每 tick 同时同步 UPDATE files 进度字段**, 且 PDF 路径每个事件双发(media-processing-* + pdf-processing-* 兼容); OCR checkpoint 每页把累积全文 clone+序列化+写库(O(n²))。索引构建是资源级全量重建(编辑即全部 chunk 重新调 embedding API); 文本搜索 `search_fts` 实为 LIKE 全表扫(前端当前无活跃调用者)。列表/状态类命令普遍把 `extracted_text/preview_json/ocr_pages_json` 三个大列整列带出。

todo/pomodoro/note/mindmap 子系统是干净的薄封装(分页/分离 content/无 emit), 无需改动。

## 发现清单

| # | 位置 | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
|---|---|---|---|---|---|---|
| 1 | handlers/attachment_handlers.rs:611-624 (`vfs_get_attachment_content`) | 整文件 base64 过 IPC; 前端消费: usePdfLoader.ts:174、TextbookContentView.tsx:343、ImageContentView.tsx:88、FileContentView.tsx:245/314、QuestionBankEditor.tsx:717 | 假设 20MB PDF → 27MB base64 String → JSON 序列化再拷贝 ≈ 54MB+ 瞬时, 每次打开全量 | **每次打开 PDF/图片/附件查看**(主内容通道) | 复用现有 `vfs_get_blob_pdfstream_url`+`pdfstream://` 协议返回 URL, 前端 `<img src>`/pdf.js 直接流式加载; 保留命令仅给多模态注入用 | 中: 6+ 个前端视图需适配, 建议分视图灰度 |
| 2 | vfs/ref_handlers.rs:861-900, 1613-1758 (`vfs_resolve_resource_refs` 多模态块) | 引用解析返回整图 base64 + 逐页 PDF 图片 base64 + 全文 OCR 于同一 JSON | 假设 20 页 PDF×压缩页 200KB → base64 267KB/页, 常见 1-5 ref ≈ 5-30MB/次; 50 ref 上限理论数百 MB | **每次发送带引用的消息** | 多模态块只传 blob_hash+page_index, 前端按需经协议懒加载; OCR 文本仅传截断预览+hash | 大: 前端 formatToBlocks/注入链需同步改造, 单列排期 |
| 3 | handlers/file_handlers.rs:195-197, 322, 376 (`vfs_upload_file`) + handlers/attachment_handlers.rs:392-399 (`by_path` 内部 base64 绕圈) | 上传走 base64 String 入参; decode 后 `content.clone()` ×2(spawn_blocking L322 / DocumentParser L376); by_path 本不过 IPC 仍做 1.33× 编码再委托 | 假设 50MB PDF: IPC 参数 66MB + decode 50MB + clone ×2 = ~216MB 峰值内存/次 | 每次上传(拖拽/附件) | 拖拽场景(已有本地路径)强制走 by_path 且内部改 `upload_from_bytes(Vec<u8>)`; spawn_blocking 载荷改 Arc<[u8]> 共享免 clone | 低-中: by_path 内部改字节直传零形状变化; upload 通道换路需前端配合 |
| 4 | handlers/index_handlers.rs:1457-1614 (`vfs_get_all_index_status` CTE) | 状态面板查询把 `ocr_pages_json/extracted_text/preview_json` 大列整列拉进 CTE 仅为算 LENGTH/has_ocr | 假设 5000 文件×200KB 大列 ≈ 1GB 逻辑读/次(SQLite 无列裁剪) | 每次索引状态面板刷新(vfsUnifiedIndexApi.ts:177) | CTE 内只 SELECT 表达式(`LENGTH(ocr_pages_json)`、`ocr_pages_json IS NOT NULL`)下推, 或把 ocr_count 物化为小列(SQL 归 A3, 契约本文记) | 低: 返回形状不变 |
| 5 | pdf_processing_service.rs:2951-2968 (emit_progress 每次附带 UPDATE; 调用点 :2156/:3616 等) | 进度事件通道上每 tick 同步写库 + PDF 路径双事件发射 | 300 页新 PDF: 压缩 300 tick + OCR 300 tick, 每 tick 1 次 UPDATE+2 次 emit ≈ **600 次 DB 写 + 1200 次 IPC**; WAL fsync 压力+UI 事件洪泛 | 每 PDF 每页 ×2 stage | 节流: 进度 ≥5% 或 ≥250ms 才 emit; DB 进度只在 stage 切换时写 | 低: 前端进度条粒度略降 |
| 6 | pdf_processing_service.rs:3579-3602 (OCR checkpoint) | 每完成一页把累积 OCR 全文 `all_results.lock().clone()` + 序列化 + UPDATE files(经典 O(n²) 累积) | 假设 1MB 总 OCR×300 页: 累计序列化+写 ≈ 150MB, CPU O(N²) 字节 | 每 PDF 每页完成 | 按 page_index 增量 UPSERT 到独立 checkpoint 表, 或每 K 页/5s 才写快照 | 低: 断点恢复语义不变 |
| 7 | pdf_processing_service.rs:1859-1865, 2091-2103 (页位图压缩) | 位图 bytes→base64→(跨函数)→decode→JPEG→base64→decode, 4 次 encode/decode 全缓冲 | 每页 3MB → ~12MB 纯搬运; 300 页 ≈ 3.6GB 累计内存带宽 | 每图片/每 PDF 页 | 给 `adjust_image_quality_base64` 增加 bytes 直传版本 `(&[u8])→Vec<u8>`, blob 本就是 bytes | 低: 纯内部函数签名 |
| 8 | pdf_processing_service.rs:1863-1865, 2093 (图片压缩调用点) | CPU 密集(解码+Triangle 缩放+JPEG 编码 20-200ms/页)在 tokio worker 同步执行, 无 spawn_blocking | 4 并发×100ms 阻塞 worker; 核少时 runtime 饥饿拖慢 chat 流 | 每图片/PDF 页 | 包 `spawn_blocking` 或专用有界线程池 | 低: 返回类型不变 |
| 9 | vfs/ref_handlers.rs:184-221 (`resolve` 每 ref 串行 6-10 查询) + indexing/mod.rs:4114-4154 (enrich 每结果 get_resource+软删检查) | 引用解析 N+1 | 50 ref ≈ 300-500 次同步查询+数 MB 磁盘读; 搜索 top_k=10 → 每次搜索 20 查询 | 每次发消息/每次搜索 | per-type 查询合并 JOIN; enrich 改 `IN (...)` 批查 | 中: 8 张表 SQL 合并, 按类型渐进 |
| 10 | vfs/ref_handlers.rs:77-100, 445-482 (文件夹引用) | count 与 refs 两次全量拉 folder_items 再逐条查询, 截断在内存做 | 假设 500 项文件夹: count 500-1000 查询 + refs 再全量拉一遍, 只为返回 ≤50 条 | 每次文件夹拖入引用 | count 下推单条 SQL(UNION 分桶); refs 查询加 LIMIT | 低: 返回结构不变 |
| 11 | indexing/mod.rs:3517-3529 (`reindex_resource`) | 资源级全量重建: 先删光 Lance 向量+segments 再全量重嵌, 无 chunk 级增量 | 假设 500 chunk 文档每次编辑 → 500 次 embedding API 重调(费用+延迟) | 每次资源编辑/手动重建 | chunk 内容 hash 对比, 只重嵌变化 chunk(需 schema 加 chunk_hash, 带迁移) | 中: 增量正确性需回归测试 |
| 12 | indexing/mod.rs:2294-2308, 2543-2556, 3373-3387 | 循环内逐 chunk `conn.execute` INSERT, 每行重新 prepare, 无显式事务批 | 500 chunk PDF → 500 次编译+插入(每条独立提交) | 每次索引构建 | 预编译 statement 复用 + 单事务包裹批插 | 低: SQL 层细节(A3 协同) |
| 13 | indexing/mod.rs:3443-3512 (`index_exam_questions`) | 最多 1000 题**串行**逐题 embedding API 往返, 无批嵌入 | 假设 100 题×300ms RTT ≈ 30s; API 本身支持 Vec 批量 | 每次题库索引 | 题目文本攒批调 `call_embedding_api`(其签名本收 Vec) | 中: 需重构 unit 写入顺序 |
| 14 | indexing/mod.rs:2058+2137 (同一 ocr_pages_json 双重解析) + index_service.rs:165 (`get_resource_units` 整行含 text_content 仅为取元数据) + pdf_processing_service.rs:3172-3249 (向量 stage 整行载入三大列再三字段 clone) | 大 JSON/大列整载入后大部分丢弃 | 假设 2MB ocr_pages_json ×2 次解析 + 数 MB 整行 ×2 峰值/文件 | 每次索引/每流水线 | 解析一次传 `&PdfPreviewJson` 两用; 投影版 get_file 只取需要列; UnitBuildInput 用引用 | 低-中: 两路径 fallback 格式需合并测试 |
| 15 | vfs/ref_handlers.rs 全文 + handlers/attachment_handlers.rs + file_handlers.rs:709 + pdf_processing_service.rs:1089/1101/4258/4294 | async fn 内同步 rusqlite + `std::fs::read`(含 100MB 整本 PDF); ref_handlers.rs:1811 还有实时解析整份 base64 PDF 的回退 | 100MB 同步读秒级阻塞 tokio worker; 解析回退同样秒级 | 对应各命令每次调用 | 命令体整体 `spawn_blocking` 包裹(by_path 命令已做对, 可作模板); 重 IO 前先还连接 | 低: 返回类型不变 |
| 16 | pdf_processing_service.rs:806+1052(主流水线 conn 跨分钟级 OCR stage), 3535(页任务 conn 跨 OCR API await), 1858(conn 跨 tokio::fs::read await) | 连接池连接跨 await 长期持有 | 4 并发任务+主流水线各占 1 conn, 池小则全局饥饿(代码他处已有"池饥饿修复"注释佐证) | 每流水线 | conn 用完即 drop(块作用域); 页任务查出 blob 路径后先还 conn 再调 OCR | 低 |
| 17 | handlers/index_handlers.rs:1098/:1127/:1151 + indexing/mod.rs:2983 (auto_ocr_page) | 索引/OCR 进度 emit 无节流(小 payload ~200B) | 全量重建 5000 文件×~4 批 ≈ 2 万次 emit; 300 页 PDF OCR=300 次 | 每资源/每 OCR 页 | 进度节流(≥1% 或 ≥250ms); auto_ocr_page 每 N 页发一次 | 低 |
| 18 | handlers/debug_handlers.rs:183-186, 226-274 (`vfs_debug_index_status`) | 全表诊断序列化返回且 `resource_id` 参数被忽略(签名有, 正文不过滤); sample 又 clone 前 15 条重复发 | 假设 1000 资源 ≈ 200-400KB JSON + 每行 6 个子查询 ≈ 6000 次 | 每次打开调试面板/刷新 | 尊重 resource_id 过滤; 默认只返回 sample+统计, all_resources 分页 | 低: 前端调试页确认无全量依赖 |
| 19 | repos/file_repo.rs:603-613(经 `vfs_list_files` handlers/file_handlers.rs:636)+ pdf_handlers.rs:655 (`vfs_list_pending_pdf_processing`) | 列表命令投影含 `extracted_text/preview_json/ocr_pages_json` 三大列 | 假设 100 文件×平均 100KB 大列 ≈ 10MB/次列表 | 每次文件列表/启动 pending 扫描 | 列表走摘要投影(去三大列), 详情按需拉; SQL 属 A3, 返回契约此处记录 | 中: 前端是否直接用列表结果里的 preview_json 待 B2/B4 核实 |
| 20 | handlers/pdf_handlers.rs:325-415 (`vfs_get_pdf_page_image`) | 每页一调: 整页 base64 返回 + 每次解析**整个** preview_json(200-300 页)找一页 hash; 前端 MarkdownRenderer 有 dataUrl 缓存 | 每页假设 200KB → 267KB base64; preview_json 解析 O(pages)×每次 | RAG 引用页图首次渲染每页一次 | 与 #1 同路: 返回 blob 路径走协议; preview_json 解析结果可按 resource 缓存或 SQL 抽出单页 hash | 中: 前端 dataUrl 缓存链需同步 |
| 21 | pdf_processing_service.rs:3993-4045 (create_ocr_note 逐页 2 条写) + handlers/debug_handlers.rs:905-1028/1104-1174 (clear_media_cache 逐行 UPDATE 无事务, ~4000 条独立提交) + pdf_processing_service.rs:2803-2818 (recover_stuck_tasks 逐文件 json_extract N+1) | 维护路径逐行写/逐行查无批量化 | 300 页 PDF 完成 → 601 条顺序事务; 清缓存卡 UI 数秒 | 每 PDF 完成/每次清缓存/每次启动 | 批量 INSERT+单事务; recover 改单条 `WHERE json_extract(...) IS NULL` | 中: decrement_ref 副作用顺序需确认 |
| 22 | vfs/ref_handlers.rs:1853-1866 (escape_xml 5 趟 replace 分配) + indexing/mod.rs:452-484 (extract_markdown_text 每次 `Regex::new`×15) | 每请求重算: 多趟字符串分配/正则重复编译 | 假设 200KB OCR 全文: escape 5×200KB 瞬时分配/块; 每条笔记 15 次正则编译 | 每引用块/每笔记索引 | escape 单趟扫描 builder; regex 改 `once_cell::Lazy` 静态 | 无: 纯函数可测试锁定 |
| 23 | vfs/ref_handlers.rs:681+714 (base64_content.clone() 进 parts 再 join) | 大 String 深拷贝链 | 假设 2MB 图 → 2.67MB base64 ×3-4 份拷贝 | 每次发送含图引用 | content_parts 持有所有权 push 而非 clone | 低 |
| 24 | indexing/mod.rs:3727-3758 (`search_fts`) | 名为 FTS 实为 `LIKE '%'||?||'%'` 全表扫 vfs_index_segments | 假设 200k segments×512 字符 ≈ 100MB 文本扫描/次查询; **但前端 vfsSearch 当前无调用者**(MULTIMODAL_INDEX_ENABLED 门控链) | 待验证(若启用即时搜索则每击键一次) | 建 FTS5 content= 外部内容表(迁移+同步维护)或路由到 Lance hybrid | 中: 需迁移; 先确认调用链再排期 |
| 25 | handlers/multimodal_handlers.rs:22-33, 148-158 (`vfs_multimodal_index` 的 `image_base64` 入参) | 命令签名允许整本页图 base64 数组塞进单次 invoke 参数; `MULTIMODAL_INDEX_ENABLED=true` | 假设 200 页×200KB 压缩页 base64 ≈ 53MB 单次 IPC 参数(若调用方传图); 当前活跃调用链未确认(vfsIndexResource 无 feature 级调用者, 待 B2) | 待验证 | 强制 blob_hash 直读(签名已有该字段), image_base64 标记 deprecated | 低: 后端忽略字段即可, 前端无需立刻改 |

## 最小数据传递方案

该模块理想形态(功能不变, 数据只移动一次):

1. **字节流零过 IPC**: 所有文件/页图/图片内容走 blob 文件系统 + `pdfstream://` 自定义协议(基础设施已在: `vfs_get_blob_pdfstream_url`), IPC 只传 hash/路径/元数据。base64 仅保留在多模态 API 请求组装点(后端读 blob 后直拼, 不过前端)。上传方向: 拖拽场景传路径走 by_path(内部 bytes 直传), 无路径场景才 base64。
2. **列表=摘要, 详情=按需**: 列表/状态类命令统一摘要投影(去三大文本列), `preview_json/ocr_pages_json/extracted_text` 由专门命令按资源拉取; `vfs_get_all_index_status` 只传表达式结果。
3. **索引增量**: chunk_hash 对比跳过未变 chunk 的重嵌; 题目/文本攒批发 embedding API; 写入预编译+事务批。
4. **事件=状态不=持久化**: 进度事件节流(时间/百分比), DB 进度只在 stage 边界写; OCR checkpoint 增量 append; PDF 双事件收敛为单事件+前端兼容映射(需查两套监听是否都在用)。
5. **重活进阻塞池**: 图片压缩/整本读/引用解析整体 spawn_blocking; 连接池连接不跨长 await; base64 中转改 bytes 直传。

## 不动清单

- **todo/pomodoro 26 命令**(todo_handlers.rs): 薄封装直达 repo、无 emit、无 clone 热点、按 list/日期过滤合理——审计未发现可改进点。
- **note/mindmap/ocr_storage/resource handler CRUD**: 薄封装、分页默认值合理、content 与元数据已分离(VfsMindMap/VfsMindMapVersion 均不含 content 大字段)。
- **OCR 透视命令返回全文**(vfs_get_resource_ocr_info / vfs_get_resource_text_chunks): 用户要看内容本身, 是功能需要非传递浪费(低频)。
- **write_gate 检查**: 正确性机制, 非 perf。
- **`dstu:change` 事件**(pdf_processing_service.rs:4065/4074): DSTU 协议契约, 归 B2/A7 协调。
- **pdf 流水线事件 payload 形状**: 已是小结构(stage/percent/ready_modes), 设计正确——问题只在频率与写库副作用(发现 #5/#17), payload 本身不动。
- **local_api v1.1 stable 端点**: 不在本模块, 但若优化波及(经 vfs 读数据)需保持契约冻结。
- **SQL 列裁剪/索引的具体实现**(发现 #4/#10/#12/#19/#24): 归 A3(vfs repos+database)排期, 本文只锁定契约侧需求。
