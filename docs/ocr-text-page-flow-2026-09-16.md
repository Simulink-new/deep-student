# OCR 文本页面：设计流程 vs 实现对照（task-048）

> 日期： 2026-09-16 ｜ 触发： 用户报三症状——断点续传无进度条 / 预览均为空白 / 页面选择无反应
> 页面： `TextbookContentView`（教材内容视图，pdf/ocr/split 三模式）+ `FileContentView`（文件附件视图）

## 1. 设计流程（应有行为）

```
打开 PDF 节点
  ├─ usePdfLoader: pdfstream → base64 兜底加载字节;成功后自动 vfs_ensure_ocr_pipeline
  ├─ 挂载轮询 getPdfProcessingStatus → setFullStatus(重开处理中的文件能接续显示进度)
  └─ usePdfProcessingProgress: 监听 media-processing-* 事件 → 全局 store

OCR 流水线（后端 5 阶段,均有进度事件）
  page_rendering → page_compression → ocr_processing → vector_indexing → completed
  断点续跑: pages 已压缩/OCR 过 → 直接从 ocr_processing/vector_indexing 恢复

状态栏（renderOcrStatusBar）
  ⓪ 未开始(needsOcr) → "开始 OCR" 按钮
  0.5 pending/error → "重新 OCR 识别"(可自愈重试)
  ② 活动阶段 → 进度条 + 阶段文案 + 页码
  ③ completed + 有文本 → pdf/ocr/split 视图切换 + 重试

OCR 文本消费
  ocr/split 模式 → get_ocr_page_md(sourceId, page-1) 按页加载 MD
  页 MD 不可得 → 回退全量 vfs_get_resource_ocr_info 文本
  PDF 翻页 ⇄ OCR 页码同步（ocrPageSync)

页面选择
  悬停页角浮层 → togglePageSelection → selectedPages → 广播 Chat InputBar
```

## 2. 对照发现的缺口与修复

| # | 症状 | 根因（文件:行） | 修复 |
|---|------|----------------|------|
| R1 | 断点续传无进度条 | **核心根因** `pdfProcessingStore.shouldAcceptUpdate`:挂载轮询把 DB 的 `completed`(order 6）灌进 store,续跑事件 `ocr_processing`(4)/`vector_indexing`(5）序号更低被守卫拒绝 → 整条重跑期间 store 冻结在 completed。error 后重试同病 | 守卫放行「终态→活动态」回退（新一轮运行只可能来自用户重跑；后端 generation 守卫已过滤旧任务迟到事件）;`update()` 识别重跑后 percent/readyModes/error 从新一轮重新累计（否则 percent 被 Math.max 钉死在 100) |
| R2 | 索引期进度消失/按钮误点 | `isOcrProcessing` 两视图硬编码漏 `text_extraction`/`vector_indexing`/`image_compression`(TextbookContentView:646, FileContentView:167);350 页文件索引约 27s 期间被误判为「不在处理中」 | store 导出统一 `ACTIVE_STAGES`/`isActiveProcessingStage`,两视图改走共享集合 |
| R3 | OCR 文本预览空白 | **核心根因** `ocrDisplayContent` 回退逻辑反转（:1394):仅 pdf 模式回退全量文本;ocr/split 模式下页 MD 失败/为空 → content=null → 整页空白 | 页 MD 有正文→页 MD；仅标题（空白页）→标题+占位说明；页 MD 不可得→回退全量文本 |
| R4 | 同上 | `get_ocr_page_md` 对旧 schema/缺页返回 Err，前端静默 catch 为 null | 由 R3 的回退覆盖；不再无声 |
| R5 | PDF 页无声空白 | react-pdf `Page` 无 `onRenderError`：渲染失败的页永远停在骨架屏 | 逐页 `onRenderError` → 就地错误占位 + `write_debug_logs` 落盘（诊断包可见） |
| R6 | PDF 页区整体空白+选择按钮消失（症状2+3 的共同结构性诱因） | `.ds-pdf__content-viewport` 是 flex column;`.ds-pdf__pages-virtualizer` 内联高度（350页≈31万px）被默认 `flex-shrink:1` 压塌 → 绝对定位页行塌陷 | CSS 两虚拟列表显式 `flex-shrink: 0` |
| R7 | 空白无声、远程不可诊断 | viewport 接线断裂（OverlayScrollbars initialized 未回调）时无任何线索 | EnhancedPdfViewer 自诊断：文档加载 2s 后仍无滚动元素/0 行 → console.error + 写前端日志 |
| R8 | 页面选择/OCR 入口缺失 | TextbookContentView 未给 TextbookPdfViewer 传 `fileId` → 查看器内 OCR 横幅（扫描件提示+启动）永不显示；FileContentView 传 `node.id` 而非 `sourceId` → store 查询永远 miss（横幅常驻）且 ensure 用错 id | 两视图统一传 `node.sourceId` |

选择链路本身（toggle→onPageSelectionChange→setSelectedPages→广播）经逐层核阅无缺陷；
症状 3 由 R6（页行塌陷→按钮不存在）与 R8（fileId 断线）解释。

## 3. 附带修复

- `handleStartOcr`（两视图）：后端确认 `ocr_started`/`ocr_resumed` 后立即把 store 置为
  `{stage:'ocr_processing', percent:1}` —— 进度条即时出现，不等首个后端事件。
- EnhancedPdfViewer 补解构 `fileName` prop（此前声明未取出）。

## 4. 验证与后续

- `npx tsc --noEmit` 双绿。无 Rust 改动。
- 生效需重新打包（taskkill deep-student-fork.exe → npx tauri build）。
- 若 R6 非症状2/3 的全部成因，R5/R7 的日志会在下次运行的前端日志/诊断包
  （设置→关于→导出诊断包）中给出确切页码与原因。
- 遗留候选：mount 轮询在续跑窗口期可能短暂回写 DB 旧终态（事件随后纠正，可接受）;
  legacy `pdf_ocr_progress` 会话映射随旧服务退役可清理。
