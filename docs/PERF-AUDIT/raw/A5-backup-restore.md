# A5 子审计: 备份/恢复路径 (子代理成果, 2026-09-11, 行号已 grep -n 核实)

## 发现清单（12 条，影响降序）

1. **每 100 页睡眠 50ms 的备份步进循环** | `src-tauri/src/data_governance/backup/mod.rs:1664-1683`（睡眠在 1677；同样模式 `mod.rs:2420-2425`，睡眠在 2423） | 限速节流 | `backup.step(100)` 每步 100 页×4KB=400KB 后 `StepResult::More` 也睡眠 50ms → 吞吐上限 ~8MB/s/库。假设 4 库共 200MB ≈ 51200 页 = 512 步 × 50ms ≈ **25.6 秒纯睡眠/每次备份**（工作区 ws_*.db 走 2420 同样模式；恢复路径 2250-2285 的 `More` 分支不睡眠，行为不对称） | 每次备份（full/tiered/with_assets 均经 `backup_single_database`） | `More` 时不 sleep，仅 Busy/Locked 睡；或 step 页数提到 1000+，或直接 `run_to_completion(1000, ...)`

2. **备份后自动验证 = 全量再读 2-3 遍** | `commands_backup.rs:1372`（`verify_with_assets`）+ `mod.rs:1694`（copy 后 hash dest）+ `assets.rs:721-731`（copy 后 hash dest）+ `assets.rs:1159`（verify 再 hash） | 重复全量 I/O | 假设备份 200MB DB + 2GB 资产/2 万文件：DB 写 1 次、读 3 次（hash、验证 hash、integrity_check）；资产读 3 次（copy 后 hash、verify hash、metadata）≈ **额外 ~4.6GB I/O/每次备份** | 每次备份成功后（tiered 路径 2058 同样） | copy 时用 tee-hash Reader 一边写一边算 SHA256；auto-verify 降级为抽样或仅 integrity_check

3. **恢复路径 4 倍读放大** | `commands_restore.rs:234-318`（逐文件 SHA256 + 每 .db `PRAGMA integrity_check`）→ `mod.rs:2239`（Backup API 再读一遍）→ `mod.rs:2289-2290`（恢复后目标库 integrity_check 再全扫） | 重复全量 I/O | 假设 200MB 备份 → 约 800MB 读 + 目标盘 200MB 写。restore_with_assets 老路径（`mod.rs:1234` verify_internal）还会再叠一层 | 每次恢复 | .db 文件二选一（integrity_check 或 hash）；恢复后 integrity_check 改 `PRAGMA quick_check`

4. **list_backups 全量解析所有 manifest.json** | `mod.rs:2610-2653`（`fs::read_to_string`+parse 每个备份的完整 manifest，含全部资产条目）；调用点 `commands_backup.rs:519、639、713、806`、`commands_restore.rs:158` | 全量加载后只用摘要 | 假设备份目录留 10 份 × 每份 manifest 含 2 万条 `BackedUpAsset`（64 字符 sha256+时间戳 ≈300B/条）≈ **60MB JSON 读+解析/每次调用**；恢复/磁盘检查只需 1 个 ID 却解析全部 | 每次打开备份列表页、每次恢复前磁盘检查、每次验证 | 按 ID 直读 `<backup_id>/manifest.json`；manifest 拆 summary.json + assets.ndjson；按目录 mtime 缓存

5. **增量备份整包内存缓冲 + pretty JSON** | `mod.rs:1821-1833`（`Vec<ChangeLogEntry>` collect）→ `1841`（`to_string_pretty`）→ `1851`（写盘后重读 hash） | 全量 Vec + 字符串双倍内存 | 假设未同步变更 10 万条 ×150B ≈ 15MB → pretty 字符串 ~30MB 同时驻留，再整块 `write_all` | 每次增量备份 | 用 `serde_json::Serializer::new(BufWriter)` 流式写（或 NDJSON 每行 flush）；写入时同步 hash

6. **资产备份每文件读 2 次 + 3 次 stat** | `assets.rs:714`（`fs::copy`）→ `721-731`（对 dest 再 `calculate_file_checksum` 全读）；`648`（`fs::metadata`）、`663`（`fs::symlink_metadata`）、`734`（`get_file_modified_time` 又一次 metadata） | 重复读 + 重复 stat | 假设 2GB/2 万文件 → 额外 2GB 读 + 6 万次 syscall | 每次含资产备份（默认 `compute_checksum:true`，单文件上限 500MB/总量 10GB，`assets.rs:292-293`） | 用包装 Reader 在 `io::copy` 流式过程中同时 hash 源文件；复用 `entry.metadata()`（DirEntry 自带）

7. **同步应用深拷贝链 + N+1 点查** | `commands_backup.rs:367-395`（每条 `SyncChangeWithData` 含 `serde_json::Value` 行数据被 `.clone()` 进 owned_changes）+ `241-248`（`has_unsynced_local_change` 每条变更一次 COUNT 查询）+ `199-204`（`get_record_data` 每条一次 SELECT） | 深拷贝 + 循环内单行查询 | 假设一次同步 1 万条变更 × 平均 2KB JSON → 20MB 深拷贝 + 1-2 万次点查 | 每次下载同步/双向同步 | 按 (table, id) 批量 `WHERE id IN (...)` 预取本地版本；改 `&mut`/Cow 传 `suppress_change_log` 免 clone

