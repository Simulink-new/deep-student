# 00 — 模块清单与数据流索引

> 2026-09-11 00:10 CST | 基线 ae1e6385 | 自动采集,索引文件与本文件同目录

## 后端模块清单 (Rust)

| 模块 | 行数 | 命令 | emit | SQL | serde序列化 | 审计任务 |
|------|------|------|------|-----|------------|---------|
| chat_v2/ | 86,045 | 80 | 39 | 251 | 182 | A1/A2 |
| vfs/ | 58,616 | 129 | 26 | 1,105 | 81 | A3/A4 |
| data_governance/ | 44,474 | 45 | 39 | 330 | 29 | A5 |
| llm_manager/ | 21,205 | 0 | 42 | 1 | 29 | A6 |
| dstu/ | 15,767 | 54 | 2 | 35 | 18 | A7 |
| database/ | 8,938 | 0 | 0 | 455 | 41 | A3 |
| cmd/ | 7,783 | 169 | 6 | 14 | 16 | A10 |
| memory/ | 7,587 | 36 | 0 | 32 | 5 | A8 |
| multimodal/ | 6,553 | 0 | 0 | 50 | 1 | A9 |
| mcp/ | 5,148 | 0 | 11 | 0 | 5 | A10 |
| tools/ | 4,259 | 0 | 2 | 0 | 2 | A2(工具执行器) |
| cloud_storage/ | 3,882 | 14 | 1 | 0 | 1 | A10 |
| llm_usage/ | 3,160 | 7 | 0 | 37 | 0 | A6 |
| essay_grading/ | 2,884 | 20 | 4 | 0 | 4 | A8 |
| ocr_adapters/ | 2,763 | 0 | 0 | 0 | 0 | A9 |
| providers/ | 2,507 | 0 | 0 | 0 | 3 | A9 |
| adapters/ | 2,435 | 0 | 0 | 0 | 7 | A9 |
| utils/ | 1,667 | 0 | 1 | 0 | 0 | 随所属模块 |
| local_api/ | 1,403 | 3 | 0 | 17 | 0 | A7 (v1.1 契约冻结) |
| translation/ | 940 | 4 | 5 | 0 | 0 | A9 |
| qbank_grading/ | 829 | 2 | 4 | 8 | 0 | A8 |
| research/ | 788 | 0 | 0 | 0 | 14 | A10 |
| crypto/ | 514 | 0 | 0 | 0 | 3 | A10(安全边界,不动) |
| 其余小模块 | ~400 | 0 | 0 | 0 | 0 | 随所属模块 |
| **根级 \*.rs** | **64,737** | **195** | **64** | **238** | **98** | **A10** |

根级大文件: commands.rs 5,900 / lance_vector_store.rs 4,491 / question_import_service.rs 3,953 / document_parser.rs 3,612 / notes_exporter.rs 3,052 / lib.rs 2,471 / streaming_anki_service.rs 2,452 / question_bank_service.rs 2,275 / notes_manager.rs 2,247 / question_sync_service.rs 2,156 / models.rs 2,118

## 前端区域清单 (TS/TSX)

| 区域 | 行数 | 文件数 | 审计任务 |
|------|------|--------|---------|
| features/chat/ | 126,451 | ~400 | B3 |
| components/ | 69,163 | 206 | B6 |
| debug-panel/ | 24,970 | 52 | B6 (开发面板, P2) |
| utils/ | 16,949 | 80 | B6(数据路径部分归 B2) |
| dstu/ (适配器) | 11,743 | 41 | B2 |
| hooks/ | 7,251 | 35 | B1 |
| command-palette/ | 5,000 | 21 | B6 |
| stores/ | 4,618 | 14 | B1 |
| mcp-debug/+mcp/ | 9,723 | 19 | B6 |
| api/ | 4,169 | 13 | B2 |
| types/ | 3,264 | 18 | B2 |
| voice-input/ | 3,226 | 20 | B6 |
| essay-grading/ | 2,158 | 11 | B1 |
| services/ | 1,919 | 6 | B2 |
| 其余(app/contexts/shared/store/events/lib…) | ~4,700 | ~30 | B1/B2 |
| features: settings 31,571 / learning-hub 30,702 / mindmap 16,986 / notes 12,056 / pdf 3,253 / todo 1,996 / pomodoro 1,076 / sandbox 624 | — | B4/B5 |

## 数据流索引文件 (全量, grep 产物)

| 文件 | 行数 | 内容 |
|------|------|------|
| idx-commands.txt | 758 | 全部 `#[tauri::command]` 位置 |
| idx-emits.txt | 225 | 全部 Rust emit 调用点 |
| idx-invokes.txt | 904 | 全部前端 invoke 调用点 |
| idx-listens.txt | 102 | 全部前端 listen 调用点 |
| per-module-counts.txt | 27 | 每模块 命令/emit/SQL/序列化/行数 |

## 横切发现 (索引生成时即得)

1. **命令名重复**(invoke 按名解析 → 二义/隐患): `get_mcp_config`、`import_mcp_config`、`export_mcp_config`、`test_mcp_connection`、`test_mcp_http`、`test_mcp_sse`、`test_mcp_websocket`、`test_rmcp_streamable_http` 各出现 2 次(疑 cmd/mcp_commands.rs 与 mcp/ 各注册一份)→ A10 确认注册表实际行为
2. **SQL 密度异常**: vfs 1105 + database 455 = 60% SQL 点集中在数据层,A3 重点
3. **序列化密度异常**: chat_v2 182 个 serde to_* 点 = 34%,A1/A2 重点
4. **emit 与命令错位**: llm_manager 0 命令 42 emit(纯事件源);cmd/ 169 命令仅 6 emit——事件模型边界清晰,A11 按此分类
