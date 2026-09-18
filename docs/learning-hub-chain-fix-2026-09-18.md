# 学习资源库全链路可用性核查与修复 (task-053)

> 日期: 2026-09-18 | 基准提交: 70d14590 | 验证: `cargo check --lib` ✅ + `npx tsc --noEmit` ✅
> 方法: 6 个子代理并行核查(命令注册/侧栏/媒体与富文本/思维导图/OCR 与进度/引用与附件),
> 覆盖 Learning Hub 190 个命令与全部视图的数据触发链路。

## 核查结论总览

| 层 | 范围 | 结论 |
|----|------|------|
| 命令注册 | 190 个 learning-hub 相关 Tauri 命令 | 189 OK;1 个漏注册(`get_ocr_page_md`,已修) |
| 侧栏/文件夹 | 增删改移、回收站、收藏、搜索 | 全部通畅 |
| 媒体/富文档 | 图片/音频/视频/docx/xlsx/pptx 经 pdfstream:// | OK(task-051/052 回归通过) |
| 思维导图 | mindmap_repo CRUD + 快照 | OK |
| OCR/进度 | OCR 页 MD、阅读进度、预览自愈 | 1 死链 + 1 数据层空洞(已修) |
| 引用/附件 | vfsRefApi、blob 存储 | 1 个无后端死 wrapper(已删) |

## 修复清单

### B1 — `get_ocr_page_md` 命令漏注册 (src-tauri/src/lib.rs)
handler 早已存在于 `cmd/textbooks.rs:1583`,前端 `TextbookContentView.tsx:905` /
`NoteContentView.tsx:134` 按页 OCR Markdown 依赖它;漏注册 → "command not found"
(有全量文本回退,非致命,但按页视图失效)。已补注册。

### B2 — `dstu_set_metadata` 静默丢弃 (src-tauri/src/dstu/handlers/common.rs) ★核心
旧 `_` 兜底分支把 metadata 序列化后**从未写库**,且 log 措辞声称 "falling through
to metadata update" —— 实际翻译/作文/教材的保存全部静默丢失:

- **`translations`**: 此前 sourceText/translatedText/srcLang/tgtLang/qualityRating/
  isFavorite/formality/customPrompt 全丢。现分支:
  - 列字段(title/src_lang/tgt_lang/quality_rating 1-5 校验/is_favorite)直接 UPDATE;
  - formality/customPrompt 无独立列 → 并入 `translations.metadata_json`;
  - sourceText/translatedText → 合并进 `resources.data` JSON
    (经 `VfsResourceRepo::update_resource_data_with_conn`,自动重算 hash +
    置 index_state='pending')。
- **`essays`**: 经 `VfsEssayRepo::update_session` 持久化
  title/essayType/gradeLevel/customPrompt/isFavorite。
  **modeId 无 essay_sessions 列** → warn 日志明示不持久化(需 schema 加列,见遗留)。
- **`textbooks` | `files` | `images`**: `readingProgress.page` → `files.last_page`、
  isFavorite 经 `TextbooksDb::update_vfs` 持久化 —— 阅读进度跨会话恢复此前从未生效
  (只剩 sessionStorage 兜底)。
- `_` 兜底保留给未覆盖类型,日志改为明确 "NO metadata written" 并列出 keys。

### B2b — converter 读回闭环 (dstu/handler_utils/node_converters.rs)
写侧修好后读侧必须对称,否则闭环仍断:
- `translation_to_dstu_node`: 先铺 `metadata_json` 额外字段(formality/customPrompt),
  再由固定字段覆盖;
- `session_to_dstu_node`: 补发 `customPrompt`;
- `textbook_to_dstu_node`: 补发 `lastPage` + `readingProgress: {page}`
  (TextbookContentView 经 `node.metadata.readingProgress.page` 恢复页码)。

### B3 — 保存结果未检查 (前端 2 视图)
适配器返回 `Result<void, VfsError>` **不抛错**,旧代码 `await` 后不查 `result.ok`
就更新本地状态 → 后端失败时用户误以为保存成功:
- `TranslationContentView.tsx` handleSessionSave: 失败 → 错误通知 + throw,
  不 setSession;
- `EssayContentView.tsx` handleSessionSave: 失败 → 错误通知 + return,
  setSession 移到成功后。

### 数据层自愈 — preview blob 磁盘缺失 (vfs/pdf_processing_service.rs)
实测 708 个历史 JPEG 页图: blobs 表行还在(ref_count=1)但磁盘文件已丢。
`get_blob_path_with_conn` 只查行不查盘,页数又齐 → task-049 的截断自愈不触发。
现: 页数齐时逐页校验 blob 磁盘存在性,任一缺失(无行或文件不在)即全量重渲染 preview。

### 其他
- `pdf_protocol.rs` MIME 表补齐(bmp/m4v/mov/mkv/avi/mp3/m4a/aac/wav/ogg/opus/flac),
  消除媒体嗅探歧义;
- `types.ts`: epub 移出 text 组(zip 二进制按纯文本渲染只出乱码)→ 归 'none' 可下载;
- `vfsFileApi.ts`: 两处过时注释(4MB 截断/仅放行 .pdf)按 task-051/052 现状重写;
- `useTextbookCover.ts`: saveImageToImagesDir 桩返回 `{path:''}` 对象,
  旧代码模板字符串化得 "<appDir>/[object Object]" 污染封面 dataUrl —— 仅当拿到
  非空字符串路径才替换为文件 URL;
- 删除死代码 `updateResourceHashV2`/`vfs_update_resource_hash`(vfsRefApi.ts) ——
  无 Rust 后端实现且全库无调用方。

## 遗留问题(已记录,未修)

| 问题 | 说明 | 建议 |
|------|------|------|
| 1 个 PDF blob 磁盘缺失 | `att_s1E0cR9Nyb`(bbdc_24918498_20260521082323.pdf)blob 行在、文件丢 | 用户侧重新导入该附件 |
| 708 个旧 JPEG blob 缺失 | 已由本次 preview 自愈覆盖(引用方) | 后续可写一次性 blobs 表清扫 |
| HEIC 解码 | CSP `script-src 'self'`(无 unsafe-eval)拦死 libheif 的 `new Function` | 需后端转码 HEIC→JPEG |
| essay modeId | essay_sessions 无 mode 列,不持久化(已 warn) | schema 迁移加列 |
| 排序 UI 缺口 | finderStore.setSorting 无 UI 调用方 | 补工具栏排序菜单 |
| 收藏数硬编码 0 | LearningHubSidebar.tsx:2050 | 接真实统计 |
| 死链: legacy pdf_ocr | pdf_ocr_service.rs + start_pdf_ocr_backend + registerOcrSession 无活跃调用 | 后续清理 |
| 死组件 | LearningHubToolbar/ResourceGridView/LearningHubActionBar 无引用 | 后续清理 |
| dstu_watch no-op | 事件订阅桩 | 后续接线或删除 |
| 7 个死 systemApi 命令 | 前端 wrapper 无后端 | 后续清理 |

## 验证

- `cargo check --lib`: exit 0
- `npx tsc --noEmit`: exit 0
- 既有单测 6/6 通过(核查阶段基线)
- ⚠️ 需重新打包后人工回归(打包前 taskkill deep-student-fork.exe)
