# B3 features/chat 数据传递审计

> 撰写: 2026-09-11 | 审计员: B3 (task-019) | 基线: main @ ae1e6385 工作树
> 范围: src/features/chat/ (448 文件, 126,451 行 — 全项目最大前端区)

## 范围与方法

**索引导航** (来自 task-005 的 idx-*.txt):

- invoke 密度 top (features/chat 共 169 处):

| 文件 | invoke 数 | 角色 |
|---|---|---|
| adapters/TauriAdapter.ts | 22 | 主 IPC 适配器 (4174 行) |
| workspace/api.ts | 19 | 工作区命令封装 |
| pages/useSessionLifecycle.ts | 8 | 会话列表/生命周期 |
| context/vfsRefApi.ts | 8 | 附件/上下文 VFS 引用 |
| debug/attachmentPipelineTestPlugin.ts | 7 | 调试插件(跳过) |
| resources/api.ts 等 | 6+ | 资源库封装 |
| skills/api.ts, ModelPanel, ModelPicker 等 | ≤5 | 低频 UI |

- listen 密度: TauriAdapter.ts 8 处 (384-393 setup + 509-519 retrySetup——非重复订阅, 有 generation 守卫), workspace/events.ts 11 处, 其余为调试插件/块组件。

**采样策略** (126K 行不可全读, 数据路径优先):
- 全读: core/middleware/{chunkBuffer,autoSave,eventBridge}.ts, core/store/{blockActions,immerHelpers,restoreActions}.ts, hooks/{useChatStore,SessionManager,useSessionTags,useImagePreviewsFromRefs}.ts, context/imagePayload.ts, resources/api.ts, pages/useSessionLifecycle.ts
- 定位读: TauriAdapter.ts 5 个区段 (订阅 370-560 / 事件入口 1313-1490 / loadSession 2799-2920 / saveSession 2894-2980 / send 1942-2330), MessageItem.tsx 2 区段, ankiCardsBlock.tsx 2 区段, MessageList.tsx 全读, AttachmentUploader.tsx 上传区段, InputBarV2.tsx 订阅区段
- grep 级横切: JSON roundtrip (0 处, 干净), `content +=` 累积, 裸 console 计数, setInterval 轮询, memo/useShallow 模式
- 跳过: debug/ (10.8K 行测试插件), styles, UI 样式组件

## 数据流摘要

**流式渲染链 (核心路径, 整体健康)**: Rust 管线 emit `chat_v2_event_{sessionId}` → TauriAdapter.handleBlockEvent (去重/乱序缓冲) → eventBridge.processEventInternal → chunkBuffer (按会话/块缓冲, **4ms 窗口 / 4KB 阈值**合批, core/constants.ts:15/18) → store.batchUpdateBlockContent (immer produce, 结构共享, 仅改写块换新引用) → useBlocksByIds ref 缓存 selector → BlockRendererWithStore 每块独立订阅 → MarkdownRenderer (React.memo + useMemo)。内容落盘已下沉 Rust 侧 PeriodicBlockPersister (5s 时间闸), 前端 5s 全量回声已删 (见热点重定位)。**结论: 该链无 O(n²) 全列表重渲染、无 store 级累积拼接热点**; 每 flush 一次 `block.content += delta` 字符串新分配是固有不可变成本, 4KB 阈值已摊薄。

**会话加载链**: 切换会话 → chat_v2_load_session **全量**拉取 (消息+块+状态) → restoreFromBackend 一次性建 Map/set → 虚拟化渲染 (>80 条启用 @tanstack/virtual)。懒加载机器 (loadMoreMessages/appendOlderMessages/顶部哨兵) 存在但 **callback 从未接线, hasMoreMessages 恒 false** — 死代码。

**发送链**: store.sendMessage → TauriAdapter.executeSendMessage → buildSendContextRefs (pMap 并行+重试+5s 超时, 每引用 resourceStoreApi.get + vfs 引用解析 + formatToBlocks) → truncateContextByTokens → invoke chat_v2_send_message (options + userContextRefs 含解析后内容)。附件上传本地路径走 uploadAttachmentByPath 直传 (前端不过字节), 剪贴板走 base64Content。

