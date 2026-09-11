# A10 根级服务 + 遗留命令模块 数据传递审计

> 撰写: 2026-09-11 08:17 CST | 基线 commit: ae1e6385 | 审计人: A10 (task-015)
> 范围: 根级大文件 + cmd/ + mcp/ + cloud_storage/ + research/ + crypto/

## 范围与方法

**路径清单** (行数为 wc -l 实测):

| 文件/目录 | 行数 | 审计深度 |
|-----------|------|----------|
| src-tauri/src/commands.rs | 5,900 | 全命令清单 + 12 个重点段精读 |
| src-tauri/src/question_import_service.rs | 3,953 | 主流程精读 + checkpoint/正则/事件专项 |
| src-tauri/src/document_parser.rs | 3,612 | 结构扫描 + 缓冲模式精读 |
| src-tauri/src/notes_exporter.rs | 3,052 | 结构扫描 + 3 处 read_to_end 上下文精读 |
| src-tauri/src/streaming_anki_service.rs | 2,452 | emit 模式 + 逐卡落库精读 |
| src-tauri/src/question_bank_service.rs | 2,275 | 未逐行（qbank 命令面在 commands.rs 已覆盖） |
| src-tauri/src/notes_manager.rs | 2,247 | N+1 模式扫描 + Lance 迁移段精读 |
| src-tauri/src/question_sync_service.rs | 2,156 | 冲突检测/解析/批量段精读 |
| src-tauri/src/models.rs | 2,118 | 大字段结构快速过（base64 中心形状汇总） |
| src-tauri/src/cmd/ (11 文件) | 7,783 / 169 命令 | 全目录 emit+payload 扫描, notes/textbooks/ocr/mcp 精读 |
| src-tauri/src/mcp/ (12 文件) | 5,148 / 11 emit | stdio_proxy 全读, 其余扫描（后端 MCP 已熔断） |
| src-tauri/src/cloud_storage/ | 3,882 | 命令面 + 前端调用配对核实 |
| src-tauri/src/research/ | 788 | 命令面扫描（实体在 commands.rs 内） |
| src-tauri/src/crypto/ | 514 | 只看不建议动（安全边界） |

**优先级覆盖说明** (按任务指令): ① commands.rs 与 cmd/ 的 IPC 边界模式 → ② streaming_anki / question_import / question_sync 的流式与批量模式 → ③ document_parser / notes_exporter 缓冲模式 → ④ mcp / cloud / research 快速过。两个专项核实（附件三段往返、命令名重复）先行定位、最后交叉验证（lib.rs 注册表机器查重 + 前端消费链逐点读码）。

**未派生子代理**，全程串行。前端侧仅追 read_file_bytes/cloud_storage/qbank_source_images 等本范围命令的消费链（完整前端审计归 B 系列）。

## 数据流摘要

**commands.rs (5,900 行)**: 杂烩遗留命令层。健康的部分已成主流: CSV 导出/备份/research zip 全部"路径直写文件"；textbooks_add 收 `Vec<String>` 路径后端处理; get_learning_heatmap 用每数据源一条 GROUP BY 聚合查询(非逐日 N+1); notes_list 走 VFS 元数据(不含全文)。遗留重模式集中在三处: ① read_file_bytes (2221) 返回 `Vec<u8>` 过 JSON 序列化(3-4× 膨胀)，前端 8 处消费; ② qbank_get_source_images (5672) 一次返回整套试卷所有源页的 base64 data_url; ③ 导入链 ImportQuestionBankRequest.content 为整份文档 base64 String。另有 PDF OCR 旧命令组(save_pdf_to_temp 等 4 个)前端已零调用。

**question_import_service (3,953)**: Visual-First 导入管线。前端把整份 PDF/DOCX base64 传进来 → DOCX 检测阶段整份解码一次、正式导入路径再解码一次 → 逐页/逐图 VLM → 逐块 LLM 结构化 → 逐题入库。事件侧已是增量(QuestionParsed 只带单题, 制卡事件洪水确认已修); 剩余热点是 checkpoint 写放大: `save_checkpoint` 把整个 ImportCheckpointState(含全部已完成 VLM 页结果+已结构化批次)序列化成 JSON 落库, 且在逐图/逐页/逐块循环内调用, O(n²) 字节写入 + O(n²) serde CPU。`<<IMG:n>>` 系列正则 5 处内联 `Regex::new` 编译(其中 1 处在逐消息路径)。

