# B2 前端数据访问层审计

> 撰写: 2026-09-11 08:18 CST | 基线 commit: ae1e6385 | 审计人: B2 (task-018)
> 范围: src/api (13 文件 6,088 行实测) + src/dstu (33 文件 10,370 行实测) + src/services (6 文件) + src/lib (4 文件 288 行) + src/types (过) + src/utils 数据路径 7 文件 3,355 行

## 范围与方法

按 PLAN §3 七项 rubric 逐项过。先用 `docs/PERF-AUDIT/idx-invokes.txt`(904 处)按文件聚合得 invoke 密度 top10，逐一审读；再对范围内全部文件做模式扫描(`Promise.all`/`.map(async`/`for…await`/`JSON.parse(JSON.stringify`/base64)；两个已知热点沿命令名(`get_api_configurations`/`dstu_get`)全量重定位并读上下文确认量级；跨 IPC 的大 payload 追到 Rust 命令体与 SQL SELECT 确认投影形状。代码只读，未做任何修改。

**invoke 密度 top10**(idx-invokes.txt 聚合, 括号内为 B2 处置):

| # | 文件 | invoke 数 | 结论 |
|---|------|----------|------|
| 1 | src/api/dataGovernance.ts | 52 | 1:1 薄封装、各自独立管理命令、无循环——**密度≠开销**, 非热点 |
| 2 | src/utils/settingsApi.ts | 44 | 薄封装; research_* 双键传参(见 #14) |
| 3 | src/utils/graphApi.ts | 37 | **发现 #4**(复习聊天全量历史重发) |
| 4 | src/stores/questionBankStore.ts | 32 | B1 范围, 跳过 |
| 5 | src/api/memoryApi.ts | 28 | 1:1 薄封装, 干净 |
| 6 | src/utils/notesApi.ts | 25 | 1:1 薄封装, 干净 |
| 7 | src/dstu/api.ts | 25 | **发现 #2/#6/#7/#8** |
| 8 | src/features/chat/adapters/TauriAdapter.ts | 22 | **发现 #3**(ApiConfig 热点), 其余走 chat_v2 服务端会话(好模式) |
| 9 | src/api/vfsUnifiedIndexApi.ts | 22 | 1:1 薄封装, 干净 |
| 10 | src/features/todo/api.ts | 20 | B5 范围, 跳过 |

采样策略: top10 全读 + 全范围模式扫描; src/types 只确认类型形状(DstuNode 无 content 字段——元数据节点设计正确)。

## 数据流摘要

前端数据访问层是**三层平行结构**叠在同一个 VFS 后端上: (a) `src/api/vfs*`+`src/utils/*Api`(薄 1:1 invoke 封装, 多为管理/设置面板服务); (b) `src/dstu/api.ts`(Result 风格 facade → dstu_* 命令, 返回纯元数据 DstuNode, 内容走独立 getContent); (c) `src/features/chat/context/vfsRefApi`(聊天附件引用模式, `src/dstu/api/vfsRefApi.ts` 仅是其兼容 re-export shim)。三层之间**未发现同一数据被两层重复转换**(问题 6 结论): dstu 适配器只做投影映射(dstuNodeToAttachment 等), src/dstu 目录 **0 处** JSON.parse(JSON.stringify), 全范围仅 4 处单例(utils/ankiTemplateAttachment、graphApi:176、shared、templateDowngrader)——PLAN §2 的 570 处 JSON roundtrip 大头在 stores/features, 属 B1/B3。

大 payload 路径有两条: 文件创建/上传走 base64-in-JSON(dstu_create、vfs_upload_attachment 剪贴板分支), 拖拽/文件选择链已改 `uploadAttachmentByPath` 路径直传(不传字节, 见发现 #13); 内容读取走 `dstu_get_content` 全量单字符串(无分页命令注册)。

## 发现清单

| # | 位置(file:line) | 模式 | 复杂度/量级估计(假设) | 触发频率 | 修复草图 | 破坏风险 |
|---|---------------|------|----------------------|---------|---------|---------|
| 1 | src/features/learning-hub/stores/finderStore.ts:562-570, 673-683 | N+1: recent 视图逐条 dstu_get | O(N) invokes, N≤50(recentStore.ts:77 上限), path 失败再 fallback `/${id}` 重试 → 最坏 ~100 次/次加载; 300ms 防抖搜索每批击键重发全量(假设输入 6-10 字符 → 每词 6-10 轮×50 invoke) | 每次切到"最近"视图 + recent 视图内每次防抖搜索 | 先用 recentStore 自带 name/type 元数据过滤(现已存 name/type/accessedAt), 只对幸存项 get; 根治=后端 `dstu_list_recent` 批量命令(recentStore 注释已预告该 API) | 低: 前端筛选语义不变; 批量命令是新端点 |
| 2 | src/dstu/api.ts:320-321 | 大 payload: create 双键同值 base64 | fileBase64+fileData 同一字符串传两份 → payload ×2; 假设 10MB 文件 ≈13.3MB base64 → ~26.6MB JSON 单次 invoke | 每次文件/教材/图片创建 | 只传一个键(后端 types.rs:403-407 两字段"同义", 均 Option) | 近零: 后端两键任一可选 |
| 3 | src/features/chat/adapters/TauriAdapter.ts:3402 | 全量拉取无视缓存: get_api_configurations 返回完整 Vec\<ApiConfig\>(含明文用户 api_key, 仅 builtin 打码 commands.rs:1577-1581), 前端只取 id 建 Set | 每配置 ~1-3KB × 配置数; 假设 10 配置 ≈10-30KB JSON × 每条消息发送 | 每条消息 send/replay/variant(TauriAdapter 2439/3112/3442 三处调用)+ ModelPicker/TranslationPopover/ModelPanel/Settings 等 9 处裸调, 全无缓存(唯 useAvailableModels.ts:79-81 有 5 分钟缓存) | 后端加 `get_api_config_ids` 投影命令(只返 id+enabled); 或前端共享缓存层(save_api_configurations 成功时失效) | 低: 新命令/纯前端缓存, 调用点渐进迁移 |
| 4 | src/utils/graphApi.ts:168-220 (continueReviewChatStream) | 大 payload: 每次追问全量历史重发(含 image_base64:187、doc base64_content:193) | O(历史×附件字节)/每消息; 假设带 2 张 2MB 图的 10 轮复习会话 → 每次追问 ~5.3MB+ 重复过 IPC | 复习聊天每次追问(用户触发) | 附件首次上传得 sourceId 后改传引用(对齐 chat_v2 引用模式), 后端按 id 取内容; 短期先在前端剥离历史中已上传附件的 base64 | 中: 需后端配合会话侧历史或 ref 解析; 假设历史项确带 base64(多模态复习场景) |
| 5 | src/dstu/api.ts:728, 757, 787 (deleteMany/restoreMany/moveMany) | N+1: 批量操作后 collectNodeIdsForInvalidation 对每条 path 再发 dstu_get | O(N) invokes/批量; moveMany/deleteMany 的 get **必然失败**(path 已失效)→ 全部走本地 extractSourceIdFromPath fallback → N 次纯浪费; 假设批量选 100 项 = 100 次无效往返 | 每次批量删除/恢复/移动 | 直接用 extractSourceIdFromPath(paths)(api.ts:144 已有, fallback 结果与 get 成功时等价——id 即 path 末段 sourceId) | 低: 缓存失效 key 语义不变 |
| 6 | src/dstu/api.ts:400, 577 (deleteResource/setMetadata) | 重复 invoke: 操作前先 get 取 nodeId 供缓存失效 | 每次 delete/setMetadata 多 1 次往返(2×化) | 每次单资源删除/元数据更新 | 同 #5, 从 path 提取 | 低 |
| 7 | src/dstu/api.ts:560-569 → dstu_get_content | 大 payload: 全文单字符串无分页 | 假设 300 页 PDF 抽取文本 1-5MB/次; 后端另有 get_content_by_type_paged(content_helpers.rs:456)但**未注册命令**; 且 file 分支先整块加载 base64 blob(content_helpers.rs:189-191)即使 OCR/extracted_text 已存在 | 每次打开 note/textbook/file 内容(useDstuResource.ts:99 唯一消费) | 注册 dstu_get_content_paged 命令+前端 useDstuResource 分页适配; blob 改为抽取策略 miss 时才加载 | 中: UI 需分页改造; blob 懒加载属 A7 侧 |
| 8 | src/api/vfsFileApi.ts:99-109 → file_repo.rs:601-615 | 全列投影: vfs_list_files SELECT 含 extracted_text/preview_json/ocr_pages_json | limit=100 × 每行最大数百 KB OCR 文本 → 理论多 MB/次; **但前端零消费 vfsFileApi.list**(仅 updateBookmarks 被 TextbookContentView 用) → 当前实际频率≈0, 潜伏地雷 | 潜伏(无调用者) | list 改摘要投影(VfsFileSummary 不含三文本列), get(fileId) 保留全量 | 无: 无消费者可破坏 |
| 9 | src/services/ankiConnectClient.ts:67-80, 111 | 扇出: 12 并发 get_setting + ≤12 并发 save_setting | 12+12 次微往返/次设置打开+保存; payload 极小 | Anki 设置面板打开/保存(低频) | get_settings_batch/save_settings_batch 键值对命令 | 低: 新命令 |
| 10 | src/App.tsx:632, 670, 715 (+全项目 17 处 get_setting) | 启动散点: 逐 key 单发 get_setting | ~17 次微往返/启动, 各自独立 effect 并行; 单次 payload 极小 | 启动一次/设置变更事件 | 同 #9 批量命令; 收益边际 | 低; 优先级低 |
| 11 | src-tauri/src/dstu/handlers/common.rs:296-313 (交叉引用 A7) | 后端 N+1: dstu_list 文件夹模式逐项 fetch_resource_as_dstu_node | O(items) 次 DB 查询/每次列表 IPC | 每次文件夹列表(前端 1 次 invoke 触发) | 后端侧按类型批量查询后 join | B2 不可修, 转 A7 |
| 12 | src/api/chatV2Api.ts:17 (upsertStreamingBlock) | 死代码: 全项目零调用者(即 PLAN §5 "streamingBlockSaver 疑已消失"的残留壳) | 0 开销 | 无 | 删除导出项 | 无 |
| 13 | src/features/chat/hooks/useAttachmentContextRef.ts:187-205 + vfsRefApi.ts:695-708 | (正面确认) 拖拽/文件选择链已走 uploadAttachmentByPath 路径直传, base64 仅剩剪贴板分支 | 原"三段往返"在主链已修; 剩余 base64 路径仅 paste 场景(无本地路径, 属必要) | 每次粘贴附件 | 维持现状; PLAN §5 该项可标已修(主链) | — |
| 14 | src/utils/settingsApi.ts:94-205 (research_*) | 双键传参: snake_case+camelCase 同值双发 | 参数 JSON ×2(均为小字符串/ID); 后端本就是未实现桩 | research 面板操作(桩, 必失败) | 引擎落地时收敛单键; 现状零收益不改 | 不动(注释已声明为过渡策略) |

注: #1 的 path→id fallback(`dstu.get(recent.path)` 失败再 `dstu.get('/'+recent.id)`)在资源正常时只发 1 次, 最坏 2 次/项——量级估计已计入。

## 已知热点重定位结论

1. **ApiConfig 数组含 key 全量拉取无视前端 5 分钟缓存 — 存活, 行号未变**: `src/features/chat/adapters/TauriAdapter.ts:3402`(getValidChatModelIdSet → `get_api_configurations`)。后端 commands.rs:1572-1583 返回完整 ApiConfig(用户 api_key 明文过 IPC, 仅 is_builtin 打码"***"); 前端只消费 id。放大面: 同一命令另有 9 处裸调无缓存(ModelPicker.tsx:153、TranslationPopover.tsx:155、ModelPanel.tsx:91、Settings.tsx:356、useSettingsConfig.ts:40、configApi.ts:91、WebSearchAdvancedConfig.tsx:154、voice-input/runtimeConfig.ts:31、useAvailableModels.ts:91), 唯一 5 分钟缓存在 useAvailableModels.ts:79-81/139 且仅覆盖派生模型列表。
2. **finderStore dstu_get N+1 (~100次) — 存活, 路径漂移**: 原 `finderStore.ts:562/673` → 现 `src/features/learning-hub/stores/finderStore.ts:562-570`(搜索路径)与 `:673-683`(loadItems 路径), 文件从 src/stores/ 移入 features/learning-hub/stores/。上限 recentStore.ts:77 maxItems=50, 含 fallback 重试最坏 ~100 invoke/次; recent 搜索 300ms 防抖(LearningHubSidebar)逐批重发; 前端 name/type 过滤在**全量 get 之后**执行(finderStore.ts:555-560), 而 recentStore 元数据已含 name/type——先过滤可直接砍掉绝大多数 get。

## 最小数据传递方案

- **配置类**: `get_api_configurations` 拆出 `get_api_config_ids`(id+enabled 投影, 不含 key); 设置面板全量视图仍用原命令。前端建一个共享 config 缓存模块(save 失效), 10 个调用点收敛到 2 个命令。
- **最近视图**: recentStore 元数据(name/type/accessedAt)先行过滤 → 仅对可见/幸存项 dstu_get; 根治为后端 `dstu_list_recent`(单次批量, recentStore 注释已预告)。
- **批量操作缓存失效**: 全部改 path 本地提取 sourceId, 消灭 batch 后的 N 次 get。
- **文件创建**: base64 单键化(后端两字段保留兼容一个版本即可); 拖拽链维持路径直传。
- **内容读取**: 注册 `dstu_get_content_paged`(helper 已在 content_helpers.rs:456), useDstuResource 改分页拉取; 大文本只在滚动/搜索需要时移动。
- **复习聊天**: 附件 base64 一次上传换 sourceId, 历史只传引用——对齐 chat_v2 已验证的引用模式。
- **设置键值**: `get/set_settings_batch` 键值对命令, 替换 13 路扇出与启动散点。
- **死面清理**: 删 chatV2Api.upsertStreamingBlock; vfsFileApi.list/get 若确认无消费可整段删或改摘要投影。

## 不动清单

- `local_api` v1.1 stable 端点(PLAN §8 全局约束)。
- dstu_get_content 全量语义本身(分页为新增命令, 非改签名——消费端 useDstuResource 需同步适配并过 tsc)。
- recentStore 前端持久化方案(zustand+localStorage, maxItems=50)——它正是 #1 修复的过滤数据源, 不是障碍。
- 剪贴板附件的 base64 上传分支(无本地路径, 属必要传输)。
- research_* 双键传参(后端桩未落地, 注释已声明为过渡策略, 提前收敛无收益)。
- dataGovernance.ts/memoryApi.ts/notesApi.ts/folderApi.ts 等 1:1 薄封装层——密度高但频率低、无循环, 改造无收益(密度≠开销的实证)。
- finderStore 搜索的 300ms 防抖与 requestId 取消机制——正确, 保留。
