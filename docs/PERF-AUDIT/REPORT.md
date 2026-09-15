# 全项目数据传递审计汇总报告 (REPORT)

> 2026-09-11 08:4x CST | 基线 cec44506 | harness campaign-2 task-023
> 覆盖: 17 批次并行审计 (A1-A11 后端 11 + B1-B6 前端 6), 详见各分报告; 本文只做汇总、对账与裁决

## 0. 战役收官 (2026-09-15, task-041 终验)

**审计 → 优化全战役完成**: 44 任务中 43 完成 0 失败(唯一遗留 task-040 拖拽链直传——等他会脏文件合流后执行,非本战役缺陷)。优化阶段共 **20 个 perf commit**(215a0482..0b88d40a), 终验 cargo check 0 error + tsc 0 error。

落地要点(按六原则归档):
1. **字节不过 JSON 通道**: 六视图主内容通道迁 pdfstream://(协议 blobs 定向放宽)+附件路径直传(聊天主链既有)+base64 惰性加载+图片缓存 Arc 化
2. **增量优先**: 工具轮消息前缀共享+中间保存增量化(ΣO(R²)→O(R))+OCR/进度变化驱动落盘+会话懒加载(首屏50条游标分页)+delta 流(既有)
3. **投影按需**: 列表 SQL 大列原位裁剪(3 repo 十位点)+ApiConfig id 投影+统计聚合 SQL 下推+manifest 拆分 summary+ndjson
4. **单一权威+失效事件**: api_configs 60s 缓存(3 写点失效)+有效 id 集 30s 缓存+图片预览两级 LRU
5. **合批/节流在源头**: PDF 进度 DB 写节流+finderStore watch 节流+focus 节流+备份 sleep 消除
6. **删除不传递**: 35 孤儿 emit+4 死监听+~5,200 行死代码(含 multimodal 3,764)+400MB 写-only 缓存+幽灵命令收编

其他: llm_request_body 回声默认关/请求体单次序列化/多变体伪 Arc 真共享(6MB→0)/memory 刷新风暴修复+Fact 双索引消除/同步 mtime 哈希备忘/备份 tee-hash+get_backup 直读。


## 1. 审计覆盖

| 批次 | 范围 | 发现 | 文档 |
|------|------|------|------|
| A1 | chat_v2 管线/事件/prompt/persistence | 13 | A1-chatv2-pipeline.md |
| A2 | chat_v2 handlers+tools | 25 | A2-chatv2-handlers-tools.md |
| A3 | vfs repos+database+lance(SQL 层) | 25+3缺索引 | A3-vfs-repos-db.md |
| A4 | vfs handlers+索引+PDF | 25 | A4-vfs-handlers.md |
| A5 | data_governance(备份/同步/迁移) | 25+11 | A5-data-governance.md |
| A6 | llm_manager+llm_usage | 19 | A6-llm-streaming.md |
| A7 | dstu+local_api | 21 | A7-dstu-localapi.md |
| A8 | memory+essay+qbank | 14 | A8-memory-essay-qbank.md |
| A9 | multimodal/ocr/translation/providers | 12 | A9-io-modules.md |
| A10 | 根级服务+cmd/+mcp+cloud | 18 | A10-root-legacy-misc.md |
| A11 | 事件层 225×102 全配对 | 25 | A11-events.md |
| B1 | stores+hooks+contexts | 15 | B1-fe-stores-hooks.md |
| B2 | api+dstu 适配+services | 14 | B2-fe-data-layer.md |
| B3 | features/chat(126K 行) | 22 | B3-fe-chat.md |
| B4 | learning-hub+mindmap | 17 | B4-fe-hub-mindmap.md |
| B5 | notes+todo+pomodoro+pdf+settings | 16 | B5-fe-notes-settings.md |
| B6 | components+杂项 | 6 | B6-fe-shared.md |

**合计 312 条正式发现**。每条带 file:line + 量级假设 + 触发频率 + 修复草图，详见分报告。

## 2. 审计期间确认"已修"清单（防重复施工）

| 项 | 证据 |
|----|------|
| 流式 chunk 合批（4KB/16ms+序号） | A1: 39 emit 点全覆盖无活旁路 |
| essay/qbank/translation O(n²) accumulated emit → delta | A8: commit 8792bbb8, 残留 clone 均为终态兜底 |
| streamingBlockSaver 5s 全量回声 | A1+B3+SESSION-2 亲核: eventBridge.ts:899 注释, 落盘下沉 Rust PeriodicBlockPersister; autoSave 单例成死代码(清理候选) |
| "今日到期" UTC 时区 bug（双侧） | B4: reviewPlanStore.ts:23 本地日期 + spaced_repetition.rs:227 chrono::Local |
| 聊天附件三段往返（主链） | A10: uploadAttachmentByPath 路径直传已落地; learning-hub 拖拽链仍三段(见 T1) |
| aws-sdk-s3 手写 SigV4 / image+hyper 裁剪 | e7dcf6f4 / 50e32d04（昨日之前） |
| 导图拖拽预计算/历史 structuredClone/制卡事件洪水 | ebdd7b93 / ae1e6385 / 更早（无回归, B4 复核） |
| 命令名重复 | A10: 误报——cfg(feature) 互斥桩+再导出, 730 命令查重零重复 |