**streaming_anki_service (2,452)**: 流式制卡。SSE 流 → 行缓冲 → 每凑齐一张卡: 解析→去重(唯一索引, 无 N+1 查询)→insert(每卡一 autocommit)→emit 单卡事件。emit 模式已修好(直接发 StreamedCardPayload 单卡, 无全量 accumulated 回声)。剩余问题仅每卡一个事务的 fsync 代价(流式 UX 权衡)。

**question_sync_service (2,156)**: 同步冲突层。冲突检测用 HashMap 索引(O(n)), 解析走 SAVEPOINT 事务, prepared 语句不在循环内。仅 batch_resolve_conflicts 逐条各开一个 SAVEPOINT(冲突数通常个位数)。健康。

**document_parser (3,612)**: 全内存解析器(可接受——桌面单文档场景), 但 7 处 `bytes.to_vec()` 把整个文件再拷贝一份仅为包 `Cursor`(而 `Cursor<&[u8]>` 本就实现 Read+Seek), 纯浪费 O(file_size)。

**notes_exporter (3,052) / notes_manager (2,247)**: 导入 zip 逐附件 read_to_end→write 属固有; assets 写库在事务内。notes_manager 的循环内逐笔记取 content 是一次性 Lance 迁移(有 flag 门控), 不算热点; notes_list 实际不带全文(VFS 版 content 为空串)。

**cmd/ (7,783, 169 命令)**: emit 仅 6 处且全是低频进度事件, 无事件层问题。payload 问题: ocr 测试命令 base64-only; notes_save_asset base64(剪贴板粘贴固有)。notes/textbooks 命令面有 meta/advanced 分页, textbooks_add 是路径直传模范。

**mcp/ (5,148)**: 后端 MCP 已熔断(get_mcp_status 恒 available:false), stdio_proxy 每消息 emit 小 payload(双层 JSON 包裹, 可接受)。当前构建基本休眠。

**cloud_storage/ (3,882)**: 主备份链路 cloud_sync_upload/download 收 zip_path 路径直传+流式上传+进度事件, 干净。cloud_storage_get/put 是 `Vec<u8>` 双向过 JSON 的遗留命令面, 但前端 putFile/getFile 封装**零调用**——死面。

**research/ (788) + crypto/ (514)**: research zip 流式写文件, 干净; crypto 全部密钥边界操作, 见不动清单。

## 发现清单

影响降序。量级估计均注明假设。

