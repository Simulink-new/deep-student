# 可行性调研: 新模型自动适配流水线

> 撰写时间: 2026-09-10 00:04 CST | 分支: main (4bb4d440)
> 需求: 配置至少一个联网搜索 key + 一个模型 key 后 —
> ① 本地不连接大模型(不发起推理)验证两个 key 的连通性;
> ② 两者可用时检测已配置供应商是否发布新模型;
> ③ 检测是否存在新的返回格式/调用方式;
> ④ 调用本地 claude code 等编程工具自动编写适配器并验证数据通路。

## 0. 结论速览

| 阶段 | 可行性 | 成本 | 关键前提 | fork 现状 |
|------|--------|------|----------|-----------|
| ① 双 key 免推理验证 | ✅ 完全可行 | 零 token / 零搜索额度(部分引擎判定"有效"需 1 次调用) | 无 | **已有基建**: `test_api_connection`(消耗 ~10 token)与 `test_all_search_engines` 已存在,改进为免推理探测即可 |
| ② 新模型发布检测 | ✅ 完全可行 | 零 token | 供应商暴露 models 列表端点 | **已有基建**: 设置页拉 `/models` 入库的路径已存在,补 diff+通知 |
| ③ 新格式/调用方式检测 | ⚠️ 两级: 免费级启发式 / 探测级确定性 | 每模型 3 次微调用(≈数百 token,可忽略) | 允许消耗极少量 token(与①的"免推理"约束区分开) | 无,需新建 probe harness + schema 签名库 |
| ④ claude code 自动写适配器 | ⚠️ 可行,限维护者模式 | 用户的 claude 订阅额度 | 开发机有 claude CLI + Rust 工具链(本机已验证: claude 2.1.251 + node v24) | 无,但 MCP stdio transport 提供了现成 spawn 路径 |

**架构红利(改变结论形状的关键事实)**: fork 中"已有供应商下新增模型 ID"是**纯运行时数据**(settings 表 `vendor_configs`/`model_profiles`,零代码)。
即: 新模型发布的 ~90% 情形**根本不需要写适配器**,只需要一条数据;④ 仅在真正格式漂移(新字段/新协议)时触发。需求里的"自动编写适配器"实际是这条流水线的最后一道兜底,而非主路径。

## 1. 现状盘点(挂点)

### 模型调用架构: 双层适配器
- **wire 层**(4 种原生协议): `src-tauri/src/providers/mod.rs:46` `trait ProviderAdapter { build_request/parse_stream }`;实现: OpenAI(:58)/OpenAIResponses(:306)/Anthropic(:815)/Gemini(:1922);路由 `llm_manager/config_types.rs:834` `build_provider_adapter()`
- **整形层**(每供应商请求体差异): `llm_manager/adapters/mod.rs:58` `trait RequestAdapter` + `ADAPTER_REGISTRY`(:192-225,~15 家供应商;多数是 GenericOpenAI + OVERRIDES 常量)
- 新推理字段(如 `reasoning_content`/`thinking_delta`)的消化点集中在 `providers/mod.rs` 各 `parse_stream` 内

### 模型目录: 四处叠加
| 位置 | 形态 | 改动成本 |
|------|------|----------|
| `builtin_vendors.rs:206` BUILTIN_MODELS (~80 条) | Rust 常量 | 需重编译 |
| `scripts/model-capability-registry.json` (2118 行,能力/quirks) | JSON 但 `include_str!` 内嵌 | 数据,需重编译 |
| 用户 vendor/model profiles (settings 表) | 运行时 SQLite | **零代码** |
| 前端 `src/utils/modelCapabilities.ts` + `apiCapabilityEngine.ts` | TS 推断引擎 | 改 TS 不改 Rust |

### 搜索集成与连通性测试(已存在)
- 搜索: `tools/web_search.rs` 7 引擎(google_cse/serpapi/tavily/brave/searxng/zhipu/bocha),全部 Rust 发出,key 加密存 SQLite(`web_search.api_key.*`)
- LLM 测试: `commands.rs:1689` `test_api_connection` — 现状是发真实 `"Hi"`+max_tokens:10 对话,只看 `status.is_success()`,10s 超时
- 搜索测试: `cmd/web_search.rs:17/61` 单引擎/7 引擎一键体检
- 子进程: 无 tauri-plugin-shell;现成 spawn 路径 = `mcp/global.rs:323-378` MCP stdio transport(tokio::process, kill_on_drop, CREATE_NO_WINDOW, env 注入)

## 2. 阶段①: 免推理双 key 验证

### 2.1 模型 key — models 列表端点探测

