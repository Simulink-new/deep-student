# B6 components+杂项 数据传递审计

> 撰写: 2026-09-11 08:24 CST | 基线 commit: ae1e6385 | 分支: main
> 范围: task-022 (components + debug-panel + voice-input + command-palette + mcp/mcp-debug + sandbox)

## 范围与方法

| 目录 | 规模 | 策略 |
|------|------|------|
| src/components/ | 69,163 行 / 206 文件 | 按"被高频视图引用度"分优先级全查热点 |
| src/debug-panel/ | 24,970 行 | 轻审——只记明显问题(开发工具属性) |
| src/voice-input/ | 3,226 行 | 全查(音频 buffer 流转专项) |
| src/command-palette/ | 5,000 行 | 全查(注册表重建/搜索扫描专项) |
| src/mcp/ + src/mcp-debug/ | 9,723 行 | 全查(长连接消息缓冲专项) |
| src/features/sandbox/ | 624 行 | 全查(实际零 IPC,直接通过) |

**优先级判定方法**: 以 `grep import` 统计被 chat/learning-hub 及全应用引用的共享组件频次——NotionButton(250 处)、UnifiedNotification(115)、custom-scroll-area(62)、CommonTooltip(31) 为最高频共享件,逐一深查;UnifiedDragDropZone(7 个消费方含 learning-hub 两个)、ModernSidebar(chat 侧栏)、CrepeEditor(2859 行,PLAN 遗留热点) 次优先;纯静态展示组件(icons/legal/previews 等)跳过。索引来源: idx-invokes.txt / idx-listens.txt 按目录过滤(共 74 处 invoke / 37 处 listen)。**未派生子代理,全部串行自查。**

## 数据流摘要

B6 区域的数据移动集中在三条链:(1) **拖拽导入链**——UnifiedDragDropZone 经 `read_file_bytes`(Vec\<u8\>→JSON number[])把整文件搬进前端构造 File,下游消费方(essay/translation/exam/csv)再用 FileReader 转 base64 经 invoke 送回后端,而 learning-hub 路径导入场景下这些字节完全被丢弃(侧栏 files 回调被 flag 跳过,统一走路径直传);(2) **图片链**——笔记图片上传走 base64 双键传参(同一字符串在 payload 出现两次),编辑器渲染经 proxyDomURL 每次全量拉 base64 data URL,题库/裁剪对话框也是一次性全量 base64;(3) **侧栏刷新链**——ModernSidebar 每次刷新发 3 个 invoke(含 groupId='*' limit=10000 全量已分组会话),且挂在 window focus 上。其余子系统(command-palette 快捷键索引、mcp 桥接与 stdio 传输、voice-input 录音、anki cardforge 事件流、debug-panel)经专项核查均为健康模式: 有界缓冲、订阅配对清理、TTL 缓存、防抖/防重入齐备。

## 发现清单

