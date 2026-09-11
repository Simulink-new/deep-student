# A5 data_governance 数据传递审计

> 撰写: 2026-09-11 08:08 CST | 基线 commit: ae1e6385 | 范围: `src-tauri/src/data_governance/`（44,474 行 / 44 文件 / 45 命令 / 39 emit / 330 SQL 点）
> 方法: PLAN.md 七项 rubric；sync 引擎核心全读 + 三个并行子审计（备份恢复 / 命令边界 / 迁移），行号均经 Read/grep -n 核实

## 范围与方法

子模块结构与采样深度：

| 子模块 | 行数 | 采样策略 |
|---|---|---|
| `sync/`（orchestrator/changeset/manifest/envelope/emitter/progress/conflict_resolver） | ~9.4K | **全读**（同步引擎数据装配与应用路径是核心问题域）；hlc/field_merge/tombstone 按调用点抽样（纯函数） |
| `commands_sync.rs` | 3.4K | **全读**（同步命令入口 + 19 个 emit 点 + 上传/下载/双向三分支） |
| `backup/` + `commands_backup.rs` + `commands_restore.rs` | ~7.9K | 子审计（备份/恢复 I/O 路径逐段核实，含 Backup API 步进、verify、manifest） |
| `migration/` | ~8.1K | 子审计（coordinator.rs 4.6K 逐段 + verifier/script_checker 抽样） |
| `commands.rs / commands_zip.rs / commands_asset.rs / commands_shared.rs / commands_types.rs / audit/ / schema_registry.rs / init.rs` | ~4.7K | 子审计（IPC 边界逐命令清单化） |
| 测试（migration_tests / critical_audit_tests / sync/tests / tests） | ~6.2K | 跳过 |

前提修正（对 PLAN.md §2 计数的校准）：`sync/permit.rs` 生产 SQL 实为 2 条（其余在 `#[cfg(test)]`，纯 RAII 权限结构）；`audit/mod.rs` 生产 SQL 实为 9 处（含 4 条 DDL）。

**emit 层结论（39 点全查）**：`commands_sync.rs` 19 处 + `sync/emitter.rs` 20 处全部是 `SyncProgress` 小 payload（phase/percent/计数/单字符串，~200B），emitter 有 100ms 节流 + 阶段变化强发（emitter.rs:23,59-88），文件级字节回调经全局 sink 200ms 最小间隔（emitter.rs:373,419-431）；备份任务事件 `backup-job-progress` 仅任务快照且 150ms 节流（backup_job_manager.rs:422,497-503）。**无数据本体过事件通道，无高频全量型 emit——该层健康，无需优化**。

## 数据流摘要

**同步引擎**：前端 `run_sync_with_progress` → 全局信号量 + RAII 会话（读 permit→写门）→ 遍历 4 个受管库读 `__change_log WHERE sync_version=0`（有索引，backup/mod.rs:649）→ `enrich_changes_with_data` 逐条点查整行 JSON →（下行）`list changes/` 按 `since_version` 增量拉取远端文件（整文件缓冲，v3/v2/v1 三格式兼容）→ 排序 → 事务内逐条冲突裁决（`__change_log` COUNT + 本地行读 + 业务指纹比较）+ UPSERT 回放（COALESCE + 字段级合并）→（上行）zstd 压缩 + DSBK 加密 + `put` → `mark_synced` → 重建 manifest（每表 COUNT+MAX checksum）再上传 → 文件级同步（ws_*.db / vfs_blobs / 资产目录**全量 sha256 扫描**比对清单）→ prune → 审计。增量拉取与变更去重语义存在，但装配与应用层有大量重复劳动（见 #5-#10）。

**备份/恢复**：Backup API 按 100 页步进（每步 sleep 50ms）复制 4 库 → 产物全量 SHA256 → 资产 `fs::copy` 后再读一遍 hash → 全部条目写单份 pretty JSON manifest → 自动 verify 把所有文件再读 1-2 遍。恢复反向 4 倍读放大（逐文件 hash + integrity_check + Backup API 再读 + 目标库再 quick_check）。ZIP 导出/导入流式（`io::copy` 进 ZipWriter），无整包入内存，但校验和导致每文件读两遍 + 整 zip 第三遍。