## 3. 横切主题（按浪费量级排序）

### T1 大字节过 JSON 通道（base64/number[]）—— 最大单项税
六处独立证实：A4 内容命令族(20MB→54MB 拷贝, 前端 6+ 视图主通道)、A7 内容读取预载 base64 后弃用(OCR 命中时 50MB 白读)+多模态题目集 13MB×3 次搬运、A10 导入 20MB PDF→27MB JSON+拖拽链每图 4× 膨胀、B6 DragDropZone number[] 过 JSON(10MB→165MB 瞬时, 50MB 上限最坏 600MB)+notes_save_asset 双键 base64(5MB→13.4MB)、B4 office 三段往返(5MB docx→20MB+)、B3 图片预览串行 N get+无缓存。
**共性**: `pdfstream://` 流式协议(pdf 模块)已被 B5 确认为全项目范本, 聊天上传链 uploadAttachmentByPath 已验证路径直传范式——**正确基础设施已存在, 推广接线即可**。

### T2 全量回声/整包拉取（该增量的不增量）
- `chat_v2_llm_request_body` 全量脱敏请求体每轮回声前端 + rawRequests 无界累积（A6+A11+B3 三角证实, 长会话 MB 级内存增长; 磁盘 logFilePath 已有副本, 回声纯冗余）
- ApiConfig 全量拉取: 每次发送全量 ~25 字段含明文 api_key 只为取 id（B2+B5 双确认 TauriAdapter.ts:3402 原位, 9 处裸调; 前端 5min 缓存已存在但发送路径未复用）
- 会话切换全量加载无分页, 懒加载 callback 从未接线（B3#1, 每次切换 MB 级）
- finderStore 最近视图 dstu_get N+1（B2: learning-hub/stores/finderStore.ts:562/673, 最坏 ~100 invoke）+ watch('*') 任意 DSTU 事件触发 limit:10000 全量 reload（B4: 批量 OCR 期间 ~3 次/秒）
- focus 全刷: ModernSidebar 每次窗口 focus 3 invoke 含 limit=10000 全量已分组会话与 limit=8 重叠（B6#3）
- 列表 SELECT 整列拉大 TEXT: files/translation/exam/essay 携 extracted_text/preview_json/grading_result_json 等, 每列表 1-25MB（A3#2; A7-D1 翻译列表携全文 ~500KB）

### T3 重复读 I/O 与写放大
- 备份+恢复 3-4 倍重复读（~4.6GB+800MB/次, copy 与校验不共享读）+ 每 100 页 sleep 50ms（200MB 库每次备份纯睡眠 25.6 秒——一行修复）（A5）
- OCR 逐页 save 读-改-写整本 ocr_pages_json O(P²)（300 页 ≈90MB 序列化+WAL 放大; **批量接口已存在未接线**）（A3#3）
- 工具轮中间保存: 每轮 2 次全量, 用户消息写 2R+2 次, 块重写 ΣO(R²), 全块 SELECT 2R+1 次（A1#3）
- 导入 checkpoint 循环内全状态重写 O(n²)（A10#3）
- 增量备份整包缓冲+pretty JSON（A5）; list_backups 每次全量解析所有 manifest（10 份×2 万条 ≈60MB JSON/次）（A5/raw）

### T4 缓存负资产/形同虚设
- lance_vector_store.rs:517 启动预热 100K 向量（≈400MB RSS+500MB 磁盘读）进**从未被读取**的 emb_cache, 5 处 new() 各触发一次（A3#1——删除即得）
- get_api_configs 20 个调用点零缓存, 每 LLM 请求（含工具循环每 turn）6-8 次 settings 读+3-5 次全量 JSON 解析（A6#1）
- schema registry 5 个命令每次重新聚合（4×开库+全历史 SELECT 再覆写缓存）（A5-commands）
- 同步全量 sha256: 2GB 资产零变更也要全量哈希, 无 mtime/size 快路径（A5#4, 加字段需旧清单兼容）
- 图片缓存命中每轮全量 clone+重编码, 无上限+blocking client 跑 async 线程（A9#1, 含图对话每轮 ~18MB）

