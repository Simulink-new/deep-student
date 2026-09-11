# A6 LLM 流式底座数据传递审计

> 撰写: 2026-09-11 08:06 CST | 基线 commit: ae1e6385 | 分支: main
> 范围: task-011 llm_manager + llm_usage | 前置: PLAN.md §3 rubric / §6 格式

## 范围与方法

| 路径 | 行数 | 内容 | 采样策略 |
|------|------|------|----------|
| src-tauri/src/llm_manager/model2_pipeline.rs | 5,325 | 两条流式管线 + 非流式 + raw_prompt 族 | 全量精读(emit 密集区逐行) |
| src-tauri/src/llm_manager/streaming.rs | 1,288 | SSE 工具/取消/消息合并/题目流式解析 | 全量精读 |
| src-tauri/src/llm_manager/model_profile_service.rs | 1,519 | 配置解析/模型选择 | 精读配置解析链(640-800) |
| src-tauri/src/llm_manager/vendor_config_service.rs | 740 | vendor bootstrap/repair | 精读 bootstrap/repair |
| src-tauri/src/llm_manager/exam_engine.rs | 1,360 | 试卷 OCR 分割 | 精读 emit_deepseek_debug 全部 15 调用点 |
| src-tauri/src/llm_manager/rag_extension.rs | 1,390 | 嵌入/重排/翻译/标签 | 签名扫描 + 标签路径精读 |
| src-tauri/src/llm_manager/{mod,parser,tool_call,config_types,image_processing}.rs + adapters/ | ~2,600 | 结构/解析 | 结构扫描 |
| src-tauri/src/llm_usage/{collector,database,repo,handlers,types}.rs | 3,160 | 记账 | collector/handlers 全读, repo SQL 点扫描 |
| 消费端核对(只读) | — | TauriAdapter.ts / TemplateAIEngine.ts / debug-panel 插件 | emit↔listen 配对抽样 |

方法: 从 idx-emits.txt 过滤 llm_manager 得 42 个 emit 点全部分类; 沿 "产生(HTTP/SSE)→移动(IPC/DB/文件)→消费(渲染/记账)" 三层链路精读; 交叉核对前端监听面。

## 数据流摘要

**流式主路径**: chat_v2 pipeline (tool_loop/multi_variant) → `call_unified_model_2_stream` (model2_pipeline.rs:750)。每次调用: 全量重读模型配置(无缓存, 6-8 次 settings 读 + 2-4 次全量 JSON 解析) → `chat_history.to_vec()` 全量克隆 → `merge_consecutive_tool_calls` 二次逐消息克隆 → 逐条 `json!` 重建全部历史消息 → 请求体经历 **sanitize 深拷贝 + to_string 测长 + 每次重试 `.json()` 再序列化 + to_string_pretty 审计日志** 共 3-4 次全量序列化 → SSE 逐行增量解析(SseLineBuffer + ProviderAdapter, 逐事件处理, 无整响应缓冲) → ContentChunk/ReasoningChunk **按增量 delta** 发出(hook 注册时走 chat_v2 批量事件队列, 无 hook 时直接逐 chunk IPC emit) → 请求结束 `record_llm_usage` 一条入 llm_usage 批量采集器(channel 1000 + 50 条/5 秒批刷)。

**第二条管线**: `call_unified_model_stream_with_config` (model2_pipeline.rs:2626, 服务总结/续写/多变量等)——同样组装成本, 但**无 hook 路径**: 每 chunk 直接 `window.emit`, 且成功后终块把 `full_content.clone()` **全量回声**重发一遍(reasoning 同样)。

**记账**: llm_usage 采集健康——每请求一条记录(非每 chunk), 有界 channel 防背压, 后台事务批插。另有 DebugLogger("start"/"end") 每请求 2 条入队(队列缓冲, ERROR 才立即刷盘)。

**取消**: watch channel + registry 双通道, 流循环每 chunk 检查; 取消即 break, 缓冲均为局部变量不泄漏; 重试仅发生在 HTTP 状态码失败时(未读响应体), 不存在"重试重新缓冲已收内容"问题。

## emit 点分类总表(42 行)

图例: 触发时机 ∈ {每chunk, 每请求, 每取消, 每错误, 每工具结果, 条件分支}; payload ∈ {增量delta, 全量, 小(<1KB), 中(1-50KB)}。se = stream_event。

