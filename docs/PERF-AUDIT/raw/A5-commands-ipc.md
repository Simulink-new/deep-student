# A5 子审计: 治理命令 IPC 边界 (子代理成果, 2026-09-11, 行号已 Read/grep -n 核实)

## 前提修正
- **sync/permit.rs 的"17 个 SQL 点"不成立**：生产代码只有 2 条 SQL（`PRAGMA query_only=ON` 与断言查询，permit.rs:66/74），其余 ~15 条全部在 `#[cfg(test)]` 测试块（356-548 行）；无 permit 表操作，是纯 RAII 权限结构（ReadPermit/WritePermit/SyncSession），无逐行写、无 N+1、无需事务。
- **audit/mod.rs 生产 SQL 点实为 9 处**（含 4 条 DDL），并非 18。

## A) 命令 IPC 边界清单（3 个文件含 #[tauri::command]，共 17 个）

### commands.rs（10 个）
| 命令 | 行号 | 返回类型 | 量级判断 | 高频? |
|---|---|---|---|---|
| data_governance_get_maintenance_status | 407 (async) | MaintenanceStatusResponse | 极小（1 个 bool） | 启动 1 次（App.tsx:576） |
| data_governance_get_schema_registry | 432 (**sync**) | SchemaRegistryResponse（4 库摘要 Vec） | 小（不含完整历史，仅 migration_count） | 设置页打开时 |
| data_governance_get_audit_logs | 462 (**sync**) | AuditLogPagedResponse（logs: Vec，默认 limit=100 + total） | 中，**有界分页**，响应 DTO 已裁剪 details 字段 | 审计页翻页 |
| data_governance_cleanup_audit_logs | 531 (**sync**) | u64 | 极小 | 手动触发 |
| data_governance_get_migration_status | 673 (**sync**) | MigrationStatusResponse | 小（4 条库摘要） | 启动 1 次（useMigrationStatusListener.ts:160）+ 设置页 |
| data_governance_run_health_check | 757 (**sync**) | HealthCheckResponse | 小（4-8 条 + issues） | 设置页 |
| data_governance_get_database_status | 896 (**sync**) | Option<DatabaseDetailResponse>（**含单库全部 migration_history Vec**） | 中（每行小，全历史返回） | 单库详情查看 |
| data_governance_get_migration_diagnostic_report | 951 (**sync**) | **String（整份文本报告）** | 中（4KB+，随历史增长）——但重活在后面 | 用户点"诊断" |
| data_governance_run_slot_c_empty_db_test | 1170 (**sync**) | SlotMigrationTestResponse{report: String} | 重 I/O，返回小 | 手动 |
| data_governance_run_slot_d_clone_db_test | 1179 (**sync**) | 同上 | **复制全部 4 库文件**，返回小 | 手动 |

### commands_zip.rs（3 个）
| 命令 | 行号 | 返回类型 | 量级 | 高频? |
|---|---|---|---|---|
| data_governance_backup_and_export_zip | 74 (async) | BackupJobStartResponse | 极小（job_id），进度走事件 | 用户触发 |
| data_governance_export_zip | 457 (async) | BackupJobStartResponse | 极小 | 用户触发 |
| data_governance_import_zip | 1134 (async) | BackupJobStartResponse | 极小 | 用户触发 |

### commands_asset.rs（4 个）
| 命令 | 行号 | 返回类型 | 量级 | 高频? |
|---|---|---|---|---|
| data_governance_scan_assets | 27 (async) | AssetScanResponse（**仅统计** HashMap+计数） | 小——**无文件内容过 IPC** | 备份前预览 |
| data_governance_get_asset_types | 82 (sync) | Vec<AssetTypeInfo>（~6 条静态） | 极小 | UI 下拉 |
| data_governance_restore_with_assets | 119 (async) | RestoreResultResponse（摘要） | 小 | 手动 |
| data_governance_verify_backup_with_assets | 266 (async) | BackupVerifyWithAssetsResponse（错误列表） | 小-中 | 手动 |

**整表/全历史/base64 结论**：范围内无 base64 大对象或整表命令。最接近"全量"：get_audit_logs（默认 100 条有界）、get_database_status（单库全迁移历史）、diagnostic_report（String 全文）。**真正风险不在 IPC 载荷，而在 sync 命令在主线程做重 I/O**（B-1/B-2）。

**zip 内存问题**：**不是整包缓冲**。导出走 `ZipWriter::new(File)` + 每文件 `std::io::copy`（commands_zip.rs:752/838；zip_export.rs:276-278 注释明确"流式"）；导入同样 `io::copy` 流式解压（zip_export.rs:424/651），且解包包在 `spawn_blocking`（commands_zip.rs:1357、commands_shared.rs:563）。content:// 虚拟 URI 是磁盘临时物化而非内存（commands_zip.rs:1156）。瑕疵：导出路径未包 spawn_blocking + 校验和双读（B-4/B-6）。

## B) 发现清单（影响降序，12 条）