### T5 深拷贝/重复序列化
- 每工具轮全量 chat_history.clone() O(R×H)（A1#1）; multi_variant Arc 包裹后仍 (*arc).clone() 深拷贝, 3 变体带图 ~6MB（A1#2）
- 每请求 3-4 次全请求体序列化/深拷贝（sanitize 深拷贝+to_string 测长+重试 .json()+pretty 审计日志）（A6#2）
- 工具输出每次 clone 1 次+序列化 3 次, data_json/content 双份进 LLM 消息（A2#2）
- upsert_streaming_block 后端 parse→Value→再序列化空转（A2#3 后端半）
- 翻译 repo 无条件 JOIN 全文+节点 metadata 复制（A7-D1）

### T6 base64 直塞 LLM 上下文
attachment/image_generation/retrieval 把 MB 级 base64 塞进 tool_result 直达 LLM 且同时落库+emit 三处放大（A2#1, 最坏单轮数百万 token——**这是钱**）

### T7 死代码/孤儿通道（删除即得）
- multimodal 四文件 3,764 行死代码(57%), 内含最重反模式（A9#5）
- legacy 流后缀孤儿事件族 ~15 emit 点零监听, RAG 引用还带 chunk_text 全文（A11#2; 全项目 ~40+ emit 点+4 死监听可零风险删）
- autoSave.ts streamingBlockSaver 死单例（60s 空转 interval）（B3）
- vfs_multimodal_index image_base64 入参前端零调用（A9）
- 会话懒加载机器死代码（B3#1 的 callback 未接线——或接线或删）
- src/store/ResourceStateManager.ts 零引用死代码（B1; src/store 与 src/stores 非新旧两套, 仅此一个孤儿）
- B1 补录三热点: useChatV2Stats 统计页拉 2×1000 完整会话前端聚合(~0.6MB, 应 SQL GROUP BY 下推); useQuestionBankSession 50/页串行 while 拉全部题目(绕过 store 分页的活跃盲区); researchStore synthesis_updated 每 chunk 全量拼接 O(n²)(5-50MB 级字符复制)——前两项并入 task-038, research O(n²) 提升为 task-038 内优先项

## 4. 最小数据传递总方案（目标架构）

**一句话**: 功能不变的前提下, 让每个字节只在产生与消费之间移动一次、以所需最小形状、按需或增量地移动。

六条原则（按本项目实情排序）:

1. **字节不过 JSON 通道**——一切二进制内容走协议或路径: 推广 `pdfstream://` 模式覆盖内容命令族; 路径直传（uploadAttachmentByPath 范式）覆盖全部上传/拖拽/导入链; 进 LLM 的图片走 blob_hash 引用+Rust 侧注入, 不进 tool_result 正文。
2. **增量优先**——事件已 delta 化的（chat/essay/qbank/translation）为范本; 保存改增量 upsert（工具轮中间保存、OCR 批量接口接线）; 同步用 since_version 游标（已存在）+mtime/size 快路径; 会话加载分页+懒加载接线。
3. **投影按需**——列表 SQL 裁掉大 TEXT 列（摘要列+detail 按需）; ApiConfig 提供 id 投影; manifest 拆 summary+ndjson; 前端只取所需字段。
4. **单一权威+失效事件**——api_configs/schema registry/模型能力以 Rust 为权威, 缓存+变更事件失效, 删前端重复实现路径; 图片缓存 Arc 共享+上限。
5. **合批/节流在源头**——emit 侧合批（已有合批队列, 删旁路）, 进度通道节流（150ms 已是范本）, 高频 UI 序列化只做 dirty 分支（Crepe）。
6. **删除不传递**——死代码/孤儿事件/回声/双键冗余直接删（本报告 T7+T1/T2 零风险项）。

**理论收益上界**（按各分报告量级假设累计）: IPC 大字节传输减少 ~90%（T1+T2）; 备份/恢复 I/O 减半以上（T3-A5）; LLM token 浪费消除数 MB 级/轮（T6）; 启动内存 -400MB+（T4-lance）; 流式场景重复计算 O(n²)→O(n)（T3/T5）。

## 5. 对账裁决（矛盾清理）