**迁移**：每次启动 `lib.rs:441 → init.rs:169 run_all` 无条件执行：pending 检查 → 有 pending 时整库 Backup API 快照（留 5 份）+ 对副本全库 quick_check → pre_repair（DDL 补齐，**含三份 init.sql 每次启动整份回放**）→ refinery 逐迁移 set-based SQL（用户数据在 SQLite 内部移动，不经 Rust 内存——这是对的）→ 全历史 verify（N+1 catalog 点查）→ fingerprint 重算并**无条件回写**几十~几百 KB canonical DDL → 再开 4 连接聚合注册表。

**IPC 边界**：命令层收敛为"小响应 + 后台 job + 进度事件"，无 base64/整表/Vec<u8> 过 IPC——设计干净；真正的问题在若干 `async fn` 是 **sync 命令在主线程做重 I/O**（诊断命令复制 4 个生产库）。

## 发现清单（影响降序，25 条）

量级假设统一口径：4 个受管库共 ~200MB / ~5 万行业务行；资产 2GB / 2 万文件；历史备份目录留 10 份；首同步或积压后一次变更集 1 万条 / 平均行 2KB。未单独注明者即用此口径。

| # | 位置(file:line) | 模式 | 复杂度/量级估计 | 触发频率 | 修复草图 | 破坏风险 |
|---|---|---|---|---|---|---|
| 1 | backup/mod.rs:1664-1683（睡眠在 1677；同模式 2420-2425，睡眠 2423） | 备份步进循环每 100 页（400KB）固定 sleep 50ms | ~512 步 × 50ms ≈ **25.6s 纯睡眠/次**（200MB）；恢复路径 2250-2285 的 `More` 分支不睡眠，行为不对称 | 每次备份（full/tiered/with_assets） | `More` 时不 sleep 仅 Busy/Locked 睡；或 step 页数提到 1000+；或 `run_to_completion` | 低——纯吞吐参数 |
| 2 | commands_backup.rs:1372 + mod.rs:1694 + assets.rs:721-731,1159 | 备份后自动验证 = 全量再读 2-3 遍（copy 后 hash dest、verify 再 hash、integrity_check） | DB 读 3 次 + 资产读 3 次 ≈ **额外 ~4.6GB I/O/次** | 每次备份成功后 | copy 用 tee-hash Reader 边写边算；auto-verify 降级为抽样或仅 integrity_check | 低 |
| 3 | commands_restore.rs:234-318 → mod.rs:2239 → mod.rs:2289-2290 | 恢复 4 倍读放大（逐文件 SHA256 + integrity_check + Backup API 再读 + 目标库再全扫） | 200MB 备份 ≈ 800MB 读 + 200MB 写 | 每次恢复 | .db 二选一（integrity_check 或 hash）；恢复后改 `PRAGMA quick_check` | 低 |
| 4 | orchestrator.rs:1897-1931（scan_asset_tree 每文件 calculate_file_hash）+ 1420（ws_*.db 同样）+ 1924 | 文件级同步对**全部**本地资产/工作区文件每次重算 sha256，无 mtime/size 快路径 | O(全部资产字节) CPU+读 I/O/次：2GB 资产 ≈ 2GB 读 + 数十秒哈希，即使零变更 | 每次同步（三方向均跑文件级） | 清单条目记录 size+mtime，先比对再哈希；同步成功后持久化本地快照 | 中——需在清单加 mtime 字段并保持旧清单兼容（缺字段回退全量哈希） |
| 5 | migration/coordinator.rs:269-315,499-580（快照留 5 份 :47） | 有 pending 迁移的启动：整库 Backup API 拷贝 + 对每个副本 `PRAGMA quick_check` 全库扫描 | O(4 库总字节)：1~3GB 读写 + 4 次全库 B-tree 扫描；快照 5×磁盘占用 | 每个新版本首次启动（每进程一次） | quick_check 降为 header/page_count 校验；快照留 2 份；schema 版本未变时跳过 | 中——快照是迁移失败的回滚保险，降级需保留至少 1 份 + 明确告警 |
| 6 | backup/mod.rs:2610-2653；调用点 commands_backup.rs:519,639,713,806 + commands_restore.rs:158 + commands_asset.rs:152-160,294-300 | `list_backups` 全量解析所有备份的完整 manifest（含每资产 sha256 条目），调用方常只用 1 个 | 10 份 × 2 万条 ≈ **60MB JSON 读+解析/次调用** | 每次打开备份列表/恢复前磁盘检查/verify | `get_backup(id)` 按目录直读；manifest 拆 summary.json + assets.ndjson | 低——新增只读接口，旧调用可保留 |
| 7 | changeset.rs:790-899,2159-2268（apply_single_record 及其调用链）+ commands_backup.rs:241-248,199-204 | 应用下载变更每条记录重复 catalog 查询：`ensure_table_allowed_and_exists`(sqlite_master) + `primary_key_columns`(PRAGMA) + `table_has_column`×多个调用点（picklist **每列一次**）+ 本地行读 + LWW 查询 | 首同步 5k 条 × ~8-10 查询 ≈ **4-5 万次冗余 catalog 点查**；同一表 schema 信息被重复查数千次 | 每次下载/双向同步的 apply 阶段 | 事务内 `HashMap<table, SchemaInfo>` 缓存（列名/PK/唯一索引/have deleted_at 一次性取齐，changeset.rs:2778 已有先例 columns_cache） | 低——纯读缓存，事务内 schema 不会变 |
| 8 | changeset.rs:1568-1628（build_download_id_aliases） | ID 别名构建 fixpoint 循环 × 每条 change × PRAGMA foreign_key_list/index_list/index_info + `source_obj.clone()` 每迭代 | O(N × 迭代数 × ~6 PRAGMA)：5k 条 × 2 迭代 ≈ **6 万次 PRAGMA** + 1 万次整行 JSON clone；首同步最大单点 | 每次应用下载变更 | FK/唯一索引信息随 #7 的 SchemaInfo 缓存；fixpoint 只重跑上一轮产生新别名的表；obj 免 clone（borrow + Cow） | 中——别名正确性影响跨设备 ID 归并，需保留收敛语义与测试 |
| 9 | changeset.rs:2772-2816（enrich_changes_with_data）+ commands_sync.rs:914-935 | 待上传变更不按 (table, record_id) 去重：同一记录多次更新 = 多次点查同一行 + 上传载荷含多份相同整行数据 | 假设 1 万条 pending 中 40% 是重复记录 → ~4 千次冗余点查 + 上传体积 +30-60%（重复整行 JSON）| 每次上传/双向同步 | 按 (table, record) 保留 changed_at 最新一条（DELETE 优先），再 enrich；批量 `WHERE id IN (...)` 按表预取替代逐条点查 | **高——云协议兼容**：去重后下载端回放顺序变化，需保持"每记录最终态"语义；DELETE+INSERT 序列不能折叠错误 |
| 10 | commands_sync.rs:1165-1166,2868-2869（refs_vec 全量 clone）+ orchestrator.rs:650（`changes.to_vec()` 再 clone 一份） | 上传链数据在内存至少 3-4 份深拷贝：all_enriched → filtered refs_vec → payload.changes → 序列化 json | 1 万条 × 2KB ≈ **20MB×3 同时驻留**（峰值 ~60MB + 压缩缓冲） | 每次上传/双向同步 | filter 改 `drain`/`partition` 产出 owned；`upload_enriched_changes` 接收 `Vec` by value 或 `&[&T]`；去掉 to_vec | 低——内部所有权重构 |
| 11 | commands_sync.rs:199-223（drain 循环）× orchestrator.rs:1988-2007（mark_blob_deleted 每次下载+上传整份云端 tombstone 清单） | blob 删除队列逐条传播：每条 = 完整清单 GET + 本地插入 + 完整清单 PUT | 队列 500 条上限 → **500 次全清单往返**（清单随删除历史线性变大）→ O(n²) 网络 | 每次同步（队列非空时） | 本地合并全部 pending 进一个 manifest 后一次 GET+PUT；失败整批重试计数 | 中——tombstone 清单是云端共享状态，需保持并发合并语义（或加锁窗口内完成） |
| 12 | migration/coordinator.rs:2239-2246（chat_v2 293 行 init.sql 整份回放）+ 2546,2601-2699（mistakes：42 个 add_column_if_missing ≈84 catalog 查询 + 353 行回放）+ 1622-1625,1954-2017（VFS：24 查询 + 25 条 DDL） | 三份 init.sql / 兼容补齐**每次启动无条件重放**，无 is_migration_recorded 守卫 | 每次启动 ~180 次 catalog 操作 + ~750 行 SQL 重复解析 ≈ 50-150ms 纯冗余 | 每次启动 | 每段加 `is_migration_recorded` 守卫；列检查按表分组单次 PRAGMA 复用 | 中——迁移守卫遗漏会把老库卡在缺失补齐状态，需按 legacy 库实测 |
| 13 | commands.rs:951-1167（slot 测试 217-355，report 内联 1092-1107） | **sync 命令在主线程**：诊断报告 = fs::copy 4 个生产库+wal + 两轮沙箱全量迁移 + 删目录 | O(全部库字节) GB 级 → UI 冻结数十秒-分钟 | 用户点"复制诊断报告" | 改 async + spawn_blocking；或后台 job + 事件回传；复制前先 checkpoint | 低——行为不变只挪线程；另:288-308 拷 -wal 未 checkpoint 属正确性问题应顺手修 |
| 14 | backup/mod.rs:1821-1851 | 增量备份整包内存缓冲：`Vec<ChangeLogEntry>` collect → `to_string_pretty` → 写盘后重读 hash | 未同步变更 10 万条 ≈ 15MB → pretty 字符串 ~30MB 双倍驻留 | 每次增量备份 | `serde_json::Serializer::new(BufWriter)` 流式写；写入时同步 hash | 低 |
| 15 | zip_export.rs:271-278,302 + commands_zip.rs:822-826,951 | ZIP 导出每文件读两遍（先整文件 sha256 再压缩）+ 结尾整 zip 再 hash 一遍 | 备份目录 2.2GB → **~4.4GB + 2.2GB 读/次** | 每次 ZIP 导出 | hash 包装 Writer 边压边算；或 checksum 改记录 central directory | 低 |
| 16 | changeset.rs:319-342 + commands_restore.rs:671-685 | 恢复收尾 `UPDATE ... SET sync_version=local_version` 每行触发 trg_upd 写 `__change_log`，随后整表 DELETE | 5 万行业务行 → **5 万次触发器 INSERT + 全表 DELETE**（单事务内，无逐行 execute 问题） | 每次恢复后 | UPDATE 前临时 DROP/禁用触发器再重建；或按表跳过无触发器表 | 中——触发器禁用窗口内的并发写需写门保护（恢复本身持维护模式，可行） |
| 17 | commands.rs:437,680,765,902,959 → init.rs:315-321 → migration/coordinator.rs:3480-3547；init.rs:121-222（needs_initialization:293 存在但生产未用） | 注册表缓存从不命中：5 个命令每次重新聚合（4×Connection::open + 全历史 SELECT）+ 启动每次全跑 run_all + 二次开库聚合 | 每次启动 + 设置页每次刷新：~20 次连接打开 + O(全部迁移行)×2 | 每次启动/设置页刷新 | `needs_initialization` 快路径；迁移完成事件失效缓存；聚合复用 run_all 的 report | 低 |
| 18 | migration/verifier.rs:35-76,99-116 + coordinator.rs:2991-2995,1406-1464 | 启动验证 N+1：verify_migrations 对**所有已应用**迁移的每个 expected 项独立点查；`column_exists` 每次把整表列名 collect 成 Vec 再 contains；repair_refinery_checksums 每迁移一条 SELECT | ~400 个 expected 项 + 63 条迁移 ≈ **~460 次点查/启动**；chat_v2 V001 的 51 列各物化一次列名 Vec | 每次启动 | 每 (db,table) 缓存一次 PRAGMA table_info；只验本轮新应用迁移，稳态只做 fingerprint 对比；checksum 一次全表 SELECT 进 HashMap | 低——验证语义收敛为"新迁移强验 + 稳态指纹"需评审接受 |
| 19 | migration/coordinator.rs:3083-3097,3170-3265 | fingerprint 命中后仍无条件 UPDATE 回写 verified_at/fingerprint/**canonical_schema（全库 DDL 文本几十~几百 KB）**；compute 每表 3 条查询 | ~112 条查询 + ~200KB 字符串写/库/启动 × 4 库 | 每次启动 | 一致时只更新 verified_at（或跳过写）；canonical_schema 仅漂移/首次时写 | 低 |
| 20 | commands.rs:379（try_save_audit_log）+ audit/mod.rs:211-215 | 每次审计写入都跑 4 条 DDL（CREATE TABLE + 3×CREATE INDEX IF NOT EXISTS） | 每个治理命令 +4 次语句解析执行 | 每个治理命令（含每次同步×2 条审计） | init.rs:270 启动已 init；删除 per-write DDL | 低 |
| 21 | conflict_resolver.rs:341-344（`d.clone()` 先于 COUNT 检查） | resolve_one 在便宜的 `__change_log` COUNT 判定**之前**深拷贝整行云端数据 | 1 万条下载 × 2KB ≈ **20MB 纯浪费 clone**（绝大多数非冲突） | 每条下载变更 | 调序：先 has_local_change（COUNT），命中才 clone | 低——一行顺序调整 |
| 22 | orchestrator.rs:916-938（sort_by 内 parse_flexible_timestamp） | 下载变更排序比较器内做 3 格式时间戳解析 | O(n log n) 次解析：1 万条 ≈ **~14 万次**字符串解析 | 每次下载 | 预计算排序键（装饰排序），或下载时顺带缓存 parsed 值 | 低 |
| 23 | changeset.rs:2732-2759（sqlite_value_to_json） | 每列试探 i64→f64→String→Vec<u8> 四次类型转换；字符串外形像 JSON 则解析成 Value（上传时又 to_vec 序列化回去） | 行×列次转换 + JSON 文本→Value→JSON 文本 roundtrip：1 万行 × 30 列 ≈ 30 万次试探 + 双重序列化 | 每次 enrich（上行）/get_record_data（下行冲突检测） | 按列声明类型直取（PRAGMA type 已可得，随 #7 缓存）；JSON 列保持文本直传免 roundtrip | 中——类型映射需全表回归（TEXT 存数字等历史脏数据场景） |
| 24 | commands_sync.rs:972-989,2500-2518（mark_synced per-db 重开连接 + O(dbs×N) filter）+ changeset.rs:2879-2958（calculate_simple_checksum） | 一次同步内 `get_database_sync_state`（每表 COUNT+MAX 全表聚合）跑 2-3 遍（初始 manifest + 上传前 refresh；detect/resolve 命令再各一遍）；每遍后重开 4 连接 | ~30 表 × 2 查询 × 3 遍 ≈ **180 次全表聚合/次同步** + ~16 次连接打开 | 每次同步 | 会话内缓存 sync state，仅 mark_synced 后对受影响库重算；manifest 复用首次结果 + 增量 data_version | 低 |
| 25 | commands_asset.rs:42,205,306（async 内裸阻塞 I/O）+ commands_zip.rs:557-1091（导出路径未包 spawn_blocking，与 import 的 1357 不一致） | async 命令直接跑全树 walk/拷库/逐文件哈希/压缩，占用 tokio worker | 大备份期间占死 1 个 worker（安卓 worker 少，拖慢其他命令） | 用户触发但常驻影响 | 统一包 spawn_blocking（照抄 commands_zip.rs:1357 模式） | 低 |

已识别未入表（影响较小或属一次性路径，供后续任务参考）：恢复时 manifest 深拷贝（commands_restore.rs:168-174）；资产恢复 per-file create_dir_all + 失败整文件重拷（assets.rs:850,977,56-109）；分层 manifest 线性膨胀（mod.rs:2840-2845）；审计 Mutex 主线程查询（commands.rs:462-510）；信号量 200ms 轮询（commands_shared.rs:150-168）；迁移 pre_repair 裸 autocommit DDL（coordinator.rs:2014-2017，一次性 legacy 路径）；init.sql 全文 to_uppercase 拷贝 + get_migrations Vec clone（coordinator.rs:1705-1742,2874-2913）；下载变更全量累积内存峰值（orchestrator.rs:743，与 #9/#10 同治）；slot D 拷 -wal 未 checkpoint（commands.rs:288-308，正确性>性能，已并入 #13 修复草图）。

## 最小数据传递方案

该模块理想形态（功能不变前提下）：

1. **上行每记录只传一次**：pending 变更按 (table, record_id) 去重到最终态再 enrich，行数据从批量 `IN` 预取取得；enrich→filter→upload 全链 borrow/move，内存中行数据恰好一份；上传文件已是 zstd+分批（1000 条/批），保留。
2. **下行 schema 信息每事务查一次**：SchemaInfo 缓存（列/PK/唯一索引/FK/声明类型）贯穿 apply 事务，消除 #7/#8/#23 的重复 catalog 与类型试探。
3. **文件级同步零哈希快路径**：清单带 size+mtime，未变文件零读；哈希仅在尺寸/时间不匹配时回退。
4. **备份单遍化**：tee-hash 使 copy/压缩/校验共享一次读；verify 抽样化；manifest 拆 summary + 资产 NDJSON，按 ID 直读。
5. **启动稳态零冗余**：迁移结果 + 注册表缓存在版本未变时完全跳过（needs_initialization 快路径）；verify 只验新迁移；fingerprint 命中不回写大文本。
6. **IPC 已是最小形态**（小响应 + job + 节流进度事件），保持；仅需把主线程重活挪入 spawn_blocking。

## 不动清单

- **云端对象协议**：`data_governance/changes/{device}/{version}-{nonce}.json.zst` 命名与版本空间、v3/v2/v1 三格式解析顺序、`manifests/{device}.json` 按设备清单合并语义、`LEGACY_MANIFEST_KEY` 回退、tombstone 清单 schema——旧版本 app 与云端既有数据互换不能破坏（#9/#11 的改动必须保持旧文件可读、新文件对旧端可跳过）。
- **DSBK 加密容器与"先压缩后加密"顺序**（orchestrator.rs:661-668）——密码学语义不可调。
- **SyncProgress 事件名 `data-governance-sync-progress` 与阶段序列**：`completed`/`failed` 必须是最后一个事件（旧前端以此触发回调，emitter.rs:9-11）；六阶段命名映射。
- **`__change_log` 触发器（trg_upd）语义与 suppress/echo 抑制机制**（changeset.rs:1726-1749）——同步收敛性依赖；#16 的触发器禁用只能在恢复维护窗口内。
- **冲突裁决语义**：LWW 严格晚于才跳过、HLC 漂移 60s 防线、KeepLatest 2s 时钟容差、`__sync_conflicts` 双端留痕 + data_hash 部分唯一去重——行为变化会改变多设备收敛结果。
- **软删 tombstone 语义**（deleted_at 显式 null 复活、幂等时间戳取 changed_at）与 blob 内容寻址（sha256 即 key，注释明确不加密以保去重）。
- **备份 A/B 插槽 + 重启切换机制**、refinery 迁移历史表语义、`local_api` v1.1 stable 契约（相邻模块，PLAN.md §8 全局冻结）。
