# B5 notes+todo+pomodoro+pdf+settings 数据传递审计

> 撰写: 2026-09-11 08:18 CST | 基线: main@ae1e6385 | 审计人: task-021 (B5)
> 方法: 全量串行阅读(无子代理), 代码只读

## 范围与方法

目录清单 (行数为 wc -l 实测, 五目录合计 49,952):

| 目录 | 行数 | 直接 invoke 点 | listen 点 | 说明 |
|------|------|----------------|-----------|------|
| src/features/notes | ~12,056 | 6 | 2 | 数据调用主要经 `src/dstu/api.ts` 适配层, idx-invokes 仅统计直接 invoke(), 低估 notes 真实 IPC 密度 |
| src/features/todo | 1,996 | 20 (api.ts) | 0 | 全部经 todo/api.ts 薄封装 |
| src/features/pomodoro | 1,076 | 5 (api.ts) | 0 | zustand persist, 前端自治 |
| src/features/pdf | 3,253 | 1 | 0 | 渲染走 pdfstream:// 自定义协议 |
| src/features/settings | 31,571 | 37 | 2 | OcrEngineCard 9 / ChatSessionArchiveTab 4 / McpToolsSection 4 / 其余分散 |

invoke 密度 top (五目录内): todo/api.ts 20, OcrEngineCard.tsx 9, pomodoro/api.ts 5, ChatSessionArchiveTab/McpToolsSection 各 4, OcrSettings/EngineSettings 各 3, **NotesContext.tsx 3**(注: notes 主数据流走 dstu.*, 记在 B2 索引)。

审计延伸到的共享文件 (发现归属 B5 但修复涉及 B2/B6 范围, 已在表中注明):
- `src/components/crepe/CrepeEditor.tsx` (2,859 行, notes 编辑器内核)
- `src/components/crepe/features/imageUpload.ts` (429 行)
- `src/features/chat/adapters/TauriAdapter.ts` (ApiConfig 消费侧, 原 B3/B2 热点由本任务按指令核实)

采样策略: notes/settings 全量精读数据层与编辑器链路; todo/pomodoro/pdf 快速过 (轮询/全量拉取/listen 清理三项检查)。

## 数据流摘要

**notes**: NotesProvider 挂载 → `dstu.list('/', {limit:10000})` 全量元数据 → 打开笔记时 `dstu.getContent`+`dstu.get` 双 IPC 装正文 → Crepe 编辑器 (src/components/crepe) 打字 → updateState hook 检测 doc 变化 → 250ms 防抖 `getMarkdown()` 全文序列化 → onChange → ① 1.5s 防抖入保存队列 ② 500ms 防抖 window 事件 `notes:content-changed`(携带全文) → NotesContextPanel 再 300ms 防抖全文逐行正则抽大纲。保存路径: saveNoteContent → `notes_list_assets` IPC → 每资产一次全文 RegExp 替换归一化 → `dstu.update` 全文 IPC。图片: 拖入/选入 → FileReader→base64 → `notes_save_asset`(base64 过 IPC) → markdown 存相对路径 → 渲染时 proxyDomURL → `get_image_as_base64` 每张一次 IPC(读盘+编码, 无缓存)。

**settings**: Settings 挂载 → loadConfig = 1× `get_api_configurations`(全量含 key) + 1× `get_model_assignments` + **37× 单键 `settingsApi.get`** 并行; 同时 useVendorModels.loadAll 3× (vendors/profiles/assignments)。每次切 tab 无条件 handleSave = **~40× 单键写**。任何保存动作派发 `api_configurations_changed` → 7 个已挂载组件各自行全量重拉。聊天发送路径上 TauriAdapter.getValidChatModelIdSet 每次发送/重试/变体重放都全量拉 ApiConfig 数组只为取 id 集合。

**todo/pomodoro/pdf**: todo 每次增删改后 reloadCurrentView 全量重拉当前视图; pomodoro 1s tick 仅运行态, persist 已 partialize+节流; pdf 阅读走 pdfstream:// 流式协议 + blob URL 缓存, 扫描件检测仅采样 3 页。

