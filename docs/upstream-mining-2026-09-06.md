# 上游采矿报告 — v0.9.40..upstream/main

> 生成时间: 2026-09-06 23:24 CST | §3 提取记录填写: 2026-09-06 23:55 CST
> 上游 tip: 1308fb6b6 (v0.9.56, 2026-09-06) | fork 基线: v0.9.40-fork.1
> 范围: 1386 个非合并提交 (1454 含合并)
> 提取分支: `mining/2026-09-06-fixes` (21 commits over main)

## 1. 分类统计

| 类别 | 数量 | 说明 |
|------|------|------|
| feat | 513 | 功能(审阅清单见 §4) |
| fix | 348 | 修复(适用性评估见 §2) |
| refactor | 167 | 重构(不单独采,随 fix/feat 带入) |
| docs/chore/style/ci/test | ~290 | 噪声,跳过 |
| perf | 4 | 性能修复,已并入 fix 评估 |

上游存在大量**同标题双提交**(nightly 分支与 main 各落一次),提取时已按标题去重。

## 2. fix/perf 适用性评估 (348 + 4)

| 判定 | 数量 | 含义 |
|------|------|------|
| CLEAN | 126 | 触碰文件 fork 全有且无工作区冲突 |
| NO_CODE | 89 | fork 没有该代码(shell 沙箱/headless/FTP 等上游新增模块),不适用 |
| PARTIAL | 70 | 部分文件缺失(多为新测试文件,可采) |
| CONFLICT | 63 | 触碰 fork 工作区已改文件或三大警示文件 |

**结构性发现**: fork 的 P0-P3 重构使 `vfs/handlers.rs`→`handlers/` 目录、`chat_v2/tools/` 无 local_shell 系列,上游涉及这些旧结构的修复需手工移植;`src/chat-v2/`(旧路径)的提交均为改名前 nightly 支线,内容已被改名合并吸收,跳过。

### 2.1 抽查验证(防重复提取)

| 修复 | fork 现状 | 结论 |
|------|-----------|------|
| 4018a01f8 memory SQL LIKE 转义 | 已有 `ESCAPE '\'` 修正版 | ✅ 已在 fork,跳过 |
| d2f442488 Android RECORD_AUDIO | manifest 已有该权限 | ✅ 已在 fork,跳过 |
| dd46b149b 审批 1000ms→0 | fork 仍为 1000ms | ⬇ 待提取 |

### 2.2 Tier-1 提取清单 (33 个,已去重、剔除 CI/纯样式)

**后端/数据完整性 (10)**
| commit | 标题 |
|--------|------|
| b2a85a690 | backfill missing VFS tables before change_log pre-repair |
| f174231a8 | recover chat_v2 schema fingerprint drift |
| c0fd61ea2 | preserve cloud sync conflicts before strategy filtering |
| 1a3ed9962 | sync: restore baseline without creating local drift |
| 1bd83dda3 | webdav: share provider request limiter across sessions |
| ba3de709a | sync: provider-aware WebDAV request limiter |
| 8892af59a | chat: enforce snapshot import size limit |
| 88a48fb67 | settings: rollback partial batch writes |
| 615419aa2 | rust: resolve executor and helper integration issues |
| 9b685b260 | pdf: expose safe attachment path check |

**对话前端 (10)**
| commit | 标题 |
|--------|------|
| dd46b149b | 审批已决态即点即出队 (1000ms→0) |
| 74255e978 | tolerate legacy stores without goal fetcher |
| 3eb31e19a | validate record identifiers during staged restore |
| 34d721628 | tolerate partial staged restore payloads |
| 6c1903ccc | dedupe overlapping sessions in sidebar feed |
| ee2cd28b1 | chat-markdown: restore spacing between streamed blocks |
| 0a5e8dba8 | preview: 沙箱预览自动高度只涨不缩的棘轮 |
| caa756f2d | keep translation popover within viewport |
| 5e7748029 | mindmap: clamp blank action popup to viewport |
| e20c25e3d | perf(timeline): memoize paragraph split + thinking 限高 |

