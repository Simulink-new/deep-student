# 运行日志体系审计与完善报告

> 日期： 2026-09-16 ｜ 任务： task-047 ｜ 基线： 25bdbebd ｜ 落地： 见文末「本次实装」
> 触发： 用户提问「运行日志记录内容、格式和位置；是否支持运行中通过日志定位问题；对照成熟商业软件补充完善」

---

## 1. 现状盘点（审计前）

### 1.1 五套并存的日志设施

| # | 设施 | 写入位置 | 内容 | 格式 | 轮转 |
|---|------|---------|------|------|------|
| 1 | tauri-plugin-log（主日志） | `%LOCALAPPDATA%\com.lanxia.deepstudent\logs\deep-student.log` | 全部 `log::`/`tracing::` 宏输出（159+114 个文件） | `[日期][时间][模块路径][级别] 消息` | **无轮转配置——每次启动清空重来** |
| 2 | crash_logger（panic hook） | `%APPDATA%\com.lanxia.deepstudent\logs\crash\crash-*.log` | panic 位置/消息/回溯 + 版本构建号 | 自定义文本 | 保留 20 个 |
| 3 | debug_logger（结构化后端） | `%APPDATA%\...\logs\{backend,frontend,debug}\<日期>_<模块>.log` | LogEntry JSON 行（级别/模块/操作/data/context） | JSONL | 7 天 + 单文件 10MB |
| 4 | debug_log_service（LLM 请求体） | `%APPDATA%\...\debug-logs\*.json` | LLM 请求体 dump（三级脱敏 Full/Standard/Compact） | 单条 JSON 文件 | 500 个淘汰 10% |
| 5 | 前端 debugLogger.ts | 经 IPC `write_debug_logs` → 设施 3 的 frontend/ | 前端结构化日志（风暴去重/限流/熔断） | LogEntry JSONL | 同设施 3 |

另有：Sentry（可选，`SENTRY_DSN` 环境变量驱动）、ANR 看门狗（3s 心跳检测后端阻塞）、MetricsServer（Prometheus 指标，127.0.0.1 随机端口）。

### 1.2 审计发现的缺陷（磁盘实证）