## 发现清单 (影响降序)

| # | 位置 file:line | 模式 | 复杂度/量级估计 (假设) | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|------------------------|----------|----------|----------|
| 1 | src/components/crepe/CrepeEditor.tsx:1135-1159 | 全文序列化: 每次文档变化 250ms 防抖后 `getMarkdown()` 整档 ProseMirror→markdown 序列化 + `.split(ZWS).join('')` 二次全串拷贝 | O(doc) 每次发射; 假设 100KB 笔记序列化 5-20ms, 200KB+ 可达数十 ms; 连续打字时约 4 次/秒 | 持续输入期间每 250ms | 大文档自适应防抖 (len>100K→800ms, >200K→1200ms, 同 NotesContextPanel 阈值); 消费端只需大纲/字数时改走 ProseMirror doc 遍历免序列化 | 低; 仅拉长发射间隔, 自动保存链已有 1.5s 防抖兜底 |
| 2 | src/features/chat/adapters/TauriAdapter.ts:3400-3412 (调用点 2439/3112/3312/3462); 后端 src-tauri/src/commands.rs:1572-1583 | IPC 边界: 只需 id 集合却全量拉 `Vec<ApiConfig>` (~25 字段含 api_key 明文) | 假设 20 个配置 ×~600B ≈ 12KB JSON/次; 每条消息发送 1-3 次 (send/retry/variant 各自调用) | 每次聊天发送/重试/变体重放 | 复用 useAvailableModels 已有 5min 模块缓存 (src/features/chat/hooks/useAvailableModels.ts:80-84 已存在!) 或后端新增 `get_api_config_ids` 投影命令; 缓存由 `api_configurations_changed` 失效 | 低; 发送路径纯只读校验, 缓存失效事件已存在 |
| 3 | src/features/notes/NotesContext.tsx:981-997 | 保存时图片链接归一化: 每次保存先 `notes_list_assets` IPC, 再对每资产构造 RegExp 跑一次全文替换 | O(资产数 × 全文长度)/保存; 假设 50 张图 × 200KB = 1000 万字符扫描/次 | 每次自动保存 (打字停 1.5s) + 切换笔记冲刷草稿 | ① 前缀 guard: 正文不含 `convertFileSrc('')` 前缀(如 `http://asset.localhost/`)时整段跳过 (indexOf 一次); ② 合并为单趟 alternation 正则 | 低; 归一化语义不变, 仅减少扫描趟数 |
| 4 | src/components/crepe/features/imageUpload.ts:46-61, 93-116, 304-371 | 大 payload 三段往返: pickImageWithTauriDialog 已持原始路径却 path→asset://→fetch→Blob→File→FileReader base64→JSON IPC→Rust 解码→写盘 | 10MB 图 → ~13.3MB base64 字符串过 JSON IPC, 内存峰值 ~3-4×; 假设单张截图 1-3MB | 每张拖入/粘贴/选入图片 | 新增后端命令 `notes_save_asset_from_path`(收源路径, 后端直接 copy+重命名), 前端仅传路径; 走 dialog 原始 `selected` | 中; 需新命令+前端双轨兼容 (粘贴场景仍需 base64 路径) |
| 5 | src/components/crepe/features/imageUpload.ts:209-287 (proxyDomURL); src-tauri/src/file_manager.rs:386 | 渲染期零缓存: 每张 notes_assets/ 图片渲染都 `get_image_as_base64` → 后端读盘+base64 编码+整串过 IPC→data URL | 假设 20 张图×500KB: 每次编辑器重建(切笔记/重挂载) 20 次 IPC 共 ~13MB base64; 同图多实例重复拉 | 每次图片节点创建/编辑器重挂载 | 前端按路径的 blob URL LRU 缓存 (Map<path, blobURL>, 容量 50, revoke on evict); Windows 可直接 convertFileSrc(参考 ExamPreview.tsx:154 模式), macOS WebView 限制保留后端通道但加缓存 | 低; 缓存层透明, 失效仅随笔记资产删除 |
| 6 | src/features/pdf/components/PdfReader.tsx:178 + src/utils/chatApi.ts:39-48 | Vec\<u8\>→JSON 数字数组: `invoke<number[]>('read_file_bytes')` 整本 PDF 过 JSON | 假设 50MB PDF → ~150-250MB JSON 字符串 + JS number 数组 400MB, 再拷贝进 Uint8Array | 手动"选择文件"打开 PDF (事件路径已走 pdfstream:// 无此问题) | Tauri 2 raw request/ArrayBuffer 通道, 或改走 `convertFileSrc(path,'pdfstream')` 与事件路径统一 (路径已在手) | 中; chatApi.ts 为共享工具 (B2/学习封面/会话生命周期也用), 需单独适配各自消费端 |
| 7 | src/features/settings/components/useSettingsConfig.ts:39-109 | 打开链 N 次单键 get: Promise.all 37 个 `settingsApi.get` 各一次 IPC round trip | 37× IPC 固定开销 (每次 ~0.1-1ms 序列化+调度), 合计可感延迟; 数据量本身极小 | 每次打开设置页 | 后端新增 `get_settings_bulk(keys: Vec<&str>) -> HashMap` 单命令; 前端一次调用 | 低; 纯增量命令, 旧调用可保留 |
| 8 | src/features/settings/components/useSettingsConfig.ts:341-383, 445-454 | 切 tab 无条件全量写: handleTabChange 每次先 handleSave(true) = ~40 个单键 `settingsApi.save` | 40× IPC/次切换; 假设用户浏览 8 个 tab = 320 次写, 绝大多数值未变 | 每次设置 tab 切换 | ① 脏检查: loadConfig 时存快照, 仅保存实际变化键; ② 同 #7 批量写命令 | 低; 行为不变(保存语义保留), 仅省未变键 |
| 9 | 'api_configurations_changed' 派发 2 处 (useSettingsConfig.ts:392, SiliconFlowSection.tsx:237) × 监听 7 处 (Settings.tsx:379, useVendorModels.ts:160, ModelPicker.tsx:206, ModelPanel.tsx:145, voice-input/hooks.ts:363, EssayGradingWorkbench.tsx:197, ExamSheetUploader.tsx:192) | 事件扇出全量重拉: 一次保存触发所有已挂载监听者各自 fetch (Settings 1×全量 ApiConfig + loadAll 3× + ModelPanel 1× + voice 1× + ...) | 假设 4 个监听者挂载: 一次保存 ≈ 6-7 次 IPC, 其中 ≥3 次是含 key 全量数组 | 每次非静默保存/SiliconFlow key 保存 | 统一配置 store (单一 fetch + 事件仅通知失效); 与 #2 共用缓存层 | 中; 涉及 7 个组件改造, 建议分步: 先建 store, 组件逐个迁移 |
| 10 | src/features/notes/NotesContextPanel.tsx:90-129 (parseHeadings) | 全文逐行正则: split('\n') + 每行 regex + normalizeHeadingText 3 次正则/标题行 | O(行数)/次; 假设 5000 行 ≈ 2-5ms 主线程; 已有 300ms 防抖(>200KB→1200ms)+requestIdleCallback 缓解 | 打字期间每 ~300ms (经 250ms 序列化+500ms 事件+300ms 自身防抖级联) | 大纲直接消费 ProseMirror heading 节点 (编辑器 emit 增量 heading 列表), 文本路径保留给 DSTU 模式 | 低; 现有缓解已可接受, 此项为根治项非紧急 |
| 11 | src/features/notes/NotesContext.tsx:1723-1796 | context value 未 memo: 每次 Provider 渲染生成新对象字面量, ~6 个 useNotes/useNotesOptional 消费组件全量重渲染 | 假设每次保存(setNotes map 10k 项 + setActive)触发一轮; 消费者含编辑器头部/工具栏/侧栏面板/树容器 | 每次保存/tab 切换/搜索态变化 (~1.5s 一次打字期间) | value 用 useMemo 聚合; 高频字段 (loadedContentIds Set 每次新建) 拆分或改 zustand selector (notesTreeStore 已是好范本) | 中; 需逐一核对消费端等值语义 (引用变化被依赖的地方) |
| 12 | src/features/todo/stores/useTodoStore.ts:185-218, 330-355 | 变更后全量重拉: create/update/toggle/delete 完成后一律 reloadCurrentView() 重取整个视图列表 | 假设当前视图 300 项 ×~300B ≈ 90KB/次点击 | 每次勾选/编辑/删除 | 乐观更新: update/create 命令已返回更新对象, 直接 patch store; 删除同理 | 低; 失败回滚路径需补 (catch 时 reload 兜底) |
| 13 | src/features/notes/NotesContext.tsx:486-526 (ensureNoteContent) | 双 IPC 取一物: `dstu.getContent` + `dstu.get` 两轮; 节点元数据在 refreshNotes 列表里已有 | 2× IPC + 2× 反序列化/次首开; 假设列表已有元数据则 get 纯冗余 | 每个笔记首次打开 | 元数据从 notes 数组取 (dstuNodeToNoteItem 已在内存), 仅 getContent 拉正文; saveNoteContent:1004 的 update 返回值已含节点可复用同思路 | 低; 注意列表项可能过期 (冲突场景已有 ensureNoteContent 兜底) |
| 14 | src/features/notes/NotesContext.tsx:1274-1312, 1088-1125 (renameItem/updateNoteTags) | setMetadata + dstu.get 双往返: 写后整读重建本地对象 | 2× IPC/次重命名或标签保存; get 返回值仅用于重建已知字段 | 每次重命名/改标签 | setMetadata 成功后本地构造 updated (字段全已知), 省 get; 或后端 setMetadata 返回节点 | 低 |
| 15 | src/features/notes/NotesContext.tsx:1134-1183 (renameTagAcrossNotes) | 分页重列表 + 每笔记 2 IPC: 200/页循环重拉全部元数据 (refreshNotes 用 limit:10000 单次, 此处不一致), 然后逐笔记 updateNoteTags (=setMetadata+get) | 假设 500 笔记: 3 页列表 + 500×2 IPC = ~1003 次调用; 用户显式动作, 量可感 | 手动"跨笔记重命名标签" | ① 复用内存 notes (已有 fallback 逻辑, 反转优先级); ② 后端批量 set_metadata_many; ③ 与 refreshNotes 统一 limit | 低; 批量命令需后端配合, 前端侧 ① 零风险 |
| 16 | src/features/notes/NotesContext.tsx:884-891 | 孤儿事件: `canvas:get-state` 监听无任何派发者 (全仓 grep 零命中), handleGetState 内还做全文 parseStructure+generateSummary | 零运行成本 (永不触发); 死代码 + 白板功能移除残留 | 无 | 删除监听与 getCanvasModeState 中未被事件使用的分支 (getCanvasNoteMetadata 另有真实调用方需保留) | 极低; 纯死代码清理, 需确认无动态字符串派发 |

补充 (未计入上限, 备忘): Settings.tsx:813-827 reloadAssignments 与 useSettingsConfig 重复一份 12 字段映射 (维护性); NotesCrepeEditor.tsx:383-386 事件携带全文字符串为引用传递无拷贝, 本身零成本。

## 已知热点重定位结论

1. **Crepe 每 250ms 全文序列化 (原 CrepeEditor.tsx:1128-1160)**: **未修复, 未漂移** — 现位于 `src/components/crepe/CrepeEditor.tsx:1128-1160`, 行号都未变 (原报告即此文件, notes 侧包装层是 NotesCrepeEditor.tsx)。精确语义: 非固定轮询, 而是 updateState hook 检测 docChanged → 250ms 防抖 → `getMarkdown()` 全文序列化 (+ZWS 全串替换拷贝), 带 IME 合成跳过与 lastMarkdown 去重。打字期间实际 ~4 次/秒整档序列化, 是 #1 发现。
2. **笔记标题全文正则扫描 (原 NotesContextPanel.tsx:90-154)**: **仍在原位, 已有缓解** — `NotesContextPanel.tsx:90-129` parseHeadings 逐行正则不变, 但上游有 500ms 事件防抖 (NotesCrepeEditor.tsx:381) + 自身 300ms 防抖 + >200KB 时 1200ms 防抖与 requestIdleCallback (NotesContextPanel.tsx:83-84, 141, 170-180)。当前状态: 可接受的 O(n)/300ms, 根治需改走 ProseMirror 节点 (#10)。
3. **ApiConfig 数组含 key 全量拉取无视前端缓存 (原 TauriAdapter.ts:3402)**: **未修复, 路径未漂移** — `src/features/chat/adapters/TauriAdapter.ts:3402` 原行号命中, `getValidChatModelIdSet` 每次发送/重试/变体重放全量拉取仅取 id。前端**已有** 5min 模块级缓存 (useAvailableModels.ts:80-84) 但 TauriAdapter 未复用; 全仓另有 10 个独立调用点、7 个变更监听重拉 (#2/#9)。属 B2/B3 修复面, 本审计提供结论与证据。

## 最小数据传递方案

理想形态 (功能不变前提):

- **配置 (settings/chat 共享)**: 单一 ApiConfig store — 一次全量拉取 + `api_configurations_changed` 失效; 发送路径只消费 id/enabled 投影 (后端 `get_api_config_ids` 或前端缓存的派生 Set)。键值设置走 `get_settings_bulk`/`save_settings_bulk` 批量命令, 37 读 40 写 → 各 1 次。
- **编辑器链**: markdown 全文只在"保存边界"序列化一次 (1.5s 防抖已有); 大纲/字数等 UI 派生量由编辑器从 ProseMirror doc 增量维护并 emit 结构化小 payload, 文本全文不再流经 window 事件。
- **图片**: 上传 = 路径直达后端 (一次 IPC 零 base64); 渲染 = 每资产至多一次 base64→blob 转换并缓存, 同图终身一次直至淘汰。
- **notes 数据**: 列表一次 (元数据投影, 已是); 正文每笔记一次 (loadedContentIds 已是); 元数据不再二次 get; 保存归一化仅在检测到 preview URL 前缀时执行。
- **todo**: 每次变更移动一个对象 (命令返回值), 不重拉视图。
- **pdf**: 维持 pdfstream:// 按需分页流式 (已是), 手选文件路径并入同一协议。

## 不动清单

- **GlobalPomodoroWidget 1s tick** (GlobalPomodoroWidget.tsx:25): 仅运行态计时, persist 已 partialize 排除 timeLeft + 节流存储 (usePomodoroStore.ts:211-218) — 已是良好实现。
- **SyncIndicator 30s 轮询** (data-governance/SyncIndicator.tsx:42): 仅同步 tab 挂载期间存在, 单计数查询, 注释已论证保守间隔。
- **pdf pdfstream:// 协议 + blob URL 缓存 + 3 页采样检测** (PdfReader.tsx:173-254, EnhancedPdfViewer.tsx:394-406, TextbookPdfViewer.tsx:107): 本模块已是流式范本。
- **ExamPreview convertFileSrc 渲染** (ExamPreview.tsx:154, 209): 零拷贝正确模式, 是 #5 修复的参照。
- **useAvailableModels 5min 模块缓存** (useAvailableModels.ts:80-84): 保留并作为统一配置缓存层的种子。
- **notesTreeStore** (zustand + subscribeWithSelector + immer): 订阅粒度良好。
- **proxyDomURL 的 macOS WebView asset:// 限制注释** (imageUpload.ts:208-231): 平台约束真实存在, #5 修复必须按平台分派, 不能一刀切改 convertFileSrc。
- 全局项: local_api v1.1 stable 端点契约、前端可观察行为、既有 SQLite 数据 (PLAN §8)。
- **imageTrackInterval 500ms** (CrepeEditor.tsx:2691-2693): 仅 `imageDebugEnabled` 时创建, 生产不运行 — 不动。
