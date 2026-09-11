# B1 前端 stores+hooks 数据传递审计

> 撰写: 2026-09-11 13:30 CST | 基线: main@ae1e6385 + 工作树脏文件 | rubric: PLAN.md §3/§6
> 说明: 本批次为重试（上次尝试丢失）。全程串行精读，未派生子代理。

## 范围与方法

| 路径 | 文件数/行数 | 方法 |
|------|------------|------|
| src/stores/（11 顶层 .ts + anki/ 3 文件） | 14 文件 / 4,618 行 | 全量逐文件精读 |
| src/hooks/（35 文件 / 7,251 行） | 35 文件 | 轮询类+监听类+数据类全读；纯 UI 类（countdown/debounce/focusTrap 等 12 个）grep invoke/listen/interval 模式关闭 |
| src/contexts/ | 1 文件（DialogControlContext.tsx, 30KB） | 全读 |
| src/events/ | 1 文件（chat.ts, 纯常量+类型安全 dispatch 包装） | 全读，无数据传递热点 |
| src/store/ | 1 文件（ResourceStateManager.ts） | 全读 + 全库引用核查 |
| src/essay-grading/（前端侧, 2,158 行） | 11 文件 | 快速过：useEssayGradingStream + GradingStreamRenderer + streamingMarkerParser 精读，其余纯函数 grep |

**store 清单**（zustand, 除注明外均无 persist middleware）：

| store | 行数 | 形态 | 初评 |
|-------|------|------|------|
| questionBankStore | 1,629 | invoke 薄包装 + Map/Order | 中等问题（Map 全拷贝/裸订阅消费方） |
| reviewPlanStore | 754 | invoke 薄包装 | 良好（B4 已覆盖分页与 UTC 修复） |
| researchStore (Hpias) | 517 | 事件流归约器 | 多处流式 O(n²) |
| anki/useAnkiUIStore | 485 | UI 切片 + useShallow selector | 良好（范例级） |
| ankiQueueStore | 248 | 手动持久化（TauriAPI.saveSetting） | 全量 stringify 写放大 |
| unifiedIndexStore | 170 | invoke 薄包装 | 良好 |
| templateAiStore | 135 | 流式状态容器 | 中等（调用方逐 chunk set） |
| 其余 7 个（settingsShell/systemStatus/network/ui/view + anki/index/types） | ~760 | 小状态/类型 | 干净；uiStore 用 throttledStorage |

**交叉引用（勿重复审计）**：finderStore watch('*') 全量刷新链与 LearningHubSidebar.tsx:121 裸订阅 → B4 #2/#3；ApiConfig 全量拉取 TauriAdapter.ts:3402 → B2 #3；reviewPlanStore UTC bug 前端已修（reviewPlanStore.ts:23-26 localTodayISODate）→ B4 热点重定位①。

## 数据流摘要

前端状态层数据流分四类。(1) **页面打开型拉取**：统计页（useChatV2Stats 2×1000 会话全量）、题集打开（useQuestionBankSession 串行全分页）、设置页（useVendorModels 3 invoke、useSystemSettings 8 invoke、DialogControl ~11 invoke）——均为一次性全量，无跨实例共享缓存（useStatisticsData 是唯一例外，自带全局缓存+inflight 去重+60s 轮询）。(2) **流式事件归约**：researchStore（HPIAS 深研引擎 35+ 事件类型逐条 set）、essay/qbank AI 评判（delta 协议已就绪，前端逐 chunk 拼接+全文重解析）、TemplateAIEngine（逐 chunk setStreamState）——共同模式是每 chunk 一次 store 写 + 一次订阅者通知。(3) **持久化**：仅 uiStore（throttled persist，1 字段）与 ankiQueueStore（手动 latest-wins 合并队列，但每次变更全量 stringify 含完整文档内容的队列）。(4) **context 广播**：DialogControlContext 聚合 MCP 工具/搜索引擎可用性，value 对象未 memo，任一 state 变化重渲染全部消费者。

## 发现清单（影响降序，上限 20）