按协议分 4 路,全部 GET、零推理:

| 协议 | 探测请求 | 语义 |
|------|----------|------|
| OpenAI 兼容(DeepSeek/Qwen/Zhipu/Doubao/Moonshot/MiniMax/xAI/SiliconFlow/...) | `GET {base_url}/models` + Bearer | 401/403=key 无错;200=key 有效 |
| OpenAI Responses | 同上(OpenAI 同时暴露 /v1/models) | 同上 |
| Anthropic 原生 | `GET /v1/models` + `x-api-key`+`anthropic-version` | 官方支持,已核实文档 |
| Gemini 原生 | `GET /v1beta/models?key=` | 免费 |

### 2.2 差分鉴权探测(解决自建网关假 200)

部分自建网关/代理对任何 key 都回 200。补一个零成本差分:
同端点**不带鉴权**再发一次 —
- 带 key 200 + 不带 key 401 → 鉴权语义确认,结论"key 有效"为**确定性**
- 两次都 200 → 网关忽略鉴权,降级结论为"端点连通,鉴权未验证"(如实标注)

### 2.3 搜索 key — 每引擎探测表

| 引擎 | 免费验证端点 | 判定"有效"成本 |
|------|--------------|----------------|
| tavily | `GET /usage`(已核实官方文档) | 0 |
| serpapi | `GET /account` | 0 |
| searxng | 自托管 `{endpoint}/search?format=json` | 0 |
| brave | 无状态端点 | 1 次真实查询(免费额度内) |
| google_cse | 无 | 1 次查询 |
| zhipu / bocha | 无(需运行时核实) | 1 次查询 |

**不对称规律**: 判定 key **无效**(401/403)永远免费、永远确定;判定**有效**部分引擎需消耗 1 次调用。UI 应区分"验证(免费)"与"深度验证(消耗 1 次额度)"两档。

### 2.4 准确性边界(如实声明)

- 401/403 → 无效: **确定**
- 200 + 差分通过 → key 鉴权有效: **确定**
- 但 200 **不能**证明: 该 key 对具体模型有调用权(如 Anthropic 需申请的模型)、账户有余额、未超限流
- 需要"彻底确认"时保留现有 10-token 对话测试作为可选 L2 深测(现状命令直接复用)

**改动量估计**: `test_api_connection` 加 `probe_mode` 参数(免推理/深测两档)+ 搜索侧各引擎探针函数,约 200-300 行 Rust,无新依赖。

## 3. 阶段②: 新模型发布检测

- **数据源**: 对每个已配置 vendor 定期(或手动)拉 models 列表 — **拉取入库路径已存在**(设置页 `/models` → settings 表)
- **diff 基线**: `BUILTIN_MODELS` ∪ capability registry ∪ 用户已有 `model_profiles`;出现新 ID → 记录+通知
- **搜索通道**(这就是"搜索 key 必须可用"的原因): 用联网搜索抓供应商发布公告/changelog,取回文档 URL 与发布说明,供阶段③④消费
- **局限**: 灰度发布时列表可能滞后;列表出现 ≠ 该 key 可调用(需 ③ 的探测级确认)

**改动量估计**: 定时任务/手动巡检命令 + diff + 通知,约 300-400 行,纯 plumbing。

## 4. 阶段③: 新格式/调用方式检测

### 4.1 两级检测

- **免费级(启发式)**: web 搜索供应商 API changelog/docs 变更 → 只能提示"疑似",不能定论
- **探测级(确定性,小额消耗)**: 对新模型发 3 个最小探针:
  1. 非流式 `max_tokens=1` → 捕获完整响应 JSON 结构
  2. 流式 `max_tokens=1` → 捕获 SSE 事件序列
  3. 故意发不支持参数(如对推理模型发 temperature)→ 错误信息常暴露新参数名
- 每模型总消耗 ≈ 数百 token,成本可忽略;但**这步不是零成本**,与阶段①的"免推理"约束是两回事(需求原文只把免推理约束加在连通性验证上)

### 4.2 schema 签名分类(核心算法)

把捕获结构与已知签名字段集比对(`choices[].message` / `content[].type=thinking` / `reasoning_content` / `input_json_delta` / usage 字段集...) → 三类:

| 类别 | 判据 | 处置 | 占比预估 |
|------|------|------|----------|
| **T0 纯新 ID** | 结构与所在协议已知签名一致 | 运行时 model profile + capability registry 数据条目(**零代码**,fork 已支持) | ~90% |
| **T1 新字段/参数** | 出现未知字段(如当年 `reasoning_content` 首发) | `providers/mod.rs` 或 `RequestAdapter` 小改(Rust) → 触发④ | 少数 |
| **T2 新协议** | 事件序列/请求形状整体不同(如 Chat→Responses 级) | 新 `ProviderAdapter`(Rust) → 触发④ | 罕见 |

