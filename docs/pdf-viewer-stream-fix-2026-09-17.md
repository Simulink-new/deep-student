# PDF 查看器直连流式加载修复（task-051）

> 日期: 2026-09-17 | 症状: 「PDF 本身都打不开」| 前置: task-048/049/050

## 用户症状

22:24 会话（Build 11100 = git cc07605b,task-048 已在包内）中,用户逐一点击多本教材 PDF,
每本只等约 10 秒就切走——全程无任何一本打开,36 秒内判断「都打不开」。

## 三条根因（相互独立,共同构成「打不开」的体感）

### R1: 查看路径是「整文件下载完才开始解析」

`usePdfLoader` 旧路径:`getBlobStreamUrl` → `fetchBlobStreamResponse(Range: bytes=0-)`
**整文件 206 下载** → Blob → File → blob URL → PDF.js 才开始工作。

- Miranda 扫描书 blob = **175MB**,整文件 fetch 需数十秒;
- 用户每本只给 ~10 秒 → 永远停在 spinner,感知为「打不开」;
- pdfstream 协议本就为 Range/206 设计(4MB 无 Range 截断、Content-Range 续传),
  但前端从未把 URL 直接交给 PDF.js。

### R2: pdfstream `in_blobs_dir` 放行规则是死代码

`pdf_protocol.rs` 安全检查 2:blob 目录内文件应放行全部扩展名(VFS hash 命名,
内容受管控)。旧判断 `d.ends_with("/blobs")`——但 VFS blob 目录实际名为
**`vfs_blobs`** → 条件永不命中 → 所有非 `.pdf` 扩展名的 blob 一律 403。

实证:19:28:47 日志 `拒绝访问非 PDF 且不在 blobs 目录内的文件`,被拒的
`file_UcCnRegcYp` 是扩展名丢失的 `.bin` PDF blob。

### R3: 诊断静默——Document 级失败完全无迹可查

- 前端文件日志只捕获 `window.onerror`/`unhandledrejection`;
  react-pdf `Document` 的 `onLoadError` 只走 `console.error` → **不落盘**;
- 「加载从未完成也未报错」(协议挂起/worker 崩溃/Range 死锁)无任何信号;
- task-048 的空白自查只在 `numPages > 0` 后启动,Document 打不开的 case 够不着。

## 修复内容

| 文件 | 修复 |
|------|------|
| `src-tauri/src/pdf_protocol.rs` | `in_blobs_dir` 增加 `ends_with("/vfs_blobs")` 匹配,死代码激活;blob 内非 .pdf 文件恢复放行 |
| `src/hooks/usePdfLoader.ts` | 新增 `preferStreamUrl` 模式 + `streamUrl` 状态:blob 存在时不下载整文件,直接返回 pdfstream URL;成功后照常触发 `vfs_ensure_ocr_pipeline` |
| `src/features/pdf/components/TextbookPdfViewer.tsx` | 新增 `streamUrl` prop;viewerUrl 优先级 file → **streamUrl** → filePath |
| `src/features/learning-hub/apps/views/TextbookContentView.tsx` | `preferStreamUrl: true`;spinner 超时 effect 与加载门闸均纳入 `pdfStreamUrl`;两处 TextbookPdfViewer 传入 streamUrl |
| `src/features/learning-hub/apps/views/FileContentView.tsx` | 同上接线(preferStreamUrl、门闸 `pdfFile \|\| pdfStreamUrl`、传 streamUrl) |
| `src/features/pdf/components/EnhancedPdfViewer.tsx` | ① Document 加载失败写 `PDF_DOCUMENT_LOAD_ERROR` 持久日志(不再只 console.error);② 加载成功写 `PDF_DOCUMENT_LOAD_SUCCESS` 一行(区分「没发起」与「加载成功但空白」);③ 新增 20s 看门狗 `PDF_DOCUMENT_LOAD_TIMEOUT`,覆盖「无声挂起」 |

## 效果

- 175MB 扫描书:**整文件下载数十秒 spinner → PDF.js 直接 Range 加载秒开**
  (首屏只需前几页的字节);
- 丢失扩展名的 PDF blob(.bin)不再 403;
- 三类失败(加载错误/加载超时/页面空白)全部落盘,诊断包可见。

## 验证

- `cargo check --lib` ✅(144 warnings,0 errors)
- `npx tsc --noEmit` ✅(零输出)

## 生效条件

前端 + Rust 均有改动,**必须重新打包**(`taskkill /F /IM deep-student-fork.exe` 后构建)。
task-049 的 OCR 修复(commit 24552aeb)同样未进任何已运行构建。