**订阅粒度**: 全区无裸全 store 订阅; MessageItem/ActivityTimeline/InputBarV2 均为 selector+ref 缓存/useShallow 模式; finderStore (chat 侧) 仅窄 selector 消费。

## 发现清单 (影响降序)

| # | 位置 (file:line) | 模式 | 复杂度/量级估计 (假设) | 触发频率 | 修复草图 | 破坏风险 |
|---|---|---|---|---|---|---|
| 1 | adapters/TauriAdapter.ts:2812 + core/store/restoreActions.ts:720 | 会话加载全量无分页; 懒加载机器死代码 (setLoadMoreMessagesCallback 全库 0 调用者) | O(M+B) IPC payload; 代码自估 500B/消息 + 1KB/块 — 500 消息×4 块 ≈ 2MB+ JSON 每次切换 (假设: 重度会话) | 每次切换会话 | 后端 chat_v2_load_session 加 limit/before 游标 + 接线 loadMore 回调 (哨兵/UI 已就绪); 或首屏仅拉最近 N 条 | 中: 分页正确性/滚动锚点 (锚点代码 MessageList.tsx:445-456 已存在) |
| 2 | adapters/TauriAdapter.ts:1426-1475 (chat_v2_llm_request_body 事件) | 后端完整 LLM 请求体 (含全上下文) 整包 emit 回前端, 存入 _meta.rawRequests[] 逐轮累积, 会话生命周期常驻内存 | O(prompt) IPC + 内存驻留; 假设 200KB/轮 × 5 工具轮 = 1MB 常驻/消息; 全会话累加 | 每 LLM 轮 (工具密集=高频) | payload 已带 logFilePath (磁盘已有全量) — 前端只存 path+摘要 (model/url/字节数), 调试面板按需从文件加载; 或后端 emit 截断体 | 低-中: 调试面板"查看原始请求"需改懒加载 |
| 3 | core/middleware/eventBridge.ts:890,893,901,1086 + adapters/TauriAdapter.ts:1947 等 (全区裸 console 共 200+ 处, vite.config.ts 无 drop-console) | 生产构建裸 console.log; 每 chunk 3-4 条字符串格式化; sendMessage:1947 dump 完整 attachments (含 previewUrl 多 MB base64) | 每 chunk ~3 次 log 调用; 每次发送可能 MB 级对象序列化 (DevTools 开时); 假设 30 chunk/s 流式 | 每 chunk / 每次发送 / 每 IPC | 统一走 debugLog (debugMasterSwitch 默认关, blockActions/autoSave 已是范例); vite prod 加 esbuild `drop:['console']` 兜底; 日志中剥离 previewUrl | 低: 仅日志 |
| 4 | hooks/useImagePreviewsFromRefs.ts:131-166,169-205 + components/MessageItem.tsx:912 + context/imagePayload.ts:33-36 + resources/api.ts:183-199 (无缓存) | 图片预览链: 每引用串行 vfs_get_resource → 批量 vfs_resolve_resource_refs 返回全量 base64 过 IPC → normalizeBase64 正则 replace (全量拷贝) → data URL 拼接 (再全量拷贝); 虚拟列表滚回即全部重做, 直渲模式全消息同时挂载 | 假设 4MB 图: 5.3MB base64 过 IPC + ~11MB 瞬时拷贝/次; N 图 × M 重挂载 | 每次含图消息挂载/滚回 | 模块级 hash→blobURL 缓存 (LRU+revoke); step1 Promise.all 并行; external 存储走 Tauri asset protocol 免 base64 | 低: 显示不变 |
| 5 | core/store/restoreActions.ts:860-872 (串行 exists N+1), 776-805 (分组 pinned 资源串行 createOrReuse) | 恢复期每引用一次串行 IPC await | 假设 10 引用 × 5-20ms/往返 = 50-200ms 串行阻塞恢复尾链 | 每次会话切换 | 后端加 resources_exists_many 批量; 前端 Promise.all 并行 | 低 |
| 6 | plugins/blocks/ankiCardsBlock.tsx:855-895 (cards 变化全量指纹重建+CustomEvent), 909-948 (全卡模板兼容双重过滤+CustomEvent) | 调试副作用生产无条件执行; 流式制卡 cards 数组渐增 → 累计 O(N²) 指纹串构建 | 假设 200 卡 × 渐进 20 次更新 = 全量扫描 20 次 ≈ 4000 卡次扫描 + 20 CustomEvent | 每次制卡流式更新 | gate 到调试开关 (debugMasterSwitch.isEnabled() 或采样); 指纹计算增量化 | 低 |
| 7 | pages/useSessionLifecycle.ts:172-211 (createAnalysisSession) | 图片 bytes→`binary +=` 32KB 分块拼接 (O(n²/32KB) 拷贝 + UTF-16 双字节)→btoa→data URL→**同时塞 metadata.initConfig.images 与 initConfig.images 两份**过 IPC | 假设 5MB 图: ~800MB 瞬时字节拷贝 + 2×6.7MB base64 IPC | 每次创建分析会话 (每图) | dialog 返回本地路径 → 复用 uploadAttachmentByPath 直传, init 传 sourceId/路径; 或 chunked btoa 直转 | 中: analysis 初始化管线需后端配套接受路径 |
| 8 | pages/useSessionLifecycle.ts:235-254 | 会话列表双通道 (grouped '*' + ungrouped '') 各 limit=10000 全量 + 2 次 count + 全量标签 batch | 元数据行小但无界; 假设 500 会话 ≈ 数百 KB × 2 通道 | 启动一次 | 单查询 (groupId null 语义) 或后端聚合视图; 保留 PAGE_SIZE 真分页 | 低 |
| 9 | adapters/TauriAdapter.ts:1316-1384 (handleBlockEvent) | chatanki/anki_cards 事件每条派发 ≤2 个 window CustomEvent (第二个 1351-1365 无采样); template-designer 每事件一次 emit; detail 携带 payload/result 引用 | 假设制卡流 30 事件/s × 2 dispatch + 对象保留 | 每 chatanki/template 事件 | 与 #6 同 gate; 合并两个重复 if 块 | 低 |
| 10 | components/MessageList.tsx:220-232 (effectiveEstimatedItemSize) | messageOrder 每变化全量扫 M 消息 × B 块判 anki | O(M×B)/消息新增; 假设 300 消息×4 块 = 1200 查询/次 | 每条新消息 | 由块类型集合派生 (store 维护 typeIndex) 或首算后缓存标志 | 低 |
| 11 | core/middleware/autoSave.ts:303-505 + adapters/TauriAdapter.ts:621 | streamingBlockSaver 死单例: 模块加载即构造并启动 60s 清理 interval 永久空转; callback 已接线但零喂入者 | 1 interval/60s 空扫描 (微); ~200 行死代码 | 常驻 | 删除 StreamingBlockSaverImpl + 接线 (eventBridge:1204/1236 注释确认休眠) | 无: 无喂入者 |
| 12 | debug/chatV2Logger.ts:121-125,193-226 | 日志系统 storageEnabled 恒真 + 每 log 一次 window.dispatchEvent, 生产也跑; logAttachment 每引用每发送多条 | 假设每发送 5 引用 × 6 条 = 30 entry+dispatch | 每 log 调用 | storage/dispatch gate 到调试面板开启或 DEV | 低: 调试面板改为开启时才采集 |
| 13 | plugins/events/toolCall.ts:430,485,641 + workspace/events.ts:504 | workspace_status 块每工具完成 2 次全量 toolOutputJson upsert (即时 + 500ms 后快照) | O(snapshot)×2/工具调用; 假设 10 agent 快照 ≈ 数 KB-数十 KB | 每 workspace 工具完成 | 合并为 end 事件单次持久化; 快照更新走增量 diff | 低 |
| 14 | components/input-bar/InputBarV2.tsx:245-276 | useShallow 订阅含 inputValue → 每击键重渲染 InputBarUI (3177 行树); 选择器内联 lastAssistantUsage 反向扫 messageOrder (流式期间每 flush 执行, 通常末位早退) | 每击键一次 3K 行组件树 render; O(M) 最坏每 flush | 每击键 / 每 store 变化 | inputValue 下沉叶组件 (本地 state + debounce 同步 store); lastAssistantUsage 抽成 store 派生字段 (流结束更新) | 中: 焦点/IME 行为需回归 |
| 15 | hooks/SessionManager.ts:192-206,275-302,53-59 | useAllSessionIds/useSessionStats/useIsSessionStreaming/useSessionStore 死轮询/全订阅 hooks — 零组件消费 | 0 运行时成本 (未挂载); 死代码误导 | — | 删除或改事件驱动 (sessionManager 已有 subscribe) | 无 |
| 16 | adapters/contextHelper.ts:235-242 等 | 每引用每发送一条裸 console.log('[InjectContent]' 含 200 字符预览) + logAttachment 详单 | O(引用数) 字符串构建/发送 | 每次发送 × 每引用 | 并入 #3 的 debugLog 统一 | 低 |
| 17 | hooks/useImagePreviewsFromRefs.ts:117-123,228-240 | 每次图片加载 2 个 debug CustomEvent (含 previewUrl 前 50 字符/长度) | 2 dispatch+对象/加载 | 每图片加载 | 并入 #6 gate | 低 |
| 18 | adapters/TauriAdapter.ts:2904-2957 (saveSession) | 500ms 尾随 debounce 的会话元数据保存: 每次全量 chatParams+features+modeState+skillStateJson+pendingContextRefsJson | 假设典型 2-10KB/次; skillStateJson 大时可观; 连续流式期间被推后 (机制健康) | 流式停顿 ≥500ms / 流结束 / 击键停顿 | skillStateJson 超阈值时改增量版本号或分片; 其余不动 | 低 |
| 19 | components/AttachmentUploader.tsx:203-218 | 所有附件 (含 by-path 直传场景) FileReader.readAsDataURL 全量 base64 仅作 previewUrl, 常驻 store.attachments 至会话销毁 | 假设 10MB PDF = 13MB base64 字符串驻留/附件 | 每次添加附件 | 有 sourcePath 时仅存路径+缩略图; previewUrl 用 blob URL (revoke on remove) | 低-中: 预览渲染源切换 |
| 20 | core/store/restoreActions.ts:156-966 | 恢复路径 3 级降级 JSON 解析 (标准/逐元素/字符扫描 5s 超时兜底) — 健壮但正常路径仅走第一级; 每级 console 详单 | 正常 O(n); 异常时最坏 5s 阻塞微任务 | 每次恢复 (仅异常触发重路径) | 不动 (数据安全设计); 日志并入 #3 | — |
| 21 | adapters/TauriAdapter.ts (chatAnkiChunkLogCounter) | chunk 采样计数 Map 按 blockId 无界增长 (会话生命周期) | 假设百级 entry, 微 | 每 chatanki chunk | 随会话 cleanup 清理 | 低 |
| 22 | components/MessageList.tsx:259-287 | 流式期间 rAF 循环定期 virtualizer.measure() (80ms/闪卡 200ms) — 有界且 intentional, 解决动态高度 | 12.5 次/s measure | 流式期间 | 不动 (已按闪卡场景调优) | — |