| # | 位置(file:line) | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|----------------|----------|----------|----------|
| 1 | src/features/learning-hub/resourceDropImport.ts:119 + src/dstu/api.ts:294 | **附件三段往返残存**: read_file_bytes(Vec\<u8\>→JSON number[] 3-4×)→new File(未挂 path)→dstu.create→fileToBase64(FileReader +4/3×)→IPC 回传 | 假设 5MB 图片: 峰值 ≈5MB(后端)+20MB(IPC JSON)+6.7MB(base64) ≈ 4× 净膨胀, 3 次跨界 | 每次拖图片/媒体文件进学习中心 | fileFromLocalPath 构造 File 时挂 attachNativeSourcePath; dstu.create 增加可选 sourcePath 走后端按路径读盘(复用 uploadAttachmentByPath 同型命令) | 低: 前端两处适配, 后端加一条命令 |
| 2 | commands.rs:997/1036 + ExamSheetUploader.tsx:479-489 | 整份文档 base64 String 过 IPC(ImportQuestionBankRequest.content) | 假设 20MB PDF: 27MB JSON 字符串一次性序列化+传输+反序列化 | 每次题目集导入(含 OCR 图片模式, content 为 JSON 数组亦同) | request 增加可选 file_path: 桌面端路径直传后端读盘; content:// 等 URI 走 unified_file_manager 物化; 保留 base64 兼容 | 中: 命令签名扩展+前端调用侧适配, 移动端路径需验证 |
| 3 | question_import_service.rs:3100 save_checkpoint, 调用点 1647/721/1319 等(21 处, 循环内) | checkpoint 写放大: 全状态(全部 VLM 页结果+已结构化批次+全文)每步整量序列化落库 | O(n²): 假设 50 页 PDF、每页 VLM 结果 3KB、100 题每题 1KB → 累计 ≈4-10MB SQLite 写 + 同量级 serde CPU | 每次导入的每页/每图/每块 | 增量化: vlm_page_results/structured_batch_results 拆分表按行 UPSERT, 或节流(每 N 项/每 2s 一存+阶段边界强制存) | 中: 断点恢复读路径需同步改, 需兼容已有 import_state_json 数据 |
| 4 | question_import_service.rs:318-327 (docx 检测) vs 333-340 (正式导入) | DOCX 整份 base64 解码两次(检测含图→再走导入路径各自 decode) | 假设 10MB DOCX: 多一次 13MB→10MB 解码+分配 | 每个 DOCX 导入 | 检测阶段返回 bytes 传给导入路径复用(改函数签名传 Vec\<u8\> 而非再解 request.content) | 低: 纯内部重构 |
| 5 | commands.rs:5672-5741 qbank_get_source_images | 一次返回题目集全部源页 base64 data_url | 假设 10 页×2MB 截图: ≈27MB base64 过 IPC JSON | 每次打开 ImageCropDialog 裁图对话框(ImageCropDialog.tsx:81) | 拆成按 blob_hash 单张惰性命令; 或返回 VFS file_id 列表走既有 vfs_get_attachment_content 通道+前端按需取 | 低: ImageCropDialog 单点适配 |
| 6 | document_parser.rs:2248/2436/2622/2685/2723/2775/2814 (7 处) | `bytes.to_vec()` 全量拷贝仅为包 Cursor; Cursor<&[u8]> 本就实现 Read+Seek | O(file_size) 额外分配+拷贝/次, 假设 5MB xlsx → 每次 +5MB | 每次解析/编辑 xlsx/pptx | 删 to_vec, 直接 Cursor::new(bytes); extract_pptx_as_spec→extract_pptx_from_bytes 链同理 | 低: 签名 &[u8] 已满足 |
| 7 | useSessionLifecycle.ts:174-187 | read_file_bytes→JS 分块手搓 base64→data URL(图片分析会话) | 每图 3-4×(IPC)+4/3×(JS base64)+O(n) 字符串拼接 | 每次发起图片分析 | 新后端命令收路径数组直接返 data URL(后端 base64 一次), 或走 VLM 通道收路径 | 低: 单调用点 |
| 8 | useTextbookCover.ts:98-101 | 整本 PDF 字节过 IPC 仅为 pdfjs 渲染第 1 页封面 | 假设 30 本×20MB: 全量字节过 IPC 仅为首页; 会话内有 cache 但不持久 | 打开学习中心教材视图(每次会话首渲染) | 后端用既有 PDF pipeline 渲染首页缩略图存 VFS blob, 前端拿小图 | 中: 需后端渲染管线接线, 缩略图尺寸/清晰度需对齐 |
| 9 | PdfReader.tsx:178 + TextbookContentView.tsx:371 | 整 PDF/文件字节 Vec\<u8\>→JSON number[] 过 IPC(3-4×) | 假设 20MB PDF → 60-80MB JSON 传输+双端解析 | 每次打开 PDF 阅读/教材预览(有大小阈值门) | 自定义 tauri protocol 按路径流式供 pdfjs fetch(注意历史坑: asset protocol 对中文/空格路径 fetch 失败——需 encodeURIComponent 或 custom scheme); 或 tauri.ipc.request 二进制通道 | 中: Windows 路径编码是已知坑, 需回归中文路径用例 |
| 10 | UnifiedDragDropZone.tsx:449 + useTauriDragAndDrop.ts:265 | 拖拽校验链全文件读字节(类型/大小校验+File 构造), 上传实际走路径直传 | 每文件多一次全量 3-4× IPC 读取, 假设 10MB×5 文件 → ~150MB 瞬时 JSON | 每次拖拽文件入任何 drop zone | 校验只需 get_file_size+扩展名(已有); File 构造改懒(仅在预览组件需要时读), 或 drop zone 直接传路径 | 中: 下游消费 File 对象的链路需梳理(预览缩略图依赖) |
| 11 | cloud_storage/mod.rs:152-166 + cloudStorageApi.ts:192-217 | cloud_storage_get/put Vec\<u8\> 双向过 JSON(3-4×); **前端封装零调用(死面)** | 无运行时成本(死代码), 但属危险接口形状 | 无(未被调用) | 删除两条命令+两个前端封装; 若未来启用须改路径模式 | 无(确认无动态拼接调用后) |
| 12 | commands.rs:343-369 pin_images | base64 图片数组原样 JSON 存 settings 表(跨启动恢复全量读回) | 假设 3 图×3MB base64: settings 表单行 ≈12MB, 启动恢复读+parse | 每次 pin/每次启动恢复 | 存 VFS blob hash 列表, 恢复时按需加载 | 中: 旧数据迁移(启动时把存量 base64 转 blob) |
| 13 | commands.rs:669/689/833 (init_pdf_ocr_session/upload_pdf_ocr_page/save_pdf_to_temp) | PDF OCR 旧前端渲染管线命令组, 前端零调用(死命令, base64 整本+逐页 base64 形状) | 无运行时成本 | 无 | 确认无动态命令名拼接后删除(新路径 start_pdf_ocr_backend 已路径直传) | 低: 待验证移动端无隐式调用 |
| 14 | cmd/ocr.rs:604/676-692 test_ocr_engine | base64→解码→写临时文件→适配器再从路径读回(三段-lite) | 单图 4/3×+两次盘 IO | 设置页 OCR 引擎测试(低频) | 增加可选 image_path 参数(translation.rs:74 已是 path/base64 双模模范) | 低 |
| 15 | question_import_service.rs:1084/2867/2881/2934/3007 + notes_manager.rs:271 | `Regex::new` 内联编译(<<IMG>> 系列 5 处+sanitize 1 处, 1084 在逐消息路径) | 每次编译 ≈10-100μs × 每题/每块/每消息 | 每题(3007/2934)、每块、每错误日志 | `static RE: LazyLock<Regex>` 收敛 | 无 |
| 16 | streaming_anki_service.rs:1279-1282 | 逐卡 insert 各一 autocommit(每卡一次 fsync) | 假设 100 卡/文档: 100 次 fsync, WAL 下仍有序列化点 | 每张流式卡 | 保持 emit 即时, 落库改 2s/N 卡批量事务(崩溃丢失窗口由 Truncated 断点语义兜底) | 中: 流式中断恢复语义需验证; 收益有限建议缓做 |
| 17 | cmd/notes.rs:685 notes_save_asset | 笔记附件保存仅收 base64(粘贴固有, 文件拖入也可走此道) | 粘贴场景必须 base64; 文件场景多 4/3× | 笔记编辑器贴图/传附件 | 增加可选 path 分支(仅文件来源) | 低 |
| 18 | commands.rs:554 read_debug_log_file | 整日志文件 String 过 IPC(无截断) | 假设日志 10MB: 单次 10MB String | 调试面板"完整复制"(低频) | 增加 max_bytes/尾部 N 行参数 | 低 |