### model2_pipeline.rs — call_unified_model_2_stream(主管线, 17 点)

| # | 位置 | 事件 | 触发时机 | payload |
|---|------|------|----------|---------|
| 1 | model2_pipeline.rs:710 | `chat_v2_llm_request_body` | 每请求(chat_v2 流, 经 log_and_emit_llm_request) | **全量**(脱敏后完整 messages, 含全部历史) |
| 2 | model2_pipeline.rs:1451 | `{se}_memory_sources` {"stage":"disabled"} | 条件分支(模型不支持工具+memory 开) | 小 |
| 3 | model2_pipeline.rs:1633 | `{se}_start` (id=request_id) | 每请求 | 小 |
| 4 | model2_pipeline.rs:1813 | `{se}_start` (id=se) — 与 #3 重复 | 每请求 | 小 |
| 5 | model2_pipeline.rs:1824 | `{se}_id` | 每请求 | 小(疑孤儿) |
| 6 | model2_pipeline.rs:1875 | `{se}_cancelled` | 每取消 | 小 |
| 7 | model2_pipeline.rs:1934 | `{se}` StreamChunk | 每 chunk(无 hook 时; 有 hook 走队列) | **增量delta** |
| 8 | model2_pipeline.rs:1954 | `{se}_reasoning` StreamChunk | 每 reasoning chunk(同上) | **增量delta** |
| 9 | model2_pipeline.rs:2098 | `{se}_usage` | 每请求(usage 事件到达, 通常 1 次) | 小 |
| 10 | model2_pipeline.rs:2108 | `{se}_safety_blocked` | 每错误(安全拦截, 罕见) | 小 |
| 11 | model2_pipeline.rs:2121 | `{se}_error` (safety) | 罕见 | 小 |
| 12 | model2_pipeline.rs:2229 | `{se}_error` (流读错误) | 每错误 | 小 |
| 13 | model2_pipeline.rs:2233 | `stream_error`(全局) | 每错误 | 小 |
| 14 | model2_pipeline.rs:2271 | `{se}` StreamChunk(SSE 尾部残余) | 每请求尾 | **增量delta** |
| 15 | model2_pipeline.rs:2290 | `{se}_reasoning`(尾部残余) | 每请求尾 | **增量delta** |
| 16 | model2_pipeline.rs:2494 | `{se}` final_chunk | 每请求成功(无 hook 时) | **全量回声**(full_content.clone()) |
| 17 | model2_pipeline.rs:2514 | `{se}_reasoning` final | 每请求(CoT 开且无 hook) | **全量回声**(reasoning 全量) |

### model2_pipeline.rs — call_unified_model_stream_with_config(第二管线, 17 点)

| # | 位置 | 事件 | 触发时机 | payload |
|---|------|------|----------|---------|
| 18 | model2_pipeline.rs:2897 | `{se}_memory_sources` disabled | 条件分支 | 小 |
| 19 | model2_pipeline.rs:3075 | `{se}_start` | 每请求 | 小 |
| 20 | model2_pipeline.rs:3112 | `{se}_error` (HTTP) | 每错误 | 小 |
| 21 | model2_pipeline.rs:3117 | `{se}_end` (error+stats) | 每错误 | 小 |
| 22 | model2_pipeline.rs:3166 | `{se}_cancelled` | 每取消 | 小 |
| 23 | model2_pipeline.rs:3212 | `{se}` StreamChunk | 每 chunk(**此管线无 hook, 恒直发**) | **增量delta** |
| 24 | model2_pipeline.rs:3228 | `{se}_reasoning` | 每 chunk | **增量delta** |
| 25 | model2_pipeline.rs:3350 | `{se}_usage` | 每请求 | 小 |
| 26 | model2_pipeline.rs:3357 | `{se}_safety_blocked` | 罕见 | 小 |
| 27 | model2_pipeline.rs:3370 | `{se}_error` (safety) | 罕见 | 小 |
| 28 | model2_pipeline.rs:3470 | `{se}`(尾部残余) | 每请求尾 | **增量delta** |
| 29 | model2_pipeline.rs:3487 | `{se}_reasoning`(尾部残余) | 每请求尾 | **增量delta** |
| 30 | model2_pipeline.rs:3508 | `{se}_cancelled` — 与 #22 重复 | 每取消 ×2 | 小 |
| 31 | model2_pipeline.rs:3521 | `{se}_end` (cancelled+stats) | 每取消 | 小 |
| 32 | model2_pipeline.rs:3545 | `{se}` final_chunk | 每请求成功 | **全量回声** |
| 33 | model2_pipeline.rs:3555 | `{se}_reasoning` final | 每请求(CoT 开) | **全量回声** |
| 34 | model2_pipeline.rs:3568 | `{se}_end` (success+stats) | 每请求 | 小 |