1. **目录分裂**：主日志在 `%LOCALAPPDATA%`，崩溃/前端/结构化日志在 `%APPDATA%`——没有任何文档记载这一点，排查时要猜两个蜂巢。
2. **主日志无轮转**：每次启动截断重来。昨天的崩溃，今天的日志里已经找不到昨天。
3. **日志级别编译期焊死 Info**：用户现场出 bug 时无法提级到 debug，只能发版。
4. **Webview 镜像常开**：生产环境每条日志都过一遍 IPC 送进 Webview 控制台，持续开销。
5. **崩溃文件信息贫乏**：527 字节 = 位置 + 无符号回溯。不附主日志尾部、不附系统信息；本地文件不脱敏（Sentry 上报反而脱敏——写反了）。
6. **panic 不进主日志**：主日志在 04:41:16 戛然而止，崩溃原因只在 crash 目录。
7. **DebugLogger 队列内存泄漏**：只在 ERROR 时 flush；`log_llm_usage` 等 INFO 调用永久堆积内存，`logs\backend\` 恒为空（实证：目录存在但无文件）。`debug_log!` 宏零调用。
8. **无启动横幅**：主日志无版本/构建号/git hash——两份会话首尾相接时分不出边界。
9. **无诊断打包能力**：用户报障要手动翻两个蜂巢、四个子目录。
10. **API key 审计**：未发现明文 key 落日志（仅有记录密钥**长度**的两处，可接受）。✅

### 1.3 「能否运行中靠日志定位问题」审计结论

**部分支持**：日常 INFO 级主日志能定位「哪个子系统挂了」级别的问题（本次 task-046 闪退正是靠崩溃日志精确定位到 model_watch.rs:480）。但对「偶发/用户现场」问题力不从心：级别不可调（缺陷 3）、历史会话丢失（缺陷 2）、崩溃无上下文（缺陷 5/6）、前后端日志分裂（缺陷 1）。

## 2. 商业软件对照调研

| 能力 | VS Code | JetBrains IDE | 本应用（审计前） |
|------|---------|---------------|-----------------|
| 日志级别运行时调整 | `Developer: Set Log Level` 按通道即时生效；CLI `--log ext:debug` | `Debug Log Settings` 按类别即时生效（无需重启） | ❌ 编译期焊死 |
| 日志轮转 | 按会话保留多代 | idea.log.1/.2… 滚动 | ❌ 每次启动清空 |
| 崩溃上下文 | 崩溃转储 + `--verbose` 日志 | 线程转储自动捕获 + 日志附带 | ❌ 527B 无上下文 |
| 一键诊断打包 | 内置问题报告器 | `Collect Logs and Diagnostic Data` → zip | ❌ 无 |
| 打开日志目录入口 | `Developer: Open Logs Folder` | `Help → Show Log in Explorer` | ❌ 无 UI 入口 |
| 日志落盘位置 | 单一日录树 | 单一日录树 | ❌ 分裂两个蜂巢 |
| 敏感信息 | 用户自行清理 | 官方提示 scrub | ⚠️ 仅 Sentry 路径脱敏 |
| 风暴防护 | 输出通道天然缓冲 | 日志框架 | ✅ 前端已有（去重/限流/熔断） |

## 3. 本次实装（commit 见 harness 记录）

### 后端
1. **统一日志根目录**：`crash_logger`/`debug_logger`（frontend/backend/debug 三族）全部迁到 tauri `app_log_dir`（Windows = `%LOCALAPPDATA%\com.lanxia.deepstudent\logs\`），与主日志同树。LLM debug-logs 属按需开发者设施，维持原位不动（决策记录于此）。
2. **轮转**：`RotationStrategy::KeepSome(10)` + 单文件 10MB 上限——保留最近 10 个历史日志。
3. **本地时区**：`TimezoneStrategy::UseLocal`——读日志不再心算 UTC。
4. **logging.json 运行时配置**（新模块 `logging_config.rs`）：`{level, webviewMirror}`，Builder 装配前同步读取，设置页可改，**重启生效**（tauri-plugin-log 无运行时热改 API；JetBrains 式即时生效需换 tracing-subscriber reload 底座，列为后续候选）。
5. **Webview 镜像默认关**（生产）：仅在 `webviewMirror=true` 或 dev build 时开启。
6. **启动横幅**：每次启动一条 `========== vX (Build Y, git Z) 启动 | os=… pid=… | 日志目录: … ==========`。
7. **崩溃日志增强**：崩溃文件附带**主日志尾部 ~64KB**（本次闪退若带此功能，crash 文件本身就含完整启动序列）；本地文件与 Sentry 同标准脱敏；panic 同时写入主日志（双向关联）；补充 os/arch 字段。
8. **DebugLogger 修复**：队列有界（500，溢出丢最旧）、≥20 条即 flush、`debug_log!` 死宏删除。logs/backend 从此有真实内容。
9. **新命令**：`logging_get_prefs` / `logging_set_prefs` / `open_log_dir` / `export_diagnostics_bundle`（zip 到下载目录：system.txt + main/ 主日志归档×5 + crash/×20 + frontend/×10 + backend/×10 + manifest.txt，全部用户名脱敏 + 单文件 2MB 截尾 + 总量 40MB 上限，压缩走 spawn_blocking）。

### 前端
10. **设置 → 关于 → 日志与诊断**（新 `DiagnosticsSection.tsx`）：级别下拉、镜像开关、「打开日志目录」、「导出诊断包」（导出后自动在文件管理器中选中 zip）。独立 `diagnostics` i18n 命名空间（glob 自动注册，避开在途脏文件）。

### 验证
- `cargo check` exit 0（无新增警告）
- `tsc --noEmit` 零输出

## 4. 遗留候选（本次未做）

- **JetBrains 式级别热更新/分类别级别**：需以 tracing-subscriber reload::Handle 替换 tauri-plugin-log 的固定过滤器，牵动面大，单列任务评估。
- **崩溃日志符号化**：release 构建保留符号表（pdb 分离上传 Sentry），当前回溯帧为地址级。
- **ANR 看门狗联动**：卡顿时自动落线程转储（对标 JetBrains threadDumps-*）。
- **日志内查看器**：应用内直接 tail 主日志（当前需打开目录用外部编辑器）。

## 5. 给后续维护者的速查

| 要找什么 | 去哪里 |
|---------|--------|
| 后端运行日志（每次启动的各子系统初始化、运行期 INFO+） | `%LOCALAPPDATA%\com.lanxia.deepstudent\logs\deep-student.log`（+ 轮转归档） |
| 崩溃报告（panic 位置/回溯/**主日志尾部现场**） | 同上 `logs\crash\crash-*.log` |
| 前端未捕获异常/Promise rejection | 同上 `logs\frontend\<日期>_*.log` |
| 后端结构化 JSONL（LLM 用量等） | 同上 `logs\backend\<日期>_*.log` |
| LLM 请求体 dump（默认关闭，debug.persist_logs 开启） | `%APPDATA%\com.lanxia.deepstudent\debug-logs\*.json` |
| 级别/镜像配置 | `%APPDATA%\com.lanxia.deepstudent\logging.json`（或设置页） |
