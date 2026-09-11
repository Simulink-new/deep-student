# B4 learning-hub + mindmap 数据传递审计

> 撰写: 2026-09-11 | 基线 commit: ae1e6385 | 审计人: B4 (task-020)
> 范围: `src/features/learning-hub/` (~30.7K 行) + `src/features/mindmap/` (~17.0K 行) + 关联全局 store (`src/stores/reviewPlanStore.ts`, `src/stores/questionBankStore.ts`) 与 Rust 侧 `review_plan` / `spaced_repetition`
> 方法: 全量目录清单 + invoke/listen 密度排序 → 热点文件全文通读 (mindmapStore/MindMapCanvas/OutlineView/TextbookContentView/finderStore/Sidebar 关键段) + 跨层链路核实 (前端 date → Rust local_today; read_file_bytes 消费链)。未派生子代理, 全程串行。代码只读。

## 范围与方法

### invoke/listen 密度表 (来源: idx-invokes.txt / idx-listens.txt, 按文件聚合)

| 文件 | invoke 数 | 文件 | listen 数 |
|------|----------|------|----------|
| learning-hub/apps/views/TextbookContentView.tsx | 15 | LearningHubSidebar.tsx:761 (textbook-import-progress) | 1 |
| mindmap/api/mindmapApi.ts | 7 | resourceDropImport.ts:307 (textbook-import-progress) | 1 |
| learning-hub/apps/views/ExamContentView.tsx | 5 | views/IndexStatusView.tsx:329 (vfs-index-progress) | 1 |
| mindmap/components/mindmap/MindMapEmbed.tsx | 4 | views/IndexStatusView.tsx:432 (mm_index_progress) | 1 |
| learning-hub/apps/views/FileContentView.tsx | 4 | 隐性事件订阅 (非 listen API): dstu.watch('*') ×2 (LearningHubSidebar.tsx:382, LearningHubPage.tsx:215 → Tauri 事件 `dstu:change`); window CustomEvent: pdf-page-refs ×2, ocrPageSync, dstu:refresh, learningHubOpenNote | — |
| learning-hub/apps/views/ImageContentView.tsx | 2 | | |
| 其余 (resourceDropImport/NoteContentView/LearningHubPage) | 各 1 | | |

mindmap 的 invoke 集中在 `mindmapApi.ts` (7 个), 组件层经 store 间接调用, 故 invoke 密度低但实际数据量集中在 get/update content 两个全量 JSON 端点。

### 通读文件清单
mindmapStore.ts (1540), MindMapCanvas.tsx (650), OutlineView.tsx (1290), BranchNode.tsx/NodeContent.tsx (编辑提交路径), MindMapEmbed.tsx (加载段), MindMapContentView.tsx (选择器段); TextbookContentView.tsx (1518), LearningHubSidebar.tsx (订阅/listen/watch/refresh 段), LearningHubPage.tsx (watch 段), finderStore.ts (893), IndexStatusView.tsx (listen 段), MemoryView.tsx (加载段), ImageContentView.tsx, TabPanelContainer.tsx, usePdfLoader.ts (缓存段), reviewPlanStore.ts (日期/加载段), questionBankStore.ts (分页段); Rust: review_plan_service.rs (get_due), review_plan_repo.rs (list_due), spaced_repetition.rs (local_today)。

## 数据流摘要

**mindmap 编辑链**: 组件失焦提交 (NodeContent.tsx:244 onBlur / OutlineView.tsx:228 commitText, 均本地 state 缓冲, 无每键 store 写) → `applyMutation` (mindmapStore.ts:433-461: pushHistory 全文档 structuredClone + immer 变异) → 三路持久化: ① 240ms 防抖草稿 (全文档 clone + JSON.stringify → localStorage, M-069 崩溃安全); ② 1.5s 防抖保存 (全文档 JSON.stringify → `vfs_update_mindmap` IPC → 后端全量写库 + 乐观锁 expectedUpdatedAt); ③ unmount/beforeunload 同步 flush。渲染侧: document 变更 → MindMapCanvas layout 引擎全树 O(N) 重算 + nodes map O(N); OutlineView flattenTree O(N) 重建 + 全行渲染。

**learning-hub 列表链**: finderStore.loadItems (dstu.list, limit:10000 全量) → 前端 sortItems O(n log n) → Sidebar (无 selector 全 store 订阅) → FinderFileList (已虚拟化 + 行 memo)。任何 DSTU 变更 (含 OCR 流水线每页建笔记) → `dstu.watch('*')` Tauri 事件 → 300ms 防抖 → 全量 reload。内容视图按 tab `display:none` 保活 (TabPanelContainer.tsx:62), 切 tab 不重拉; PDF/office 内容以 base64 全量过 IPC, PDF 有模块级 LRU (usePdfLoader)。