### streaming.rs(7 点) + exam_engine.rs(1 点)

| # | 位置 | 事件 | 触发时机 | payload |
|---|------|------|----------|---------|
| 35 | streaming.rs:498 | `{se}_web_search` sources | 每工具结果(web_search 命中引用) | 中(引用数组) |
| 36 | streaming.rs:510 | `{se}_rag_sources` | 每工具结果(rag) | 中 |
| 37 | streaming.rs:522 | `{se}_memory_sources` | 每工具结果(memory) | 中 |
| 38 | streaming.rs:553 | `{se}_web_search`(按 source_type 分类) | 每工具结果(通用工具) | 中 |
| 39 | streaming.rs:567 | `{se}_rag_sources`(分类) | 同上 | 中 |
| 40 | streaming.rs:578 | `{se}_memory_sources`(分类) | 同上 | 中 |
| 41 | streaming.rs:828 | `mcp-bridge-tools-request` | 每请求(MCP 工具缓存 TTL 过期时, 默认 5min 一次) | 小(correlationId), 回程为工具清单 |
| 42 | exam_engine.rs:277 | `deepseek_ocr_log` | OCR 每页 ×15 个调用点(7 个阶段) | **小~全量**(1140 行 payload 含整页 OCR content "preview") |

**分类结论**: 42 点中 **12 点为每 chunk 增量 delta**(#7/8/14/15/23/24/28/29 及尾部残余), 结构正确; **5 点为全量/全量回声**(#1 全量请求体审计, #16/17/32/33 终块全量回声, 另 #42 含全页 content); 其余 25 点为低频小 payload 生命周期/错误/引用事件, 其中 #3+#4、#22+#30 为字面重复发射, #5(`_id`)、#21/#31/#34(`_end`) 前端未发现监听者(疑孤儿, 待 A11 配对审计裁定)。

## 发现清单(影响降序)

量级假设统一注明; "请求"指一次 LLM HTTP 调用, "turn"指工具循环一轮(每 turn ≥1 请求)。

| # | 位置 | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
|---|------|------|----------------|----------|----------|----------|
| 1 | model_profile_service.rs:646-664 + vendor_config_service.rs:17-119 | 配置解析链零缓存: 每次调用 bootstrap(2 读)→repair(全量读+解析 vendor/profile 两个 JSON)→runtime 再各读一次→assignments 再读 | 假设 20 个 profile+8 个 vendor: 每请求 6-8 次 settings 读、3-5 次全量 JSON 解析(数十 KB 每次)、O(V×P) 合并、builtin 清单加载; 长对话工具循环 N turn 重复 N 次 | 每 LLM 请求(全模块: 对话/标题/标签/嵌入/翻译/OCR 共 20 个 get_api_configs() 调用点) | LLMManager 内加 `RwLock<Option<Arc<Vec<ApiConfig>>>>` 缓存, save_vendor_model_configs/save_setting 相关键写入时失效; bootstrap/repair 移到写入路径 | 低: 纯后端, 失效钩子漏写会导致改配置不生效——需列出全部写入点(save_setting("vendor_configs"/"model_profiles"/"model_assignments")) |
| 2 | model2_pipeline.rs:1593+1602+1620-1629+1688 / 3003+2998+3025 | 每请求全请求体 3-4 次序列化/深拷贝: sanitize 深拷贝(1593) + to_string 测长(1602) + 每次尝试 `.json(&preq.body)` 再序列化(1688, 重试各一次) + log_and_emit 内 sanitize 二次拷贝+to_string_pretty(1620) | 假设长对话请求体 200KB: 每请求 ≥3 次全量序列化 ≈ 600KB+ 临时分配; 含 base64 图片时 sanitize 遍历成本按 MB 计 | 每 LLM 请求 × 重试次数 | (a) to_string 测长改为复用一次序列化结果作为 body(或 `preq.body` 序列化结果同时供测长); (b) 1593 处 sanitize 仅在 debug 日志开启时执行; (c) log_and_emit 与 1593 共享同一份 sanitized | 中: (a) 涉及 reqwest body 构造方式(.json 改 .body+Content-Type); 行为等价可测 |
| 3 | model2_pipeline.rs:701-712 (log_and_emit_llm_request) | `chat_v2_llm_request_body` 无条件把**完整脱敏请求体(全部历史)** emit 给前端 | 假设 100 条消息长对话 150KB: 每请求 150KB IPC + 后端 sanitize+pretty; 功能上是"请求体预览"审计面板 | 每 chat_v2 请求(含每个工具 turn) | 设置门控(debug.persist_logs 或新增 debug.emit_request_body, 默认关)+payload 截断(如每消息 500 字符); 或改为前端 invoke 按需拉取最近一次请求体 | 中: TauriAdapter.handleLlmRequestBody 消费此事件做请求体展示——需同步适配前端(该功能属调试展示, 截断可接受) |
| 4 | model2_pipeline.rs:2461-2465+2501-2506 / 3540-3544+3550-3554 | 终块全量回声: is_complete=true 的 final_chunk 携带 `full_content.clone()`(reasoning 同), 响应整体二次传输 | 假设 20KB 回复+30KB reasoning: 每请求多传 50KB IPC + 2 次全量 clone; 第二管线恒触发, 主管线仅无 hook 时 | 每请求成功(第二管线), 主管线 hook 失败注册时 | 终块 content 置空串仅作完成信号; 前端如依赖终块兜底拼接需改为信任增量流(核对消费方: TemplateAIEngine 按增量拼接) | 中: 需逐消费方核对是否有"终块兜底"逻辑; 第二管线消费方(multi_variant/总结)走 hook 时本就跳过 |
| 5 | exam_engine.rs:1140-1149 (emit_deepseek_debug→277) | OCR debug 事件携带整页 content: `"preview": content` 全页 OCR 文本无条件 emit+pretty 序列化双重输出 | 假设每页 OCR 3-8KB: 每页 1 次全量 IPC + to_string_pretty; 30 页试卷 ≈ 100-240KB 纯调试数据; 唯一消费者 DeepSeekOcrDebugPlugin(面板常关, 事件仍序列化分发) | OCR 每页 | preview 截断至 2000 字符 + 由设置(debug.ocr_verbose)门控全量; 其余 14 个调用点 payload 均小可不改 | 低: 仅调试面板展示内容变化 |
| 6 | model2_pipeline.rs:1932/1952/2017/2056 (get_hook 调用) | 每 chunk 2 次 hooks_registry 异步锁+Arc clone: `self.get_hook(stream_event).await` 每 ContentChunk/ReasoningChunk 各一次 | 假设 2000 chunk 流: 4000 次 tokio Mutex lock + registry HashMap 查找 + Arc 原子clone; 单次 ~1μs 级但持锁点在热路径 | 每 chunk | 流循环开始前解析一次 `let hook = self.get_hook(...).await` 存局部(注册/注销发生在请求边界, 循环内不变) | 低: hook 在流中途不会被替换(tool_loop 在请求间隙才 unregister/re-register); 若未来支持热替换需重新评估 |
| 7 | streaming.rs:595-757 (build_tools_with_mcp) | 每请求重读 7 个 settings + 重建工具 schema: web_search schema json! 构造、MCP 白名单过滤、prepare_external_tool 逐工具规范化 | 每请求 7 次 DB 读 + O(工具数) schema 组装(假设 30 个 MCP 工具 ≈ 数十 KB Value 构造); MCP 清单本身有 5min TTL 缓存(760)但过滤与组装每请求重做 | 每 LLM 请求(支持工具的模型) | 与发现#1 共用 settings 缓存; 组装结果按 (engines, whitelist, blacklist, selected, advertise) 键做 memo; 工具 schema Value 用 Arc 共享进请求体 | 低: 纯后端 memo, 键覆盖全部输入 |
| 8 | model2_pipeline.rs:820+858 (to_vec + merge_consecutive_tool_calls) | 历史消息双重克隆: `chat_history.to_vec()` 全量 Vec clone 后, merge 内部对每条消息再 clone 进 result/pending(streaming.rs:289/299/312) | 假设 100 条消息(含图片 base64 字段): 每请求 2× 全历史深拷贝; N turn 工具循环 ×N 次 | 每 LLM 请求 | merge_consecutive_tool_calls 改为消费 `Vec<ChatMessage>`(move), 消除第一层 to_vec; Regular 分支 push(msg) 不再 clone | 低: 函数签名改 `fn(impl IntoIterator<Item=ChatMessage>)`, 调用方传 move; 均为请求路径局部数据 |
| 9 | model2_pipeline.rs:1631-1642 与 1812-1822 | 双 `_start` 事件: 同一请求先发 id=request_id 再发 id=stream_event, 前端 TemplateAIEngine 收到两次 resetStreamState | 每请求 2 次小事件; 行为上第二次 reset 在 chunk 到达前发生故未实际丢数据, 但语义脏 | 每请求 | 删除 1633 处(保留 1813 与响应对齐的那次)或反之, 二选一 | 低: 需核对两个 id 消费方取哪个字段(TemplateAIEngine 仅用事件触发不用 id) |
| 10 | model2_pipeline.rs:1246-1260 (`_approx_tokens_in`) | 死计算仍全量遍历: token 估算遍历 system+context(每值 v.to_string() 序列化!)+全部历史, 结果带下划线前缀未使用(1552 注释"不再在此处估算") | 假设 context 含 50KB RAG 值: 每请求多一次 to_string 序列化 + 全历史遍历 | 每 LLM 请求(主管线) | 直接删除该块(已无消费者) | 低: 变量未使用, 编译器仅因下划线未告警 |
| 11 | model2_pipeline.rs:1824-1833 (`{se}_id`) + 3117/3521/3568(`{se}_end`) | 疑孤儿事件: `_id` 全前端无监听; `_end`(stats) 未发现前端监听(仅第二管线发送, 主管线反而不发——协议不对称) | 每请求 2-3 个小事件序列化+分发 | 每请求 | 待 A11 emit↔listen 配对审计确认后删除或补主管线对称; 本模块先记录 | 低: 删孤儿事件无 UI 影响, 但需 A11 兜底确认 |
| 12 | model2_pipeline.rs:3508-3517 | 第二管线 `_cancelled` 双发: 3166(循环内)与 3508(收尾)各一次 | 每取消 2 次小事件 | 每取消(第二管线) | 删 3508 处(保留循环内第一时间那次)或用 bool 去重 | 低 |
| 13 | model2_pipeline.rs:2021-2023/2059-2066/3281/3318 | 每 chunk stdout print! + flush: `print!("🔧")`/`print!("/")` 直写 stdout 并 flush | 每 chunk 1-2 次系统调用; GUI 应用 stdout 无消费者 | 每 chunk(含工具 args delta) | 改 debug! 宏(级别过滤) | 低 |
| 14 | model2_pipeline.rs:1308-1323 | 重复 debug 日志: 同一 custom_tools check debug! 连发两次(复制粘贴) | 每请求 2 次格式化 | 每请求 | 删一条 | 无 |
| 15 | streaming.rs:233-234 (cancel_streams_by_prefix) | 死代码克隆: `for _key in guard.clone().iter() {}` 克隆整个 cancel_registry HashSet 后空循环 | registry 数十条时每次调用一次全量 clone | 每次按前缀批量取消 | 删除该循环(或实现真正的 registry 前缀清理——现语义只清 channel 不清 registry, 顺带核对) | 低: 若实现 registry 前缀清理反而修复潜在取消残留 |
| 16 | model2_pipeline.rs:4868+4899 (raw_prompt 路径) | 非流式响应全量 clone+序列化: `response_json.clone()`(openai 分支) + `raw_response: Some(openai_like_json.to_string())` | 假设 OCR/标签响应 10-50KB: 每调用 1 次多余 clone + 1 次全量 to_string(raw_response 消费方多为忽略) | 每 raw_prompt 调用(标题/标签/compaction/OCR) | openai 分支 move response_json; raw_response 按消费方核对后可置 None 或截断 | 中: raw_response 字段在 StandardModel2Output 契约中, 需核对全部消费方(可能仅调试用) |
| 17 | model2_pipeline.rs:2568+3588 (第二管线双估算) | full_content 两次 estimate_tokens: 3567(end 事件 stats) 与 3588(用量记录) 各遍历一次全文 | 每请求 2 次 O(n) 遍历(全文) | 每请求(第二管线) | 复用第一次结果 | 无 |
| 18 | model2_pipeline.rs:1927/1945 (chunk_id format!) | 每 chunk 生成 chunk_id 字符串: `format!("{}_chunk_{}", request_id, counter)`, 前端消费方(TemplateAIEngine)仅用 content/is_complete | 每 chunk 1 次 String 分配+格式化(2000 chunk ≈ 2000 次小分配) | 每 chunk | chunk_id 置空串或删除字段(需 tsc 核对前端类型引用) | 低: StreamChunk 序列化字段变化需前端类型同步 |
| 19 | model2_pipeline.rs:1835-1852+2566-2594 | 每请求 2 条 DebugLogger("start"/"end") 文件日志入队: 每条含 json 构造+LogEntry clone; 队列缓冲成本小但恒定 | 每请求 2 次入队+序列化(刷盘由批量机制承担) | 每请求 | "start" 条信息量低(仅 bytes=0), 可只留 "end" | 低: 调试日志面变化 |

## 最小数据传递方案

该模块的理想形态(功能不变前提下):

1. **配置与工具清单移动一次**: ApiConfig 解析结果、工具 schema 数组进程内缓存(Arc 共享), 仅在设置写入时失效——每请求的 8-15 次 settings 读与 3-5 次全量 JSON 解析降为零(发现#1/#7)。
2. **请求体一次序列化**: 序列化 buffer 同时供测长/审计/发送(发现#2); sanitize 仅在审计开启时执行。
3. **chunk 流恒为增量**: 已达成(12 个 delta emit 点); 补齐——终块不再回显全量, 完成语义用 is_complete 标志表达(发现#4)。
4. **审计数据按需拉取**: 请求体预览改为 invoke 拉取最近 N 条环形缓冲, 而非每请求全量推(发现#3); OCR debug 预览截断+门控(发现#5)。
5. **hook 解析一次/流**: 局部持有 Arc<dyn LLMStreamHooks>, 消除每 chunk 锁(发现#6)。
6. **记账维持现状**: llm_usage 采集器(每请求一条 + 批刷)已是最小形态, 不动。

## 不动清单

| 项 | 原因 |
|----|------|
| SSE/HTTP 底座留在 Rust(不迁前端) | 安全属性: API key 不进渲染进程(任务边界明示) |
| llm_usage 采集器架构(channel 1000 + 后台批刷 + 事务插入) | 已是最小数据传递: 每请求一条、非阻塞、有界背压 |
| llm_usage 7 个命令契约(日期/limit 参数化聚合) | 按需读取、无轮询、参数收敛, 无改造空间 |
| system 消息 `cache_control: ephemeral`(model2_pipeline.rs:851) | OpenAI/DeepSeek prompt caching 命中依赖此结构, 改动破坏缓存命中 |
| 取消双通道(watch channel + registry)语义 | 跨请求边界的取消竞态防护(1793 注释), 压测过的修复路径 |
| 429/5xx 指数退避重试语义(ERR-01) | 重试仅发生在响应头阶段, 不重新缓冲已收内容——现行为正确 |
| IncrementalJsonArrayParser 逐字符流式解析(streaming.rs:22-134) | 已是增量实现, 题目流式解析的正确形态 |
| merge_consecutive_user_messages/assistant_tool_calls 防御性合并 | provider 兼容性要求(Anthropic/ERNIE), 触发频率低 |
| `{se}_web_search/_rag_sources/_memory_sources` 6 个引用事件(每工具结果一次) | 频率=工具调用次数(低), payload 为必要 UI 数据 |
| 两条管线并存(有 hook/无 hook) | 服务不同调用方(chat_v2 主对话 vs 总结/多变量); 合并属架构级重构, 超出数据传递优化范畴, 单列建议 |
