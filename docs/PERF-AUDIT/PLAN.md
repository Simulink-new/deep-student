# 全项目数据传递审计与优化方案 (PERF-AUDIT)

> 撰写: 2026-09-11 00:10 CST | 基线 commit: ae1e6385 | 分支: main
> 战役: harness campaign-2 (task-005..task-023 + 后续优化任务)
> 前置: docs/execution-tier-audit-2026-09-10.md(执行层审计,已产出 4 个已修 P2 + 一批未修热点)

## 1. 目标与约束

**目标**（用户指令原文要点）:
1. 审计全部模块的数据传递**计算开销、时间开销、空间开销**
2. 探索"完整功能保留"前提下的**最少计算与最少数据传递**实现方式
3. 按此优化整个项目

**硬约束**:
- 全部功能保留; **前端可观察行为（展示功能）不变**
- 后端数据处理模式**可以改变**（含命令签名/payload 形状,前端调用侧同步适配）
- `local_api` v1.1 的 11 个 stable 端点契约**冻结**（四方会话依赖,见 docs/STUDY-DATA-API.md）
- 数据库已有数据兼容（schema 变更必须带迁移）
- 每个优化独立 commit、可独立回滚; 工作树由四方会话共享,**只 add 自己改的文件**

## 2. 项目规模基线 (2026-09-11 实测)

| 维度 | 数值 |
|------|------|
| Rust 模块 | 26 个目录 + 根级文件, 共 ~36 万行 |
| 前端区域 | 28 个目录, 共 ~41 万行 (features 224.7K/755 文件) |
| IPC 命令 | 758 个 `#[tauri::command]` |
| Rust emit 点 | 225 处 |
| 前端 invoke 点 | 904 处 |
| 前端 listen 点 | 102 处 |
| SQL 执行点 | 2611 处 (vfs 1105 + database 455 + data_gov 330 + 根级 238 + chat_v2 251 + …) |
| serde 序列化点 | 538 处 (chat_v2 182 独大) |
| Rust .clone() | 5050 处 |
| TS JSON.parse/stringify | 570 处 |

命令分布: 根级 195 / cmd 169 / vfs 129 / chat_v2 80 / dstu 54 / data_gov 45 / memory 36 / essay 20 / cloud 14 / llm_usage 7 / translation 4 / local_api 3 / qbank 2

**已发现的横切问题**: 命令名重复(invoke 按名解析,同名二义): `get_mcp_config`/`import_mcp_config`/`export_mcp_config`/`test_mcp_*` 系列 ×2。

## 3. 审计模型与七项 rubric

数据传递三层模型: **产生**(SQL/文件/LLM/OCR) → **移动**(IPC/事件/网络/磁盘) → **消费**(渲染/存储/再计算)。
审计即沿此链路找"移动了不该移动的数据、算了不该重复算的东西"。

每模块审计逐项过:

1. **IPC 边界**: command 返回全量 vs 分页/投影; 大 payload(Vec\<u8\>/base64/全表)过 JSON 序列化; 同帧重复 invoke; 可合并的批量调用
2. **事件流**: emit 频率 × payload 大小; O(n²) 累积(每 chunk 带全量 accumulated); 高频事件无合批/节流; 孤儿事件(发无人听)/重复监听(一事件多订阅重复处理)
3. **SQL**: N+1(循环内单行查询); 全表 SELECT 后内存过滤; WHERE/ORDER BY 列缺索引; 循环内逐行 execute 无事务; 重复查询同一数据
4. **序列化/拷贝**: 深拷贝(clone 链/JSON roundtrip); `serde_json::Value` 中转(强类型直达); 每请求重算可缓存数据; String 反复拼接 O(n²)
5. **内存**: 无界增长(历史快照/日志缓冲/事件队列); 全量缓冲 vs 流式(PDF/OCR/LLM 响应); 大 Vec 复制传值
6. **阻塞与并发**: 同步 IO/锁跨 await; 缺 spawn_blocking; 主线程重活; 锁粒度过粗
7. **前端特有**: store 订阅粒度(全 store 重渲染); useEffect 依赖致重复 fetch; 轮询间隔; JSON.parse 深拷贝 state; 列表无 key/全量重渲染

**量化口径**: 每条发现给 复杂度(O) + 字节/次数估计(基于代码结构合理假设,**注明假设**) + 触发频率(每 chunk/每次打开/启动一次) + 修复草图 + 破坏风险。置信度不足标"待验证"。

## 4. 批次划分 (harness 任务)