| # | 位置(file:line) | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
|---|------|------|------|------|------|------|
| 1 | src/components/shared/UnifiedDragDropZone.tsx:449 (+ 下游 EssayGradingWorkbench.tsx:382/533, TranslateWorkbench.tsx:271, ExamSheetUploader.tsx:479/661/699, CsvImportDialog.tsx:351) | IPC 大 payload + 多段拷贝: `read_file_bytes` 以 `number[]` 过 JSON(每字节一个 JSON 数字),前端拼 File 后下游又 FileReader→base64→JSON 送回后端——磁盘→number[]→File→base64→JSON 四次拷贝、两次跨 IPC。且 learning-hub 拖入时(LearningHubSidebar.tsx:994 `consumePathsDropHandledFlag` 跳过 files 回调)字节 100% 白读 | 单文件 10MB: JSON 数组文本 ~35MB → V8 双精度数组 ~80MB → Uint8Array 10MB → dataURL 13.7MB → base64+JSON 再 ~27MB,瞬时 ~165MB、主线程 JSON.parse 阻塞数百 ms;maxFileSize 默认 50MB,最坏瞬时 ~600MB(假设: JSON 数字平均 3.5 字符、V8 数组 8B/元素) | 每次拖拽导入(教材/笔记/作文/翻译/试卷/CSV——用户可感知的卡顿点) | 复用 chat 侧已验证的"路径直传"范式(vfsRefApi.ts:604 UploadAttachmentByPathParams): zone 对 Tauri 本地路径只传 path+元数据,后端读盘;仅无路径来源(浏览器拖入/剪贴板)才构造 File。下游 import_question_bank_stream / parse_document_from_base64 / essay 管线加可选 path 字段 | 中: 需后端命令加 path 入参 + 前端同步适配 + tsc;分视图逐个切换可独立回滚 |
| 2 | src/components/crepe/features/imageUpload.ts:108-116 (同模式: src/utils/configApi.ts:45,B2 交叉引用) | 参数双传: `notes_save_asset` 同时传 `base64_data` 与 `base64Data`(后端签名 cmd/notes.rs:685 只有 `base64_data`,Tauri 2 将 camelCase 自动映射到同一参数)——同一 base64 字符串在 JSON 消息里上线两遍 | 图片 5MB → base64 6.7MB → IPC payload ~13.4MB,直接翻倍(实测后端只反序列化一次) | 每张笔记图片上传(粘贴截图频率高) | 删除 snake_case 三个键(`base64_data`/`note_id`/`default_ext`),保留 camelCase;configApi.ts 同理 | 低: 纯前端删冗余键;tsc + 手工上传一张图验证 |
| 3 | src/components/ModernSidebar.tsx:455-472, 536-538 | 重复 invoke + 全量拉取: 每次刷新 3 个 invoke,其中 #1(limit=8)与 #3(groupId='*' limit=10000) 数据重叠;且 window `focus` 监听器也触发全刷 | 假设 300 个已分组会话 × ~500B/行(ChatSession 含 metadata JSON) ≈ 150KB×2 + groups,每次聚焦窗口全量重拉重排序 | 每次窗口聚焦(alt-tab 高频) + 每个会话生命周期事件(create/rename/summary_updated 派发 chat-v2:sessions-updated,5 个派发点) | (a) focus 监听移除或加 staleness 节流(>30s 才刷);(b) 后端加单命令返回 {recent, grouped} 投影或让 #3 只返回分组归类所需字段(id/groupId/title/updatedAt) | 低: (a) 纯前端;(b) 新增命令不动旧契约 |
| 4 | src/components/ImageCropDialog.tsx:81 (后端 commands.rs:5672) | IPC 全量 vs 按需: `qbank_get_source_images` 一次返回整套试卷所有页面的 base64 data URL,UI 却是单页翻页显示(currentPage) | 20 页扫描件 × 2MB/页 → base64 ~53MB 单条 JSON 响应,全部常驻 React state(假设: 扫描件 2MB/页) | 每次打开题目裁剪对话框 | 后端加 `pageIndex`/`limit` 参数分页返回;前端按需拉当前页±1,翻页时再取 | 低-中: 后端命令签名变化需前端同步;可保留无参形式兼容旧调用 |
| 5 | src/components/crepe/features/imageUpload.ts:237,264 | 无缓存重复传输: proxyDomURL 把 notes_assets 路径经 `get_image_as_base64` 全量拉 base64→data URL 塞进 DOM 属性,本层零缓存——重开同一笔记/图片重新渲染即重新整张拉取;data URL 字符串(1.33x 图体积)驻留每个 img 的 src | 笔记含 10 张 2MB 图 → 每次打开 26MB 传输 + 26MB DOM 字符串常驻;编辑期间是否重复触发取决于 Milkdown 重解析时机(待验证) | 每次打开含图笔记;切换笔记往返必重拉 | 模块级 `Map<relativePath, dataUrl>` LRU(上限 ~50 张/单笔记量),笔记卸载时 revoke;macOS asset 协议限制的注释保留(仍走后端,只加缓存) | 低: 纯前端缓存层,行为不变 |
| 6 | src/components/QuestionBankEditor.tsx:733-741 | 缓存策略: 题目图片缓存上限 50 条 base64 data URL,但淘汰按 Object.keys 插入序 `slice` 而非 LRU——照片型题库最坏 ~50×2.7MB(base64) ≈ 130MB 常驻,且可能删掉刚看过的条目导致重拉 | 触发上限内无开销;跨题连续刷题(>50 张图)内存线性爬升到上限(假设: 平均图 2MB) | 连续刷题跨题翻页(高频学习路径) | 已有 vfs_get_attachment_content 并行+竞态保护,补: 存 blob: URL 代替 data URL 字符串 + 真 LRU 淘汰 + revokeObjectURL | 低: 纯前端缓存表示替换;注意 blob URL 生命周期 |