| 矛盾 | 裁决 |
|------|------|
| A2"5s 全量回声存活" vs A1/B3"已删" | **已删**（eventBridge.ts:899 注释+存活调用均终态有界, SESSION-2 亲核 toolCall/workspace_events 调用点）; A2 该条仅后端 parse→Value→再序列化空转半段成立 |
| B3"finderStore 零 invoke" vs B2"N+1 存在" | **N+1 存在**——B3 只查了字面 invoke(, 实际走 dstu api 包装层（B2 行号 learning-hub/stores/finderStore.ts:562/673+B4 watch('*') 链证实） |
| B5"Crepe 250ms 未修" vs B6"已核销缓解" | **存在但已防抖**——docChanged→250ms 防抖→getMarkdown 仍全档序列化+全串 ZWS 拷贝, 打字期 ~4 次/秒（B5 行号级结论为准） |
| 开局 grep"essay/qbank O(n²) 未修" | **已修**（8792bbb8, A8 全链核实） |

## 6. 优化任务清单（生成 harness task-024+）

排序原则: 零风险速赢 → 高收益中险 → 大改动。⚠️ 标记涉及他会在飞脏文件的项（见 §7 协调节）。

| 任务 | 内容 | 来源 | 验证 |
|------|------|------|------|
| task-024 [P0] 速赢包 A: 备份 sleep 改 Busy-only + PDF 双名连发改单名 + 双键 base64 冗余键删除(imageUpload.ts:108 notes_save_asset) | A5#1 / A11#3 / B6#2 | cargo check + tsc |
| task-025 [P0] 速赢包 B: 孤儿事件族删除(~40 emit 点+4 死监听, legacy 后缀族含 chunk_text) + autoSave 死单例删除 + vfs_multimodal_index 休眠入参清理 | A11 / B3 / A9 | cargo check + tsc |
| task-026 [P0] 死缓存/死代码: lance emb_cache 预热移除(400MB) + multimodal 四文件 3764 行死代码删除 | A3#1 / A9#5 | cargo check |
| task-027 [P1] llm_request_body 回声治理: 后端改按需(仅调试开启时发/截断)+前端 rawRequests 有界环形 | A6/A11/B3 | cargo check + tsc |
| task-028 [P1] ApiConfig 投影命令(id-only)+发送路径走缓存+明文 key 不过 IPC | B2/B5 | cargo check + tsc |
| task-029 [P1] finderStore 批量化(后端批量 get 命令或先过滤后取)+watch('*')改定向 | B2/B4 | tsc |
| task-030 [P1] 列表 SQL 大列裁剪(files/translation/exam/essay 4 repo) | A3#2/A7-D1 | cargo check |
| task-031 [P1] 内容读取 OCR/extracted 优先免全量 base64 预载 | A7-D2 | cargo check |
| task-032 [P1] 工具轮降拷贝: chat_history 改 Arc 共享前缀+multi_variant 真共享+中间保存改增量 upsert | A1#1/2/3 | cargo check |
| task-033 [P1] api_configs 读缓存(20 调用点)+请求体序列化去重(3-4 次→1) | A6#1/2 | cargo check |
| task-034 [P1] OCR 批量接口接线+PDF 进度通道节流单事件化 | A3#3/A4#3 | cargo check |
| task-035 [P2] base64 内容命令族迁 pdfstream://(A4 六视图主通道) | A4#1 | cargo check + tsc |
| task-036 [P2] 备份/恢复 tee-hash 共享读+manifest 拆分+list_backups 按 ID 直读 | A5 | cargo check |
| task-037 [P2] 同步 mtime/size 快路径(旧清单兼容)+图片缓存 Arc 化+上限 | A5#4/A9#1 | cargo check |
| task-038 [P1] 前端热点包: research O(n²) 拼接增量+统计聚合 SQL 下推+题集串行分页并行化+Crepe dirty 分支+会话分页懒加载接线+图片预览缓存+focus 全刷去重 | B5/B3/B1 | tsc |
| task-039 [P2] memory 写路径: smart-write 批量化+双重索引守卫+刷新条件修复 | A8 | cargo check |
| task-040 [P2] ⚠️ 拖拽链路径直传(resourceDropImport+UnifiedDragDropZone+TextbookContentView office 链)——等他会脏文件合流后做 | A10/B6/B4 | tsc |
| task-041 [P0] 终验: 全量 cargo check+tsc+对照 B1 补录 | 全部 | 双绿 |

## 7. 协调与风险

- **他会在飞脏文件**（四方会话共享树）: resourceDropImport.ts / UnifiedDragDropZone.tsx / LearningHubSidebar.tsx / TextbookTextareaContentView.tsx / dstu/api.ts / AttachmentUploader.tsx / useTauriDragAndDrop.ts 等——task-040 等合流; 其余任务避开或最小交集, 提交只 add 自己改的文件。
- **契约冻结**: local_api v1.1 全部 11 端点（A7 已核, 内部 4 处优化形状不变）; LLM 工具 JSON 参数协议; 事件名已 delta 化的四条流。
- **数据兼容**: 3 个缺索引走 Refinery 迁移（blobs.ref_count 部分索引/mindmap_versions 复合/translations.created_at）; 同步清单加字段需旧清单兼容。
- **B1 已补录**（重试成功, 05b7810d）: 15 条发现已并入 §1/§7; top 三项入 task-038（升级 P1）, ResourceStateManager 死代码入 task-025 清单。