| 任务 | 范围 | 产出 |
|------|------|------|
| task-005 | 方案+全量索引 | PLAN.md + 00-inventory.md + idx-*.txt |
| task-006 | chat_v2 pipeline/events/prompt/persistence | A1 |
| task-007 | chat_v2 handlers+tools | A2 |
| task-008 | vfs repos+database+lance_vector_store | A3 |
| task-009 | vfs handlers+index+pdf_processing | A4 |
| task-010 | data_governance 全模块 | A5 |
| task-011 | llm_manager+llm_usage | A6 |
| task-012 | dstu handlers+local_api | A7 |
| task-013 | memory+essay_grading+qbank_grading | A8 |
| task-014 | multimodal+ocr+translation+providers+adapters | A9 |
| task-015 | 根级服务+cmd/+mcp+cloud+crypto+research | A10 |
| task-016 | 事件层专项 emit↔listen 配对 | A11 |
| task-017 | 前端 stores+hooks+contexts | B1 |
| task-018 | 前端 api+dstu 适配+services+types | B2 |
| task-019 | features/chat | B3 |
| task-020 | features/learning-hub+mindmap | B4 |
| task-021 | features/notes+todo+pomodoro+pdf+settings | B5 |
| task-022 | components+debug-panel+杂项 | B6 |
| task-023 | 汇总+优化任务生成 | REPORT.md |

## 5. 已知热点核实状态 (2026-09-11 00:10 实测, 承接执行层审计)

| 热点 | 昨日定位 | 今日核实 |
|------|---------|---------|
| essay/qbank O(n²) accumulated 全量 emit | essay pipeline.rs:101 等 | ❌ **未修**: essay_grading/pipeline.rs:107/128/187/211, qbank_grading/events.rs:61/77 + pipeline.rs:181/189 仍 `.clone()` 全量 |
| 流式 chunk 合批 | chat_v2/events.rs:829/926 | ✅ 机制已存在(`flush_session_chunk_events`/`drain_session_chunks` 带序号)——**覆盖面待 A1/A6 核实**(是否所有 emit 路径走队列) |
| streamingBlockSaver 5s 全量回声 | chat/core/autoSave.ts:368 | ✅ 疑已消失(原路径无 grep 命中)——A1/B3 确认删除还是漂移 |
| 附件三段往返(Vec\<u8\>→JSON→base64) | commands.rs:2221 | ⚠️ `read_file_bytes` 仍在;消费链(vfsRefApi)待 A10/B2 核实是否仍三段 |
| "今日到期" UTC 时区 bug(两侧) | reviewPlanStore.ts:724 / review_plan_service.rs:203 | ⚠️ 路径漂移无命中——B4 重定位(可能已修或移走) |
| finderStore dstu_get N+1 (~100次) | finderStore.ts:562/673 | ⚠️ 路径漂移无命中——B3 重定位 |
| ApiConfig 全量拉取无视前端缓存 | TauriAdapter.ts:3402 | ⚠️ 待 B2/B5 核实 |
| Crepe 每 250ms 全文序列化 | CrepeEditor.tsx:1128 | ⚠️ 待 B5 核实 |
| 图片逐张 base64 invoke | QuestionBankEditor.tsx:715 | ⚠️ 待 B5/A10 核实 |
| aws-sdk-s3 依赖树 | cloud_storage/s3.rs | ✅ 已修(e7dcf6f4 手写 SigV4 + 9b6c81df 修正) |
| image/hyper 特性 | Cargo.toml | ✅ 已修(50e32d04) |
| 导图拖拽/历史快照 | MindMapCanvas/mindmapStore | ✅ 已修(ebdd7b93/ae1e6385, 昨日 harness) |

## 6. 产出物规范

每份审计文档统一格式:

```markdown
# <编号> <模块> 数据传递审计
## 范围与方法   (路径清单 + 采样策略: 全量/热点优先)
## 数据流摘要   (谁→经过什么→到谁, 1-3 段)
## 发现清单     (影响高→低排序)
| # | 位置(file:line) | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
## 最小数据传递方案  (该模块理想形态: 哪些数据只需移动一次/按需/增量)
## 不动清单     (不能改的 + 原因)
```

## 7. 优化实施原则与验证

- 实施顺序: 影响高 + 风险低 先行; 纯后端内部改动(emit 模式/SQL/缓存)最安全, 跨边界改 payload 需前端同步适配并过 tsc
- 验证: Rust → `cargo check`; TS → `npx tsc --noEmit -p tsconfig.json`; 行为不变性 → 结构断言(消费端对齐检查) + 审计文档记录
- 每优化任务一个 harness task, 验证命令必填, 失败即回滚(只回滚本任务文件)

## 8. 不动清单 (全局)

- local_api v1.1 stable 端点的请求/响应形状
- 前端 UI 行为/视觉/交互流程
- 既有 SQLite 数据(迁移可以有, 数据不能丢)
- 四方会话共享工作树中他人未提交的脏文件(不 add 不回滚)
- AnkiConnect Rust 转发层(task-004 已裁决保留)