8. **分层资产备份同样双读 + manifest 线性膨胀** | `mod.rs:2995-2998`（copy 后 hash dest）→ `2840-2845`（每个资产 push 进 `manifest.files`）→ `mod.rs:167-174`（`to_string_pretty` 整串写盘） | 重复读 + O(资产数) 元数据全量驻留 | 2 万资产 → manifest.json 约 5-8MB/份，且每次备份新建一份，随后被发现 4 的 list_backups 反复解析 | 每次分层备份含资产时 | 资产清单与主 manifest 分文件；manifest 用紧凑序列化流式写

9. **ZIP 导出每文件读两遍 + 整包第三遍** | `zip_export.rs:271-273`（`calculate_file_sha256`）→ `277-278`（`io::copy` 进 ZipWriter，注释确认流式无 read_to_end）→ `302`（最终对整个 zip 再 hash 一次） | 重复全量读（内存安全） | 假设备份目录 2.2GB → ~4.4GB 读 + 2.2GB zip hash 读 | 每次 ZIP 导出 | 校验和与压缩共用一次读（hash 包装 Writer）；或 zip_checksum 改为记录 central directory 信息

10. **恢复时整份 manifest 深拷贝** | `commands_restore.rs:168-174`（`manifests.iter().find(...).clone()` 拷贝含全部 `files`+`assets.files` 的 BackupManifest） | clone() 深拷贝链 | 假设 2 万资产条目 ≈ 6-12MB 一次性堆拷贝，之后仅按引用使用 | 每次恢复 | find 后取引用（生命周期足够，或 `Arc<BackupManifest>`）

11. **恢复收尾触发器写放大** | `sync/changeset.rs:319-342`（`UPDATE ... SET sync_version=local_version` 集合式，但每行触发 trg_upd 写 `__change_log`，随后 `DELETE FROM __change_log` 全删） | O(行数) 触发器写后即删 | 假设 4 库共 5 万行业务行 → 5 万次触发器 INSERT + 全表 DELETE，均在单事务内（`commands_restore.rs:671-685` BEGIN IMMEDIATE 包裹，无逐行 execute 问题） | 每次恢复后（重启切换插槽前） | UPDATE 前临时 DROP/禁用 trg_upd 触发器再重建；或 UPDATE 加跳过无触发器表

12. **资产恢复 per-file `create_dir_all` + 失败整文件重拷** | `assets.rs:850-852、977-979`（每文件 `create_dir_all(parent)`）+ `56-109`（`copy_file_with_retry` 失败删 dest 后从 0 重拷整个文件，最多 5 次 ×80ms） | per-file syscall 开销 + 无断点重试 | 2 万文件 → 2 万次冗余 create_dir_all（父目录几乎总是已建）；大文件中途失败重拷整文件 | 每次恢复资产 | 目录级 memo（HashSet 记已建目录）；按 mtime+size 断点或 `io::copy` 带 range 续传

## 数据流摘要（备份/恢复）

备份入口（`data_governance_run_backup`/`backup_tiered`）spawn 后台任务，经全局 permit 互斥，核心走 SQLite Backup API 按 100 页步进复制 4 个库到时间戳子目录，随后对每个产物全量 SHA256，资产目录（images/documents/vfs_blobs 等，BLOB 实体存磁盘文件而非库内，`vfs/repos/attachment_repo.rs` 的 BLOB_MISSING 语义可证；chat_v2.db 内嵌 base64 图片消息是最大的库内大对象来源）用 `fs::copy` 逐文件复制后再读一遍算校验和，全部条目（含每资产 64 字符 sha256）写进单份 pretty JSON manifest。备份成功后自动 `verify_with_assets` 把所有 DB 和资产再完整读 1-2 遍。恢复走后台任务：先 `list_backups()` 解析全部历史 manifest 找目标（再深拷贝一份），Verify 阶段逐文件 SHA256 + 每 .db integrity_check，Replace 阶段经 Backup API 复制到非活跃插槽并对目标库再跑一次 integrity_check，资产按 manifest 逐文件 `fs::copy`（带重试与 per-file 进度回调），收尾在单事务内重置同步基线并标记重启切换插槽。ZIP 导出/导入全程流式（`io::copy` 进 ZipWriter、导入带 zip-bomb 防护与断点续传），无整包入内存。IPC 边界干净：`backup-job-progress` 事件 payload 仅是任务快照（进度/相位/消息/stats，`backup_job_manager.rs:497-503、948-986`），且按 150ms 节流（`:422`）；commands_backup/commands_restore 的返回类型均为小结构体，无 `Vec<u8>`/base64 过 IPC（base64 仅存在于 `commands.rs:5650` 等图片预览命令，不在备份路径）。主要代价集中在：步进睡眠封顶吞吐（#1）、备份/恢复各 3-4 倍的重复读 I/O（#2/#3）、以及 list_backups 对含全部资产明细的 manifest 的反复全量解析（#4）。