| # | 位置(file:line) | 模式 | 复杂度/量级估计(假设) | 触发频率 | 修复草图 | 破坏风险 |
|---|----------------|------|----------------------|----------|----------|----------|
| 1 | src/hooks/useChatV2Stats.ts:141-206 | IPC 边界: 拉全量算聚合——`chat_v2_list_sessions` ×2（active+archived 各 limit 1000）到前端，再 O(7×N) 内存过滤算 mode 分布/近 7 天/小时分布；消息数靠 `chat_v2_get_message_summary` 单独查，失败时前端臆造 `sessions×10` 估算值 | 假设重度用户 ~1500 会话 × ~400B ≈ 0.6MB JSON 过 IPC；聚合本身 O(kN) 可忽略——瓶颈纯在 payload | 每次打开统计页/DataImportExport（2 个调用方均 autoRefresh=false，无轮询） | 后端新增聚合命令（SQL GROUP BY mode/date/hour 一次返回计数），前端 2 调用方换 invoke；顺带消灭 `recentMessages=total×0.3` 假数据 | 低：新命令+适配 2 处调用，tsc 可验 |
| 2 | src/hooks/useQuestionBankSession.ts:173-222（消费方 ExamContentView.tsx:93） | IPC 边界: 串行全量分页——`fetchAllQuestions` while(hasMore) 逐页 invoke（50/页）拉全部题目，page_size 分页机制形同虚设；每题携带 content/answer/explanation/ai_feedback/images | 假设 OCR 试卷题集 N=200-500 题 × ~2KB ≈ 0.4-1MB，分 ceil(N/50)=4-10 个**串行** invoke（延迟叠加）；每次答题/切题集后再 O(N) Map 复制 | 每次打开题集 tab（ExamContentView 挂载即 loadQuestionsImpl） | 首页 eager + 其余页按需 `loadMoreQuestions`（已存在）；随机/模拟考试模式确需全集时一次性大 page_size（如 500）单 invoke 拉全，消串行 | 中：practiceMode=random/shuffle 可能依赖全集——需先核对 ExamContentView 各模式对 questions[] 的使用；**注：B4 曾评 questionBankStore 分页"现状良好"，但活跃消费方是本 hook 绕过 store 的全量路径，B4 结论对此链路不成立** |
| 3 | src/stores/researchStore.ts:373-382（synthesis_updated）+ 218-505（handleEvent 整体） | 事件流: 每 chunk `state.synthesis + delta` 全量字符串拼接并写入 `roundsView[rno].summary_md`，每 chunk 一次 set → 订阅者重渲染；无合批/节流 | 假设 synthesis 10-50KB × 200-1000 chunks → 累计 5-50MB 字符复制 + 同数量次 store 通知/重渲染 | 每个深研会话的 synthesis 流式阶段 | chunk 累积进模块级 ref，rAF/100ms 节流写 store（仅写增量长度或节流快照）；complete 时写权威全量——与 eventsLog 已做的"移出 store"（L81-137, P0-3）同思路 | 低-中：流式显示延迟 +100ms，需过 Cockpit 渲染核对 |
| 4 | src/contexts/DialogControlContext.tsx:614-648 | 前端订阅: `value` 对象未 useMemo——7 个 useState 任一变化（含每次 MCP 连接状态推送 L568-586）重建 context 对象 → 全部 useDialogControl 消费者重渲染 | 消费者=聊天面板等（数量待 B3 核，假设 5-15 个组件树）；每次 MCP status 推送/工具列表刷新触发一轮全量 reconcile | 初始化、每次 systemSettingsChanged（MCP/web_search 前缀）、每次 McpService.onStatus 推送 | `React.useMemo` 包 value（deps: 7 个 state + 已 useCallback 的 setter）；setSelectedMcpServers 内联箭头函数一并 useCallback | 低：纯机械，漏 dep 由 tsc/eslint 兜底 |
| 5 | src/stores/ankiQueueStore.ts:54-124 | 序列化/持久化写放大: 每次队列变更 `JSON.stringify` 整队列，material 含完整 `content`（全文）与 `snapshot`（整副卡片）——latest-wins 合并只压 IPC 次数不压体积 | 假设队列 K≤20 条 × content 10-100KB + snapshot ≈ 0.2-2MB/次 flush；每 add/remove 一次 | 每次"加入队列"（聊天摘录/错题导入） | 队列元数据与内容分离：content/snapshot 首次入队单写后端（键=queueId），队列文件只存 id 引用；或 flush 前按 size 阈值改增量 | 中：需后端配合或 localStorage 结构迁移，旧数据要兼容读 |
| 6 | src/essay-grading/GradingStreamRenderer.tsx:70-93 + streamingMarkerParser.ts:141-258 | 消费端重复计算: `parseResult = useMemo(parse, [content])`，content 每 chunk 增长 → 每 chunk 对全文重跑 6 组 marker regex + 重建 markers 数组 | 假设批改结果 3-10KB × 100-300 chunks → O(n²) 字符扫描 ≈ 1-3M 次正则步进 + 同数量次 markers 数组分配 | 每次作文/评判流式期间 | 流式期节流解析（200-500ms 或每 N chunk）+ complete 权威解析；或利用 parser 已有的 pending-bracket 概念从上次完成偏移增量解析 | 低-中：流式中途高亮短暂滞后，最终渲染以 complete 全量解析兜底 |
| 7 | src/stores/researchStore.ts:345-352/371（subagent_thought/tool_call/tool_result/failed） | 内存: `steps: [...prev, newStep]` 无上限累积，每事件复制整个 steps 数组 + subAgents 整对象 spread | 假设每 subagent M=20-50 步 × S=5 个 subagent → 每 subagent O(M²) 复制；eventsLog 已有滑动窗口但 steps 无界 | 每个深研会话全程，每 thought/tool 事件一次 | steps 设上限（保留末 K 条，UI 只显示最近活动）或改为 push+版本号跳过不可变复制 | 低：需核对 steps 的 UI 展示是否依赖完整历史 |
| 8 | src/hooks/useVendorModels.ts:156-166 + 203-239 | 冗余往返: persistVendors/persistModelProfiles 保存后 dispatchVendorModelChange('api_configurations_changed') → 自身监听器 loadAll() 再全量拉 vendors+profiles+assignments 3 invoke——尽管本地已 setVendors(next)；广播 detail 还携带全量数组 | 每次厂商/模型 CRUD = 1 save + 3 冗余读 invoke；广播 payload = 全量配置数组复制给所有监听者（含 B2 #3 的 TauriAdapter 链） | 设置页每次编辑操作 | 广播 detail 加 `origin:'self'` 标记，自身监听跳过 reload（本地 set 已同步）；或 detail 只带变更的 id 列表 | 低：注意第三方监听者（TauriAdapter）语义需保持 |
| 9 | src/hooks/usePdfProcessingProgress.ts:204-228 + 294-303 | 重复监听+无界注册: unified（media-processing-*）与 legacy（pdf-processing-*）双套各 3 监听器并行，若后端同一流水线两者都发则每条进度双处理（store 双写+缓存双失效）；`ocrSessionToFileId` 模块级 Map 只增不删 | 双处理量级=2×（每页一次 store update）；Map 随历史 OCR 会话线性增长（每条 ~100B，长会话累积 MB 级以下） | 每次媒体处理全程 | A9/A11 先核实后端是否仍双发——若 unified 已全覆盖则前端删 legacy 3 监听；ocrSessionToFileId 在 Completed/Failed 分支 delete | 低（删监听需后端 emit 面核实，标待验证） |
| 10 | src/components/practice/TimedPracticeMode.tsx:65、PaperGenerator.tsx:78、MockExamMode.tsx:81、DailyPracticeMode.tsx:66 | 前端订阅: `useQuestionBankStore()` 无 selector 整库解构——isSubmitting 每次答题翻转 2 次、pagination/stats/learningTrend 任一 set 都重渲染整组件 | 4 个模式组件（模态挂载，通常同时 1 个）；每次答题 → ≥2 轮全组件 reconcile（叠加 #14 的 Map 复制） | 每次答题/收藏/筛选 | 换用文件尾已导出的逐字段 selector hook（useCurrentQuestion/useQuestionBankStats 等） | 低：机械替换 + tsc |
| 11 | src/contexts/DialogControlContext.tsx:178+204（mcp.tools.list 同一 reload 读 2 次）+ 392-402（9 个 web_search key 逐个 getSetting） | IPC 边界: 一次 reloadAvailability ≈ 11+ 次独立 getSetting invoke | 每 reload 11-13 invoke（有 1200ms 冷却与 in-flight 复用兜底） | 初始化、settings 变更、MCP bootstrap 完成各一次 | `mcp.tools.list` 读一次复用；后端提供多 key 批量 get_settings（或前端 300ms 合并窗口） | 低 |
| 12 | src/engines/TemplateAIEngine.ts:44-88（写入 templateAiStore.ts:107-110） | 事件流: 每 chunk `setStreamState({currentContent: 拼接结果})` 无节流——store 通知 + 订阅者重渲染逐 chunk 发生 | 模板输出 1-5KB × 50-200 chunks；已有 is_complete 覆盖护栏防末块重复拼接 | 每次模板 AI 生成流 | 与 #3 同法：ref 累积 + rAF/100ms 节流写 store | 低 |
| 13 | src/stores/researchStore.ts:402-417（macro_insight_progress）+ 328-340（candidate_ranking_started） | 序列化往返: 每条进度事件 `JSON.parse(metrics_json)` 合并再 `JSON.stringify` 写回——store 内部以 JSON 字符串为中间态 | 假设 metrics_json 1-5KB × 洞察分块数（50-200）→ 每 chunk 一次 parse+stringify | 每个深研会话的 macro insight 阶段 | metrics 存活对象进 store，会话结束才序列化进 roundsView 快照 | 低 |
| 14 | src/stores/questionBankStore.ts:948/972/1037/1061（getQuestion/updateQuestion/submitAnswer/toggleFavorite） | 拷贝: 每次 mutation `new Map(state.questions)` 全量复制 | N≤10,000（内存保护阈值）/通常 50-500 → 每次 O(N) 复制，量级小但叠加 #10 裸订阅放大渲染 | 每次答题/编辑/收藏 | 现量级可接受；若题集普遍 >2k 再改版本号+可变 Map | 中：可变化打破 zustand 不可变约定，非必要不动 |
| 15 | src/store/ResourceStateManager.ts（全文件, 9.8KB）、src/features/settings/hooks/useVendorSettings.ts（全文件）、useAnkiUIStore.appendDocumentContent（stores/anki/useAnkiUIStore.ts:55） | 死代码: 前两者全库零引用（ResourceStateManager 为 HIGH-007 遗留，CLAUDE.md 05-30 resource 模块清理漏网；useVendorSettings 被 VendorSettingsContext 同名 hook 取代），后者零调用 | 纯空间成本 ~15KB + 阅读噪音；无运行时开销 | — | 删除（各自独立 commit 可回滚）；appendDocumentContent 随 useAnkiUIStore 一并删 | 无（tsc 验证零引用） |