签名库初始 = 从 4 个 `parse_stream` 实现反推的字段白名单,存 JSON,随巡检更新。

## 5. 阶段④: claude code 自动写适配器

### 5.1 前提(本机已验证)
- `claude` CLI 2.1.251 在 PATH;node v24.15.0
- spawn 复用 `mcp/global.rs:323-378` 的模式(tokio::process + CREATE_NO_WINDOW + 超时 + kill_on_drop),一次性调用而非长驻

### 5.2 调用形态

```
claude -p "<结构化任务包>" \
  --output-format json \
  --allowedTools "Read,Grep,Glob,Edit,Bash(cargo:*) tauri" \
  --max-turns <N>          # 工作目录 = deep-student 源码仓
```

任务包内容: ①探测捕获的**结构化 manifest**(见 5.3) + ②目标挂点文件清单(§1 盘点) + ③web 搜索取回的供应商文档摘录。输出 = git 新分支上的补丁,**不直接进主干**。

### 5.3 安全门控(必须)

1. **注入面**: 供应商响应是不可信输入,直接喂给 codegen 有 prompt injection 风险 → 探测样本先过 schema 提取器,只传结构化字段名/类型/事件序列清单,不传自由文本
2. **权限收紧**: claude 工具白名单限定 Read/Grep/Glob/Edit + 限定命令前缀(cargo check/rustfmt),禁网络
3. **人审闸门**: 补丁全绿后提 PR 式审阅,人不点头不合入
4. **成本透明**: UI 明示将消耗本地 claude 订阅额度

### 5.4 验证数据通路(需求原文"确保可用")

生成适配器后自动跑 probe harness,且**走新生成的适配器本身**:
`models 列表 → 1-token 非流式 → 1-token 流式 → 工具调用往返 → usage 解析 → (T1 场景) reasoning 字段往返`
全绿 → 进入人审;任一红 → 差异样本回喂 claude 重试(限 2 轮)。

### 5.5 编译边界(最重要的限制)

fork 适配器是**编译进二进制的 Rust**:
- **维护者/开发机**(即本机场景): 完全可行 — codegen → cargo check → probe → 人审 → 合并重编译
- **终端用户安装包**: 自动生成的 Rust 适配器**无意义**(无工具链、无法重编译、签名破坏) — 终端用户永远走 T0 数据路径(运行时 model profile),这正是 fork 架构已支持的
- 结论: ④定位为**维护者模式工作流加速器**,不是端用户功能;这也符合需求原文"调用本地的 claude code 等编程工具"的语义

## 6. 风险清单

| 风险 | 缓解 |
|------|------|
| 供应商无 /models 端点或格式不统一 | 按协议 4 路分发已覆盖内置全家;自建 vendor 走 OpenAI 兼容假设 + 差分探测降级标注 |
| 灰度: 列表出现但无权调用 | ③探测级确认后才提示"可用";仅列表级提示标"未验证" |
| codegen 产出编译不过 | harness 含 cargo check;失败重试限 2 轮后人审 |
| claude CLI 缺失/未登录 | 启动时探测 `claude --version`,缺失则④入口置灰,①②③不受影响 |
| 搜索额度被巡检消耗 | 巡检默认用免费端点;消耗型查询仅手动触发 |
| 上游供应商响应注入 manifest | 5.3-1 schema 提取器白名单 |

## 7. 落地路线

- **M1(纯 HTTP,零风险)**: 阶段①免推理探测(改造 `test_api_connection` + 搜索探针表) + 阶段② models 巡检 diff 通知。改动 ~500 行 Rust + 少量设置页 UI
- **M2(小额消耗)**: probe harness + schema 签名库 + T0/T1/T2 分类;T0 自动落数据条目
- **M3(维护者模式)**: claude code 集成(manifest 提取 → codegen → harness 验证 → 人审);UI 放开发者设置区
- M1/M2 是端用户可用功能;M3 仅开发机构建可见

## 8. 总结论

全链路**可行**,且比需求原文设想的更省: fork 的运行时模型档案架构使"新模型"多数不需要适配器,④是兜底而非主路径。
真正要新建的只有三块: 免推理探针层(小)、schema 签名分类器(中)、codegen 编排+验证回路(中,限维护者)。
阶段①的"准确无误"需要按 2.4 如实分层 — 免费探测能 100% 判定 key 无效与鉴权有效,不能判定配额/模型权限,后者留给可选深测。