## 已知热点重定位结论

**streamingBlockSaver 5s 全量回声 (PLAN §5 第 3 行)**:
- **已删除**: eventBridge.ts:897-899 与 1081-1083 留有注释 "删除此处每 5s 全量内容回传 chat_v2_upsert_streaming_block 的 IPC 回声", 防闪退落盘下沉到 Rust 侧 PeriodicBlockPersister (ChatV2LLMAdapter/VariantLLMAdapter, 5s 时间闸)。A1/A6 可核实 Rust 侧覆盖面。
- **前端残留**: autoSave.ts:502 单例仍被构造 (60s 清理 interval 永久空转), TauriAdapter.ts:621 仍接线 saveCallback→executeUpsertStreamingBlock (:3255), 但 `scheduleBlockSave` **全库 0 调用者** — 纯死代码, O(n²) 回声确认消失 (发现 #11)。
- **仍存活的 chat_v2_upsert_streaming_block 调用** (均为有界终态持久化, 非每 chunk): plugins/events/toolCall.ts:430/485/641 (workspace_status 块, 每工具完成 ≤2 次, 发现 #13) + workspace/events.ts:504 (同型)。

**finderStore dstu_get N+1 ~100 次 (PLAN §5 第 6 行)**:
- **已迁移且 N+1 消失**: 现位于 `src/features/learning-hub/stores/finderStore.ts` (892 行), 文件内 **零 invoke/dstu_get** — 取数走 `@/dstu/api` 层 (folderApi 批量列表 + fetchBreadcrumbs), 原 finderStore.ts:562/673 的逐节点 dstu_get 循环不复存在。
- **chat 侧消费健康**: ChatV2Page.tsx:180-181 仅窄 selector 订阅 (currentPath) + 动作引用 (jumpToBreadcrumb), 无粒度问题。残余审计归 B4 (learning-hub)。

## 最小数据传递方案

该模块理想形态 (前端可观察行为不变, 内部数据流可改):

1. **会话内容按需移动**: 切换会话只拉最近 N 条 (一屏 + 缓冲), 哨兵触达才拉更早窗口 — 全套 UI 机器已就绪, 缺的只是后端游标 + 回调接线 (#1)。会话元数据单通道单查询 (#8)。
2. **LLM 请求体不回前端**: 前端只持有 logFilePath + 摘要, 调试面板按需读文件 (#2)。请求体本质产生于后端 (上下文组装在后端完成), 回传是纯冗余移动。
3. **图片字节单向过边界一次**: external 存储的 blob 走 asset protocol URL (浏览器直读, 不过 IPC/不做 base64); inline 小图按 hash 缓存 blob URL, 滚动重挂载零重取 (#4)。上传侧已达成 (by-path 直传)。
4. **调试观测零常驻成本**: 所有 console/CustomEvent/日志存储统一 gate 到调试开关 (#3/#6/#9/#12/#17), 生产路径只剩功能数据。
5. **恢复验证批量化**: exists_many + 并行 pinned 注入 (#5), 恢复尾链从 O(N) 串行往返降为 1-2 次往返。
6. **chunk 链保持现状**: 4ms/4KB 合批 + immer 结构共享 + 块级订阅已是该层最优形态, 不需再动; 字符串 `+=` 固有成本仅在 >1MB 单消息场景才值得换 offset 追加结构 (低优先)。

## 不动清单

- **chunkBuffer 4ms/4KB 参数与 flush-on-end 语义** (eventBridge:922/1112): 防"end 后 chunk 丢弃"竞态, 改动风险大于收益。
- **eventBridge 序列号去重/乱序缓冲/gap 超时** (100/200/3000ms 常量): 数据完整性机制, 非性能热点。
- **restoreFromBackend 三级降级 JSON 解析** (#20): 数据安全设计, 正常路径零成本。
- **MessageList rAF measure 循环** (#22): 已按闪卡场景调参, 虚拟化正确性依赖。
- **immerHelpers 的"始终返回新 Set 实例"约定**: Zustand 引用比较正确性依赖 (CRITICAL-001 注释), 不可"优化"为原引用返回。
- **监听器 generation 守卫与 setup/retry 双注册**: 防泄漏正确性设计, 非重复订阅。
- **streamingBlockSaver 的 Rust 侧继任者 (PeriodicBlockPersister)**: 属 A1/A6 范围, B3 不越界。
- **local_api v1.1 stable 端点、UI 行为、既有 SQLite 数据**: 全局硬约束。
- **发送链 vfs 内容解析 (buildSendContextRefs)**: prompt 构建必需数据, 只可做后端侧组装的跨边界优化 (归 A2 议题), 前端单侧无法消除。