| # | file:line | 模式 | O/量级假设 | 频率 | 修复草图 |
|---|---|---|---|---|---|
| 1 | commands.rs:951-1167（slot 测试在 217-355，report 内联调用 1092-1107） | **sync 命令在主线程跑全套重活**：诊断报告 = 复制 4 个生产库+wal（fs::copy）+ 两轮沙箱全量迁移 + 删目录 | O(全部库字节)，GB 级时 UI 冻结数十秒-分钟 | 用户点"复制诊断报告" | 改 async + `spawn_blocking`；或后台 job + 事件回传；至少 Slot D 复制前先 checkpoint |
| 2 | commands.rs:437/680/765/902/959 → init.rs:315-321 → migration/coordinator.rs:3480-3547 | **注册表缓存从不命中**：5 个命令每次都 `get_current_schema_state` 重新聚合（4×`Connection::open` + table_exists + 全历史 SELECT），再覆写 `Arc<RwLock<SchemaRegistry>>` | 每次调用 4 次开库 + O(全部迁移行)；均在主线程 | 每次启动+设置页每次刷新 | 读缓存为主，迁移完成事件时失效；或加 TTL/版本戳比对 |
| 3 | commands.rs:379（try_save_audit_log）+ audit/mod.rs:211-215 | **每次审计写入都跑 4 条 DDL**（CREATE TABLE + 3×CREATE INDEX IF NOT EXISTS） | 每条审计日志多 4 次语句解析执行 | 每个治理命令 | init.rs:270 启动时已 init 过；删除 per-write init |
| 4 | commands_asset.rs:42 / 205 / 306 | **async 命令内裸跑阻塞 I/O**（无 spawn_blocking）：scan_assets 全树 walk、restore_with_assets_to_dir 拷库、verify_with_assets 逐文件哈希 | O(资产总字节)；安卓 tokio 工人少，拖慢其他命令 | 用户触发但常驻影响 | 与 import 一致包 `spawn_blocking` |
| 5 | commands_asset.rs:152-160 / 294-300 | **为取 1 个 manifest 读全部备份**：`list_backups()` 枚举每个备份目录的 manifest.json 再 `find()` | O(备份个数 × manifest 读盘) | 每次 restore/verify | BackupManager 增加 `get_backup(id)` 只读单目录 |
| 6 | commands_zip.rs:822-826 + 951 | **导出时每文件读两遍**：include_checksums（默认 true）先整文件哈希，再重新打开压缩；结尾再整 zip 哈希一次 | O(2×备份内容 + 1×zip) 磁盘读 | 每次 ZIP 导出 | 写压缩器时用 Tee/HashWriter 边压边算；或默认关 checksums |
| 7 | commands_zip.rs:557-1091 | **导出路径阻塞 I/O 直接在 async 任务跑**，与 import 的 spawn_blocking（1357）不一致 | 大备份期间占死 1 个 tokio worker | 每次导出 | 包 spawn_blocking（模式照抄 1357 处） |
| 8 | commands.rs:462-510 + audit/mod.rs:146-177 | **主线程持全局 Mutex<Connection> 查询**：sync 命令 get_audit_logs 持锁跑 COUNT(*)+分页查询；后台 try_save_audit_log 争同一把锁 | 锁窗口 = 查询时长；审计页翻页与后台审计写互相串行、卡 UI 线程 | 审计页每次翻页 | 改 async + spawn_blocking；或 WAL 模式下用独立只读连接 |
| 9 | init.rs:121-222 + lib.rs:441 | **启动每次全跑**：ensure dirs → audit CREATE → run_all（磁盘预检、可能有核心库快照备份、逐库迁移+验证）→ 再聚合（4 次开库）。`needs_initialization()`（init.rs:293）存在但生产路径未调用（仅测试用） | 每次启动 O(4-8 连接 + 验证查询 + 可能的快照拷贝) | 每次冷启动 | 先走 needs_initialization 快路径；聚合复用 run_all 的 report 而非二次开库 |
| 10 | commands.rs:288-308 | **拷贝运行中的 -wal 文件**：slot D 测试直接 fs::copy `*.db-wal`，未先 checkpoint，可能得到撕裂快照（正确性>性能） | 1 次全库拷贝 | 诊断/测试命令 | 拷贝前对源库执行 `PRAGMA wal_checkpoint(TRUNCATE)` 或用 SQLite Backup API |
| 11 | schema_registry.rs:198-253 vs coordinator.rs:3480-3547 | **两套聚合实现并存**：`SchemaRegistry::aggregate_from_databases`（未被命令路径使用）与 coordinator 私有版本字段语义不同 | 无运行时代价，漂移风险 | — | 收敛为一个实现 |
| 12 | commands_shared.rs:150-168 | **信号量等待 200ms 轮询**：`select! { permit, sleep(200ms) }` 循环重试取消检查 | 每个排队任务 5 唤醒/秒（CPU/电池噪音，移动端） | 有任务排队时 | 用 `Notify`/直接对 acquire future 做 select 取消 |

N+1 专项核查：范围内**没有 SQL N+1**。锁内重活专项：唯二的重活持锁点是 B-8（audit Mutex）。

## C) 模块职责摘要（3 句）
1. 命令层把 IPC 边界收敛为"小响应 + 后台 job + 进度事件"模式，重数据全部留在磁盘不过 IPC——设计亮点。
2. 治理核心维护 4 个受管 SQLite 的迁移状态派生视图与独立 audit.db 审计流，启动时 fail-close 跑迁移并聚合；但"派生视图"退化为每次全量重算，缓存形同虚设（B-2/B-9）。
3. sync/permit.rs 是纯 RAII 同步读写门（只读连接 + AppState 写门槽 + 全局信号量），生产 SQL 仅 2 条 PRAGMA 相关语句。