**复习计划链**: reviewPlanStore 分页加载 (get_due 服务层 limit 100 / repo 默认 50 / loadAllPlans 1000), SQL COUNT+LIMIT 双查询; "今天"取本地日期, 前后端契约一致。

## 发现清单 (影响降序)

| # | 位置 (file:line) | 模式 | 复杂度/量级估计 (假设) | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|----------------------|---------|---------|---------|
| 1 | learning-hub/apps/views/TextbookContentView.tsx:371-374 | IPC 边界: office 文件三段往返 (read_file_bytes Vec\<u8\>→JSON 数字数组→前端 Uint8Array→base64→RichDocumentPreview 再解码) | 字节以 JSON number 序列化, 每字节 ~3-6 字符。假设 5MB docx: ≥20MB JSON 字符串过 IPC + 2 次全量格式转换 + 前端再解码。为 PLAN.md §5 "附件三段往返" 的活跃消费点 (commands.rs:2221 read_file_bytes 仍在) | 每次打开带 filePath 的 docx/xlsx/pptx/text | 后端新增 `read_file_base64`(或复用 vfs_get_attachment_content 的 base64 形状), 前端删 uint8ArrayToBase64 中转; 或走自定义协议 URL | 低: 前端调用侧同步适配即可, 行为不变 |
| 2 | learning-hub/LearningHubSidebar.tsx:121-148 | 前端订阅粒度: `useFinderStore()` 无 selector 整库解构, 2633 行组件树随任意 store 变化重渲染 | 每次点击选择(select 每次 new Set)/每次搜索键入(searchQuery)/每次 items 刷新 → sidebar 全树 reconcile。假设 500 项目录: 列表行有 memo 缓解, 但 frame/工具栏/树/QuickAccess 全量重跑 | 每次选择/每键(经 300ms 防抖后每词)/每次刷新 | 拆为逐字段 selector (参照 LearningHubPage.tsx:305-310 已有写法) 或 useShallow; 纯机械改动 | 中: 漏拉字段会引入 undefined, 需逐项核对 + tsc |
| 3 | learning-hub/stores/finderStore.ts:804-834 (limit:10000) + LearningHubSidebar.tsx:382-409 (watch('*')→300ms 防抖全量 refresh) | 事件流+SQL: 任何 DSTU 变更 → 全量 reload (万条上限列表 + O(n log n) 前端排序 + 全 sidebar 重渲染) | 假设批量 OCR 300 页文档 → 每页 1 个 created 事件 → 持续触发防抖 → ~3.3 次/秒 × 每次全量 IPC + 排序。假设库 2k 项: 每次刷新 2k 节点序列化过边界 | 任何 DSTU 写 (OCR 每页/批量制卡/笔记保存) 期间持续 | watch 事件已带 node 对象 → 本地对 items 做 upsert/remove 增量 patch, 仅在失效时全量; 或后端聚合成 "batch-end" 信号再刷新 | 中: 增量 patch 需覆盖 moved/restored/purged 全事件语义, 漏 case 会显示脏数据 |
| 4 | mindmap/store/mindmapStore.ts:422-431 (debounceSave 1500ms) + 1001-1129 (save) | 序列化/IPC: 停笔 1.5s 后全文档 JSON.stringify + 全量 IPC + 后端全量写 (含 1042 行 stringify) | 假设 5k 节点 × 平均 120B/节点 ≈ 600KB 文档: 每次保存 600KB×2 (stringify+IPC) + 后端 parse+写盘。连续编辑按停笔节流 (打字中不触发, 且编辑失焦才 commit) | 每次编辑停顿 1.5s / 每次 undo/redo/view 切换 | 增量保存: 文档分块哈希或 op-log patch (仅传变更子树); 后端合并。需保留 expectedUpdatedAt 语义 → 可改为 per-patch 版本向量 | 高: 乐观锁/冲突自动恢复/草稿恢复链路均假设全量语义, 需专项设计 |
| 5 | mindmap/views/OutlineView.tsx:990 (flattenTree) + 1230-1252 (全行 map) + 126-137 (SortableOutlineNode 无 memo, 每行 ~20 个订阅 141-160) | 渲染层: 大纲视图无虚拟化, flattenTree 每次产全新 FlatNode 对象 → 全部行重渲染 | 假设 1k 节点文档: 任一文档变更 (一次失焦提交/一次折叠) → O(N) flatten + O(N) 行 reconcile + 20N 选择器求值。DOM 常驻 1k 行 | 每次文档变异 | ① 行组件 React.memo + flatNode 字段级浅比较 (node 引用 immer 下未变路径稳定); ② 引入虚拟化 (行高不定, 用 estimedSize); ③ 订阅收敛 (searchResults→Set 传入) | 中: memo 比较函数写错会显示旧文本; 虚拟化影响 scrollIntoView 焦点滚动逻辑 (194-205) |
| 6 | mindmap/components/mindmap/MindMapCanvas.tsx:191-221 (nodes useMemo) + 443-454 (onNodeDrag flushSync setDragPositionOverride) | 渲染层: 拖拽每帧 dragPositionOverride 变更 → nodes map 重建全部 N 个节点对象 (新 data 引用 + 新 onOpenMenu 闭包 212-213) → ReactFlow 节点 memo 失效 | 假设 500 节点 60fps 拖拽: 3 万对象/秒分配 + 可见节点全量 re-render。虚拟化 (onlyRenderVisibleElements) 只救渲染不救分配 | 拖拽期间每 mousemove | 拖拽中绕过 store/useState: 直接持有 position override 的 ref + 仅对受影响节点 setNodes (ReactFlow applyNodeChanges); onOpenMenu 提升为稳定引用 (nodeId 参数已自带) | 中: 拖拽视觉链路 (子树跟随/置灰/放置高亮) 需回归测试 |
| 7 | learning-hub/stores/finderStore.ts:662-697 (loadItems recent 分支) + 552-580 (executeSearch recent 分支) | N+1: recent 视图对每个最近项单独 `dstu.get` (无缓存, dstu/api.ts:261 每次裸 invoke) | recentStore maxItems=50 (recentStore.ts:77) → 每次进入 recent 视图 ≤50 次 IPC + 50 次后端单行查询; 且随 #3 的全量刷新链重复发生 | 每次进入 recent 视图/每次全局刷新 | 后端 `dstu_get_batch(paths[])` 一次往返; 或 recentStore 持久化时内嵌 node 快照 (显示用字段), 失效才回源 | 低: 批量端点新增, 前端两处调用点替换 |
| 8 | mindmap/store/mindmapStore.ts:406-420 (buildDraftPayload deepClone) + 297-313 (writeDraft JSON.stringify) | 序列化冗余: 草稿路径全树 clone 后紧接同步 stringify — 写路径无并发窗口, clone 纯冗余 (structuredClone+stringify 双遍历) | 与 #4 同量级: 每次草稿持久化 2×全树遍历, 240ms 防抖后触发 | 每次编辑停顿 240ms | buildDraftPayload 直接引用 s.document (stringify 同步快照语义等价), 删除 deepClone; saveDraftSync 卸载路径同 | 低: 一行删除, 语义等价 (JSON.stringify 同步遍历不受后续变异影响) |
| 9 | learning-hub/apps/views/TextbookContentView.tsx:279-291 + usePdfLoader.ts | IPC 边界: 教材 PDF 恒走 DB 全量 base64 (PDF-403 修复刻意不走 pdfstream://, 见 270-272 注释) | 假设 50MB PDF → ~67MB base64 字符串过 JSON IPC + 前端 base64ToFile 解码。LRU 缓存 (usePdfLoader.ts:19-47, 容量+字节上限) 缓解重复打开 | 每个新 PDF/缓存淘汰后首次打开 | 后端为 VFS blob 内容注册协议 handler (类 pdfstream 但按 attachmentId 鉴权, 不受目录白名单限制), 前端传 URL 免 base64 | 中: 动摇 PDF-403 修复决策, 需确认新 handler 的鉴权边界 |
| 10 | learning-hub/views/MemoryView.tsx:188-199 (listMemory(undefined,10000)) + 315/396 (每次记忆操作后全量重载) | 全量加载+重复拉取: 记忆管理视图每次单条增删改 → 全量重拉 | 假设 1k 条记忆: 每次操作 1k 行 IPC + 重渲染; 连续批量整理时 n 次操作 = n×全量 | 每次记忆 CRUD | 操作返回值直接 upsert/remove 本地列表 (API 已返回实体), 失败才回退全量 | 低: 单视图内部状态, 不涉它方 |
| 11 | learning-hub/LearningHubSidebar.tsx:783-790 | 事件泄漏: 导入进行中组件卸载 → isMountedRef 提前 return, unlisten (786) 不再执行 → textbook-import-progress 监听器存活至会话结束 | 1 个泄漏监听器/次中断导入; 后续同事件到来时对已卸载组件 setState (React 18 无警告但白耗) | 导入中关闭 sidebar/视图 (低频) | unlisten 移入 finally 或卸载 effect 兜底 unlisten | 低 |
| 12 | learning-hub/views/IndexStatusView.tsx:395-397 (单资源 completed → loadData 全量) + 328-409 (async listen 竞态) | 事件流: 批量索引期间每资源完成触发 200 条全量 reload; 且 setupListener 为 async, 卸载早于 listen resolve 时 unlisten 为 null → 监听器泄漏 | 假设批量索引 100 资源: 100 次 getAllIndexStatus(200)+listDimensions; 竞态窗口毫秒级但存在 | 批量索引期间每资源 / 视图快速开关 | completed 也走防抖 (复用 batch 通道); listen 泄漏用 isCleanedUp 模式 (参照 dstu/api.ts:636-645 已有写法) | 低 |
| 13 | mindmap/store/mindmapStore.ts:395-403 (pushHistory 每变异 1 次全文档 clone) + 243 (MAX_HISTORY=50) | 内存/序列化: 历史快照 50×全文档常驻 + 每变异 1 次 structuredClone | 有界非无界 (答任务问: 快照数组不会无界增长)。假设 600KB 文档: 历史上限 ~30MB 常驻 + 每次编辑 1 次 O(N) clone (structuredClone 后成本已减半, ae1e6385) | 每次文档变异 | 命令模式历史 (存 op + 逆 op), 或 immer patch (produceWithPatches) — 属 applyMutation 不可变化重写, 风险高 | 高: 留待专项, 见最小方案 |
| 14 | mindmap/components/mindmap/MindMapEmbed.tsx:369-407 | 重复拉取: embed 无内容缓存, 同一导图被多个 embed 引用时各自全量拉 content JSON | 假设聊天里 3 个引用同一 600KB 导图: 3×(metadata+content) IPC + 3×JSON.parse | 每个 embed 挂载 | 模块级 content LRU (key: targetId, TTL 短); 版本引用 (mv_) 可不缓存 | 低: embed 只读展示, 短 TTL 不致陈旧 |
| 15 | learning-hub/LearningHubSidebar.tsx:382 + learning-hub/LearningHubPage.tsx:215 | 重复监听: 同一 `dstu:change` 全局事件双订阅 (职责不同: 列表刷新 vs 删除关 tab), 每事件 payload (含全 node 对象) 双份处理; 后端 dstu_watch('*') 注册 ×2 | 每事件 2 次反序列化 + 2 次回调; 小 payload, 量级可忽略但架构噪音 | 每 DSTU 变更 | 单一 hub 订阅后按 type 分发; 或 Page 侧复用 watch 事件的 deleted 分支由 Sidebar 刷新顺带处理 | 低 |
| 16 | learning-hub/apps/views/TextbookContentView.tsx:502-517 | 双写冗余: 书签同时写 vfsFileApi.updateBookmarks + dstu.setMetadata (全 metadata merge 携带 bookmarks 全量) | 每次书签变更 2 次 IPC, metadata 全量重写 (含 readingProgress 等无关字段)。防抖 1s | 每次书签操作 (防抖后) | 明确单一真源 (VFS 侧), DSTU metadata 仅存指针或去掉冗余通道 | 中: 两处消费方 (sidebar/恢复链) 都读 metadata, 需一并核对 |
| 17 | learning-hub/ResourceGridView.tsx:368 (displayNodes.map 全量渲染, 无虚拟化) | 死代码候选: 仅被 index.ts:33 桶导出, 未发现实际消费方 | 若被启用且列表大则全量 DOM; 现状零成本 | — | 确认无消费后删除 (归 B6 清理批次) | 低 |

## 已知热点重定位结论

**① "今日到期" UTC 时区 bug — 双侧已修, 无回归**
- 前端: `src/stores/reviewPlanStore.ts:23-26` `localTodayISODate()` 用本地 `getFullYear/getMonth/getDate` (文件头注释明确记录原 toISOString 缺陷), 消费点 733/738 (`getOverdueCount`/`getTodayDueCount`, 即原 724-732 漂移后的位置)。
- Rust: `src-tauri/src/spaced_repetition.rs:227-234` `local_today()`/`local_today_string()` 用 `chrono::Local::now().date_naive()`, 文档注释明言"勿再直接取 Utc 日期"。消费点: `review_plan_service.rs:203`, `vfs/repos/review_plan_repo.rs:242/352/599/819`。
- learning-hub 内残留 toISOString 仅 5 处, 均非日期比较语义: ExamContentView.tsx:245/286 (考试 ended_at 事件时间戳), MemoryView.tsx:545 + MemoryFolderBanner.tsx:143 (导出文件名 `slice(0,10)`, 东八区早 8 点前文件名差一天, 无害), desktopStore.ts:336 (createdAt)。

**② 每 1.5s 全量 JSON 自动保存 — 仍在, 但语义澄清为"停笔防抖"**
- 位置: mindmapStore.ts:422-431 `debounceSave` (setTimeout 1500ms, 每次新编辑重置) → `save()` 1001-1129, 其中 1042 行 `JSON.stringify(docWithViewState)` 全文档 → `api.updateMindMap` 全量 IPC。
- 澄清: 非定时器轮询——连续输入不触发 (且文本编辑失焦才 commit, NodeContent.tsx:244 / OutlineView.tsx:228), 实际频率 = 编辑停顿次数。但每次触发仍是全文档序列化+全量传输+后端全量写, 大文档下为 mindmap 剩余最大单点开销 (发现 #4)。
- 配套: 240ms 草稿防抖 (365-373) 存在双遍历冗余 (发现 #8); 冲突自动重载/重试链路完好 (1085-1127)。

**③ 已修三处现状 — 均在位, 无回归**
- 拖拽子树预计算 (ebdd7b93): MindMapCanvas.tsx:101 `dragSubtreeIdsRef` + 414-435 DragStart 一次 DFS 构建 Set + 468 `subtreeIds.has(n.id)` O(1) 查询, DragStop 505 重置。✅
- 历史深拷贝 structuredClone (ae1e6385): mindmapStore.ts:253-257 `deepClone` 助手, pushHistory/undo/redo/buildDraftPayload/剪贴板全部走它。✅
- 制卡事件合并 (2b2d96ef/5df87c3f): `ankiCardFlushQueue` 100ms 合并缓冲位于 `src/features/chat/adapters/TauriAdapter.ts` — 属 features/chat (B3 范围), 非 B4 目录, 现状核实移交 B3。
- 快照数组无界增长: **否** — history.past/future 均有 MAX_HISTORY=50 上界 (mindmapStore.ts:243, 398-399 shift 淘汰)。
- 渲染层全树重渲染: **部分存在** — Canvas 侧任一文档变更触发全树 layout O(N) 重算 + nodes map O(N) (虚拟化仅限渲染); Outline 侧无虚拟化全行重渲染 (发现 #5); 拖拽帧全节点对象重建 (发现 #6)。但"每键全树"不成立——编辑均为失焦提交。

## 最小数据传递方案

理想形态 (功能不变前提):
1. **mindmap 保存链**: 变更只移动一次——失焦 commit 产生 op (节点级 patch), 草稿只存未保存 op 列表 (而非全文档), 1.5s 保存只传 op + 基线版本; 后端合并递增。历史改 produceWithPatches 命令模式后, 快照内存从 50×全文档降为 50×op。过渡期零风险项: 删 buildDraftPayload 冗余 clone (#8)。
2. **learning-hub 列表**: DSTU watch 事件已携带 node → 列表增量 upsert, 全量 reload 仅作为兜底 (节流 30s); recent 视图快照内嵌; office/PDF 字节流走协议 URL, base64 仅保留 DB 迁移期回退。
3. **渲染订阅**: Sidebar 逐字段 selector; OutlineView 行 memo + 虚拟化; Canvas 拖拽期间冻结 layout 重算 (位置 override 不入依赖)。
4. **元数据瘦身**: dstu.list 若后端可投影 (排除大 metadata 字段), 列表 payload 可再降一档 (待 A7/A4 核实列表端点形状后联动)。

## 不动清单

- `TabPanelContainer.tsx:62` display:none 保活策略——切 tab 不重拉是刻意行为 (前端可观察行为不变)。
- finderStore `limit:10000` 的全选/批量计数语义——可优化传输方式但不可改回分页截断 (注释明言后端默认 50 会截断批量操作)。
- M-069 localStorage 同步草稿崩溃安全机制——可优化实现 (#8), 不可移除。
- TextbookContentView.tsx:270-272 PDF-403 回避 pdfstream 的决策——#9 方案需先证明新 handler 鉴权等价。
- `local_today_string()`/`localTodayISODate()` 双侧本地日期契约——已修勿动。
- mindmap 乐观锁 expectedUpdatedAt + 冲突自动恢复流程——#4 增量保存设计的硬约束。
- 题库分页 (questionBankStore 792-829)、复习计划分页 (服务层 limit 100/repo 50/loadAllPlans 1000)、usePdfLoader LRU、FinderFileList 虚拟化+行 memo、ImageContentView 大文件守卫——现状良好, 无需改动。