**已核实为健康/已修的模式**(不计入发现, 供汇总引用): CSV 导出路径直写(commands.rs:1317); export_unified_backup_data 已瘦身(3622, 仅 settings+API 配置); research zip 流式写(4218); textbooks_add 路径直传(cmd/textbooks.rs:113); get_learning_heatmap 每源一条 GROUP BY(5413); notes_list 不含全文(notes_manager.rs:1911); qbank 同步 HashMap+SAVEPOINT; streaming_anki 事件增量(2098-2216); question_import 事件增量(699 单题); cmd/ 仅 6 处低频进度 emit; cloud_sync_upload zip_path 直传+加密在临时文件完成后流式上传。

## 专项核实结论

### 1. 附件三段往返 (read_file_bytes → File → base64 → 回传)

**结论: 一半已修, 一半仍在。**

- **已修(聊天主上传链)**: InputBarUI.tsx:707-737 / AttachmentUploader.tsx:252 / useAttachmentContextRef.ts:194 均先取 `getNativeSourcePath(file)`, 有本地路径走 `vfsRefApi.uploadAttachmentByPath`(路径直传, 内容不过前端), 无路径(剪贴板)才 FileReader→base64。useTauriDragAndDrop.ts:283-285 拖拽即挂 File.path。此链 PLAN.md 热点表所记"待核实"项可标 ✅。
- **仍在(learning-hub 资源拖拽导入链)**: resourceDropImport.ts:116-124 `fileFromLocalPath` 用 read_file_bytes 构造 File 但**不挂原生路径** → importAttachmentFiles(:351) → attachmentDstuAdapter.create → dstu.create(src/dstu/api.ts:294)→ `fileToBase64` → base64Content 过 IPC 回后端。完整三段。
- **改"按路径直传"的具体落点**: ① resourceDropImport.ts:121 构造 File 后补 `attachNativeSourcePath(file, path)`(工具已存在 fileManager.ts:87); ② src/dstu/api.ts create 分支: options 增加 sourcePath, 有路径时调用后端"按路径创建附件"命令——可复用/对齐 chat 链的 uploadAttachmentByPath 后端实现(vfs 附件表同源); ③ 虚拟 URI(content://)保持现路径回退。
- 其余 read_file_bytes 消费点(单程读取, 无回传): 拖拽校验两处(#10)、图片分析(#7)、教材封面(#8)、PDF 阅读/预览(#9)、debug 面板——见发现清单。

### 2. 命令名重复 (get_mcp_config / import_mcp_config / export_mcp_config / test_mcp_*)

**结论: 误报, 无运行时重复。**

- grep 看到的"×2"实为 **`#[cfg(feature = "mcp")]` / `#[cfg(not(feature = "mcp"))]` 互斥对**(commands.rs:4088-4146 三对; cmd/mcp.rs:60-173 test_mcp_* 四对)——同一构建只编译其一, 是特性开关的兜底桩, 不是重复注册。
- lib.rs 注册表(898-1796 行)中 `crate::commands::test_mcp_*`(1154-1157) 经 commands.rs:66 `pub use crate::cmd::mcp::*;` 再导出, 与 `crate::cmd::mcp::test_rmcp_streamable_http`(1166) 等**指向不同函数的不同名字**。
- 机器验证: 提取注册表全部 730 个命令名 `sort | uniq -d` → **零重复**。PLAN.md §2 该横切问题条目应更正。
- 机制说明(备未来真出现重复时): tauri generate_handler 生成按命令名的 match, 若真重复注册, **首个注册生效**, 后续同名为不可达死代码(rustc unreachable pattern 警告)——不存在"后者覆盖前者"。
- 可选卫生清理(非必须、无性能收益): 把 cfg 兜底对收敛为单定义+内部 cfg 分支(mcp_stdio_start:174 已是这种写法, 可作模板)。

## 最小数据传递方案

本范围理想形态(功能全部保留):

1. **字节不过 IPC**: 文件内容一律以"路径/URI"为 IPC 载荷, 读盘在后端。需落地的按优先级: ① learning-hub 拖拽附件路径直传(修 #1, 补齐 chat 链已有的模式); ② 题目集导入 file_path 分支(#2); ③ 裁图源图按需单张(#5); ④ PDF 阅读走自定义 protocol(#9, 收益最大但风险也最高, 放最后)。`read_file_bytes` 命令本身保留(移动端 content:// 与小文件工具场景), 但每个消费点要么有路径分支要么有大小门。
2. **状态按增量持久化**: 导入 checkpoint 从"整状态 JSON 每步重写"改为行级 UPSERT(或节流), 使写放大从 O(n²) 降 O(n)——数据只写一次。
3. **解码/拷贝只做一次**: DOCX 检测与导入共享一次 decode(#4); document_parser 去 to_vec(#6); 正则静态化(#15)。
4. **派生数据后端生成+缓存**: 教材封面缩略图由后端渲染一次存 blob(#8), 前端只拿小图; pin 图存引用不存 base64(#12)。
5. **死面清除**: cloud_storage_get/put、PDF OCR 旧三命令确认后删除(#11/#13)——数据不再有"根本不移动"的通道残留。

## 不动清单

- **crypto/ 全模块**(密钥派生/加密格式/rotate/master key)——安全边界, 审计只读; backup_crypto 的全量内存加密属备份正确性要求, 不因性能改动。
- **AnkiConnect Rust 转发层**(cmd/anki_connect.rs)——task-004 已裁决保留。
- **local_api v1.1 stable 端点形状**——本范围命令若被 local_api 复用(如 qbank 系列)改签名时须保持端点冻结(全局约束)。
- **流式制卡"逐卡即发"的 UX 语义**——#16 只可改落库批量, emit 时机不可动(前端可见行为)。
- **write gate 接线**(check_qbank_write_gate/check_commands_write_gate 各命令入口)——任何签名重构必须保留调用。
- **cfg(feature="mcp") 兜底桩机制**——可收敛写法但不删除兜底(无特性构建需可编译)。
- **既有 import_state_json 存量数据**——checkpoint 增量化(#3)必须带读兼容或一次性迁移, 不得丢断点。