## 最小数据传递方案

B6 理想形态按三条链收敛:

1. **拖拽导入链——"路径只过一次桥"**: 本地文件的用户意图是"让后端处理这个文件",前端不需要字节。zone 层对 Tauri 路径来源只传 path(+size/mime 由 get_file_size 一次拿到),字节跨越 IPC 的场景仅限浏览器拖入/剪贴板粘贴等无路径来源。每个下游管线(import_question_bank_stream、parse_document_from_base64、essay、csv)接受 path 或 bytes 二选一。一次拖拽 = 1 个 get_file_size + 1 个业务 invoke,数据全程只在后端磁盘↔内存之间移动。
2. **图片链——"引用与字节分离"**: markdown/题目正文中只存路径引用(现状已如此,P0-18 修复方向正确),字节侧统一 blob: URL + 前端 LRU 缓存(上限按张数),上传侧消灭双键传参,大列表(裁剪对话框)按页拉取。图片字节在任何链路上最多完整移动一次,重复展示走缓存。
3. **侧栏刷新链——"事件驱动 + 投影"**: 会话列表刷新由生命周期事件触发(去掉 focus 全刷),后端提供按用途投影的轻量字段集(id/title/groupId/updatedAt/pin),recent+grouped 合并为单命令单往返。

## 不动清单

| 项 | 原因 |
|----|------|
| useEventRegistry (src/hooks/useEventRegistry.ts) | add/remove 配对、deps 驱动重挂载,核查干净;ModernSidebar/CommonTooltip 等全部使用者无泄漏 |
| command-palette 全子系统 | 快捷键索引按版本号缓存重建(CommandPaletteProvider.tsx:156-175)、订阅有清理、search 全量扫描在 ~100 命令×5 字段量级下 <1ms/按键,交互频率无感知 |
| voice-input 全子系统 | MediaRecorder 压缩块+停止时单次 Blob,单次 base64 invoke;PCM 降级路径双缓冲仅罕见 fallback,录音时长受用户交互约束无无界增长 |
| mcp/ + mcp-debug/ | stdio 传输监听 per-session 前缀+close/remote-closed 双路清理;桥接按 correlationId 请求-响应无累积;工具缓存 TTL+localStorage 持久化有界;McpDebugPlugin MAX_ENTRIES=1000+脱敏 |
| anki/cardforge 三引擎 | 事件透传无累积、监听防重入门闩、文档流有暂停/5 分钟超时边界;TaskDashboard 卡片展开时懒加载+守卫。模板 per-session 重复拉取影响小(显式展开动作) |
| CrepeEditor.tsx:1128-1160 250ms 变更发射 | PLAN 遗留热点核实为**已缓解**: 防抖在连续输入时不断重置(停顿 250ms 后才序列化一次)、IME 合成态守卫、内容未变早退——归档为已修复,REPORT 汇总时可销项 |
| UnifiedNotification / NotionButton / CommonTooltip / custom-scroll-area / AppMenu | 计时器/监听全清理;NotionButton 无 memo 但属叶子按钮单 DOM diff 极廉;调试发射均有 debugMasterSwitch 或 DEBUG_MODE=false 常量门 |
| debug-panel(整体) | 轻审通过: 插件按需激活、日志有界、门控齐全;开发工具属性不深挖 |
| DataImportExport waitForJob 双通道(轮询+事件) | 设置页显式动作、任务时长有界、双通道互为兜底且都清理——P2 务实标准不收 |
| features/sandbox | 零 invoke 零监听 |
| TaskDashboardPage.tsx:445 failedTasks.map 并行 invoke | 显式用户重试动作,N=失败任务数,可接受 |

## 与其他批次的交叉引用

- 发现 #1 的复合链路中 `useTauriDragAndDrop.ts:265`、`resourceDropImport.ts:119`、`TextbookContentView.tsx:371` 同为 `read_file_bytes` number[] 模式(6 处),分属 B1/B4——建议 REPORT 汇总时合并为一个"消灭 number[] 字节搬运"横切优化任务。
- 发现 #2 的 configApi.ts 双键模式属 B2 范围。
- "ApiConfig 全量拉取"(WebSearchAdvancedConfig.tsx:154) 属 B5 热点表既有条目,本批次不重复收录。