**移动端/无障碍 (9)**
| commit | 标题 |
|--------|------|
| 51d738531 | 移动端设置抽屉返回/关闭按钮被拖拽手势误吞 |
| c9c1acc06 | 触控目标 44px 契约 rem 锚点缩水 |
| ccd6f4377 | 闪卡复习按钮移动端隐藏(死路动作) |
| 3d2bb2a6d | 移动端欢迎空态不再显示 Ctrl/⌘+N 提示 |
| 5a5d460ca | 技能卡片网格移动端横向溢出 21px |
| 9d3e685bd | 题库答题祝贺弹窗移动端错位 (#51) |
| dd354efd8 | anki/skills 移动端页头导航统一 (#110) |
| cd255cc04 | a11y: 会话列表项 role=button/键盘激活 |
| 797d6cd92 | 总览图表无数据时渲染空状态 |

**启动/MCP (4)**
| commit | 标题 |
|--------|------|
| b3d165282 | startup: recovery preflight timeout 15s→120s |
| 16efa1037 | mcp: stdio 全链路四断点(ping schema/重连不刷新/空缓存TTL/启动竞态) |
| e0e8b58f8 | mcp: propose connection tests against strict servers |
| 3185361c3 | mcp: Rust 侧协议类型补 camelCase serde rename |

### 2.3 高价值但需手工合并 (暂不进 Tier-1)

| commit | 标题 | 卡点 |
|--------|------|------|
| dbfc7d0de | **审批栏卡死**——approval_expired 反复弹通知 | 缺 3 个新测试文件,核心文件全在 |
| 3e3a47087 | 晚到/重放块乱序沉底——按时间戳稳定归位 | 缺 2 个新测试文件 |
| 712209310 | 移动端手势 touchcancel 卡死+滑动误触豁免 | 新增 useSwipeGesture.ts |
| 481c6efe2 | 空模型响应自动重试 | 仅缺测试文件 |
| 7691dc8cd | 粘贴笔记图片路径规范化 | 仅缺测试文件 |
| 1d2302e59 | 隔离过期流与损坏历史记录 | 缺 2 个后端 handler(fork 已重构) |
| 4952286d6 | compaction 健壮性(失败冷却+瞬态重试+token 估算) | fork compaction 为单文件,上游已拆目录 |
| 1a1661db6 | MCP stdio spawn 加固+发送时自愈注入 | 触碰 TauriAdapter.ts(警示文件) |
| 91d538fb6 | 移动端输入框防缩放/横屏安全区 | 触碰 InputBarUI.tsx(工作区已改) |
| e9803c0a2 | 移动端视图层切换动画方向镜像 | 触碰 App.tsx(工作区已改) |
| 9af68f48b | 安卓虚拟 URI 导入导出 | 触碰 textbooks.rs(工作区已改) |
| 4ccd2c121 | setup 完成闸门修复启动预检误报 blocked | 缺 startup_gate.rs,触碰 lib.rs |
| aa14a5e2b | 评审缺陷清扫:anki 幂等/同步收敛 | 多文件缺失+lib.rs |
| 8ed21167d | 后端缺陷批:加密轮换/迁移守卫/路径 | 缺 resource_repo.rs,触碰 lib.rs+pdf_processing_service.rs |
| 4ab48384e | 白屏——循环 vendor chunk 与 barrel import | 触碰 TauriAdapter.ts+tauri.conf.json |
| a064ac689 | perf: 窗口拖拽卡顿的样式失效热点 | 触碰 lib.rs+App.tsx+InputBarUI.tsx |

### 2.4 明确跳过

- **CI/发布修复 (~40)**: 上游 release-please/migration-gate 专用,fork 已断开上游更新通道
- **settings 灰卡样式系列 (~15)**: 属上游设置页重设计的一部分,纯样式,fork 有自己的设置页演进
- **src/chat-v2/ 旧路径提交**: 改名前支线,内容已吸收
- **89 个 NO_CODE**: shell 沙箱/headless runner/FTP/自动化调度等上游新模块,fork 无此代码

## 3. 提取记录

> 填写时间: 2026-09-06 23:55 CST | 分支: `mining/2026-09-06-fixes` (21 commits over main)
> Tier-1 全量 33 个修复提交已处理完毕: **10 cherry-pick + 11 手工移植 = 21 落地**;2 空(fork 已有)/ 6 NO_CODE / 2 净零 / 2 已存在 = 12 跳过。
> 手工移植统一不采上游 `#[cfg(test)]` 测试模块(本机不编译验证,盲采测试代码风险自负);所需测试已在各提交信息中注明"上游有对应测试可补"。

### 3.1 cherry-pick 直接应用 (10,带 -x 溯源行)

| fork commit | 上游 commit | 标题 |
|-------------|-------------|------|
| ce4e6cb62 | dd354efd8 | fix(anki,skills): unify mobile page header navigation config (#110) |
| 9c63d70dd | 5e7748029 | fix(mindmap): clamp blank action popup to viewport |
| 2ce9a9772 | ee2cd28b1 | fix(chat-markdown): restore spacing between streamed blocks |
| 823ab01fd | b2a85a690 | fix: backfill missing VFS tables before change_log pre-repair |
| 09aca2800 | 797d6cd92 | fix(overview): 总览图表区无数据时渲染空状态,不再留白 |
| e9c94bbe3 | 0a5e8dba8 | fix(preview): 沙箱预览自动高度只涨不缩的棘轮 |
| d44b03319 | 3185361c3 | fix(mcp): Rust 侧 MCP 协议类型补 camelCase serde rename |
| 8e4a5c3cd | 16efa1037 | fix(mcp): MCP stdio 全链路四个断点(ping schema/重连刷新/空缓存TTL/启动竞态) |
| 8ade06ad1 | 8892af59a | fix(chat): enforce snapshot import size limit |
| 260fa5967 | 74255e978 | fix(chat): tolerate legacy stores without goal fetcher |

### 3.2 手工移植 (11,带 Upstream-Commit trailer)

| fork commit | 上游 commit | 标题 | 移植方式 |
|-------------|-------------|------|----------|
| 01185138a | 5a5d460ca | fix(skills): 技能卡片网格补 grid-cols-1(移动端溢出 21px) | 适配(锚点类名差异) |
| 0c8e2c8ac | dd46b149b | fix(chat): 审批已决态即点即出队(1000ms→0) | 部分(仅核心常量,fork 该文件已分叉) |
| 6806f422d | cd255cc04 | fix(a11y): P3-11 会话列表项 role=button/tabIndex/键盘激活 | 适配(keydown 走 currentTarget.click) |
| 6f0fd7e3c | e20c25e3d | perf(chat/timeline): thinking 正文限高 min(60vh,320px) | 部分(memo 半量 fork 已有,仅补限高) |
| a9d82af0d | 615419aa2 | fix(rust): canvas 语义搜索 snippet None→null | 部分(3 hunk 取 1,其余 fork 无对应代码) |
| b1af587ab | 1bd83dda3 | fix(webdav): 同 provider 限流滑窗跨 storage 实例共享 | 适配(ba3de709a 限流本体 fork 已有等价) |
| 7e7079ace | c9c1acc06 | fix(voice-input): 语音按钮触屏 44px 物理最小高 | 全量 |
| 7df2e212f | caa756f2d | fix(chat): 翻译弹窗实时测量钳制视口内 | 全量(定位逻辑整体重写) |
| b8c3a8f83 | c0fd61ea2 | fix(sync): 云同步冲突在策略过滤前落表保护 | 全量(保留 fork 的 total_skipped 计数);上游 ~185 行测试未采 |
| c26283cc9 | e0e8b58f8 | fix(mcp): 严格服务器连测四点修复(loopback 绕代理/SSE 停止信号/initialize 错误细节/complete_request 告警) | 适配(fork 无 browser 模块,loopback 判断内联;SSE 为自研 eventsource-stream,3 处退避点全部接停止信号);上游测试未采 |
| 42d8e1418 | f174231a8 | fix(migration): 指纹校验加"历史比二进制新"护栏 | 部分(仅 13 行通用护栏;~330 行已知指纹特判函数 NO_CODE——fork 无 20260711 迁移,投毒场景不存在) |

### 3.3 跳过 (12)

| 上游 commit | 状态 | 原因 |
|-------------|------|------|
| 9d3e685bd | already-in-fork | cherry-pick 为空——题库祝贺弹窗移动端定位,fork 已有等价修复 |
| 9b685b260 | already-in-fork | cherry-pick 为空——pdf safe attachment path check,fork 已有 |
| 3d2bb2a6d | NO_CODE | fork 无目标代码 |
| b3d165282 | NO_CODE | fork 无目标代码 |
| 6c1903ccc | NO_CODE | fork 无目标代码 |
| ccd6f4377 | NO_CODE | fork 无目标代码 |
| 88a48fb67 | NO_CODE | fork 无目标代码 |
| 51d738531 | NO_CODE | fork 无目标代码 |
| 34d721628 + 3eb31e19a | net-zero | 引入即回滚的一对提交,净变更为零 |
| 1a3ed9962 | already-present | fork 已有等价实现 |
| ba3de709a | already-present | provider 感知限流本体 fork 已有等价(见 b1af587ab 提交说明) |

### 3.4 验证状态

- 所有触碰的 Rust 文件通过 `rustfmt --edition 2021` 解析级校验(本机不编译,遵守 no-build 约定)
- `TranslationPopover.tsx` 通过 `tsc --noEmit` 无该文件相关错误
- 待用户验证: 合并到 main 后统一 `cargo check` + `npm run build`

## 4. 功能更新审阅清单 (513 feat → 按主题)

### 建议移植候选(实用、与 fork 方向一致)

| 主题 | 代表提交 | 规模 | 备注 |
|------|----------|------|------|
| 流式期间消息排队 | input-bar×8 + chat-store×8 + chat-queue×6 | 22 提交 | 打字不中断流式,排队发送 |
| 会话快照导入导出 | 9ffbfcead/9c25d7e64/91c9222a0/3ee514d01 | 4 | 跨设备迁移对话 |
| 会话内消息搜索 | f5d70918d | 1 | 带命中导航 |
| 历史消息向上懒加载 UI | dadb7edd6 | 1 | 顶部横幅/自动触发 |
| goal mode 跨轮自动续跑 | 5edffa1a6 + a6bca190c | 2 | 长程任务 |
| 会话置顶/分组/侧栏操作 | 9cec34fbc 等 (chat-v2) | ~8 | |
| title_locked 防自动摘要覆盖 | 91ecc64e8 | 1 | 小而美 |
| todo 回收站 | 850c2f39d | 1 | 恢复/彻底清除 |
| 番茄钟每日目标+沉浸专注 | c5780edef/29e8825fb/a825ebc62 | 3 | |
| 记忆 Hermes 策略+蒸馏回溯 | e0cd8bf78/a0906b75b | 2 | 画像溢出自合并+安全扫描 |
| LLM routing/failover 层 | 53a22a311 | 1 | 供应商故障转移 |
| DeepSeek 运行时推理控制 | e7df37985 | 1 | |
| 小米 MiMo provider | 17a70f9bb | 1 | |
| 模型能力注册表 | 7f59fe599 | 1 | 自动推断 vision/tools/reasoning |
| 语音输入 ASR 模块 | voice-input×4 | 4 | 模型分配支持 |
| 翻译弹窗重写(NDJSON 流式+LRU) | c72f97b37 | 1 | |
| mindmap 布局引擎/大纲分屏/多选 | f21319fee 等 | 5 | |
| DOCX VLM 直提取+原生导入 | 67d3fdb49/8a24d1598 | 2 | 含检查点恢复 |
| workspace_file_edit 局部编辑工具 | 8fcdf05c7 | 1 | coding 能力关键补丁 |
| FTP/FTPS 云存储 | 8169aafea/e41f3a78e/acbab11af | 3 | |
| 安卓不透明文档 ID 文件名清理 | 58cc4c378/7baa88cb9 | 2 | 与 fork 安卓维护方向一致 |
| 数据治理 E2E 加密+blob 删除队列 | 7a2148234 | 1 | |

### 建议拒绝或暂缓

| 主题 | 规模 | 理由 |
|------|------|------|
| settings 灰卡重设计+供应商管理重做 | 36 | fork 设置页已自行演进(含 local-api 区) |
| workbench 桌面壳(窗口平台/dock/agent 中心) | 15 | 巨型改造,与 fork 冻结基线策略冲突 |
| native-feel macOS 系列 | 14 | fork 主打 Windows/Android |
| demo 落地页/演示壳 | 12 | 营销演示,非产品功能 |
| study-ui 迁移+style-lab+tokens 设计系统 | ~16 | 设计系统推倒重来 |
| 移动端响应式 token 系列 (01-01~08-01) | ~28 | 大改布局体系,风险高;可按需回采 |
| chat UI 大重建 (AgentTaskPanel/FlowToken/渲染器重做) | ~30 | 与 fork 当前 chat 架构分叉太大 |
| shell 沙箱/headless runner/自动化调度 | ~20 | fork 无此基础设施 |
| 图标 Phosphor 迁移/品牌图标 | 4 | 非必要 |

## 5. 复现方法

```bash
git fetch upstream
git log v0.9.40..upstream/main --no-merges --format='%h|%s'          # 分类
git log v0.9.40..upstream/main --no-merges --format='@@@%h|%s' --name-only  # 适用性
# 分类器: 按 fork 文件集(v0.9.40-fork.1 ls-tree)+工作区脏文件+三大警示文件判定
```