## 最小数据传递方案

该层理想形态：**聚合下推、增量流经、按需展开、引用持久化**。
1. **聚合下推后端**：统计类（#1）前端只需要计数与分布，SQL GROUP BY 一次返回；会话实体不过边界。
2. **列表按需展开**：题集（#2）默认首页+滚动加载，全集仅在确需 shuffled 全量的模式一次性大页拉取（1 invoke 取代 k 串行）。
3. **流式三段式**：chunk → 模块级 ref 累积（零通知）→ rAF/100ms 节流快照进 store → complete 权威全量替换。researchStore synthesis/templateAi/#6 解析同构适用；essay/qbank 已走 delta 协议，只差节流一环。
4. **持久化存引用**：ankiQueue（#5）队列文件只存 id+元数据，内容单写不复写。
5. **订阅逐字段**：selector 化（#4/#10），广播事件带 origin 标记避免自触发全量 reload（#8）。
6. **净删除**：死代码三处（#15）零行为差异。

## 不动清单

- **useStatisticsData.ts**：全局缓存 + inflight 去重 + requestIdleCallback 首载 + 60s 轮询共享 key——本层最佳实践，作为其他 hooks 的模板。
- **useTauriEventListener / useEventRegistry / useBackupJobListener / useMigrationStatusListener**：unlisten/卸载竞态防护完备，无注册表泄漏。
- **useAppUpdater.ts**：fork 更新通道已断（UPSTREAM_UPDATES_ENABLED=false），启动检查零网络请求。
- **useQbankAiGrading.ts / useEssayGradingStream.ts 前端侧**：delta 增量协议已实现（feedback 逐 chunk 拼接的 O(n²) 在 3-10KB 量级可接受）；后端 accumulated 全量 emit 未修属 A8 范围。
- **uiStore throttled persist、usePdfLoader LRU(5 文件/100MB)、useNavigationHistory MAX_HISTORY_LENGTH、useTheme、useAnkiUIStore useShallow 切片**：现状良好。
- **B4 已覆盖项**（勿重复改）：finderStore watch('*') 链、LearningHubSidebar 裸订阅、reviewPlanStore loadAllPlans(1000)/UTC 修复、questionBankStore 自身分页（本文件 #2 指出的 useQuestionBankSession 绕行路径除外）。
- **useCountdown 250ms tick**：仅倒计时活跃期运行且只更新数字，改 500ms 无感知收益。
- PLAN §8 全局不动项（local_api 契约、UI 行为、既有数据、他人脏文件）。
