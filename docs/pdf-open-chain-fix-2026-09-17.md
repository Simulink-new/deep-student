# PDF 打开全链路根因修复(task-052)

> 日期: 2026-09-17 | 症状: task-051 重新打包后**全部** PDF 仍打不开 | 前置: task-048/049/050/051

## 证据链(21:54 会话日志取证)

| 时间 | 证据 | 结论 |
|------|------|------|
| 21:54:18-31 | 后端 5 次 `vfs_get_blob_pdfstream_url`(URL 已发往前端),OCR_DIAG 显示 task-051 的 `vfs_ensure_ocr_pipeline` 新分支被执行 | task-051 代码确实在运行 |
| 21:54 全session | 后端 `[pdfstream] raw_uri=...` **零条**(09-16 旧构建有 21 条) | PDF.js 从未发出任何网络请求 |
| 前端日志 | 无 `PDF_DOCUMENT_LOAD_ERROR/SUCCESS/TIMEOUT`(task-051 诊断一条未发) | `EnhancedPdfViewer` **从未挂载**(诊断代码在它内部) |
| global log | `EvalError`(CSP unsafe-eval)×2,url=`blob:...` line2 col1320381 | 见下方"红鲱鱼"排除 |

**红鲱鱼排除**: EvalError 定位到 bundle 第 144 行 132 万字符行 = **libheif**(HEIC 图片解码器 Emscripten 胶水,`new Function` 被 CSP 拦)。与 PDF 链路无关,时间戳在 PDF 打开之前(启动时)。HEIC 转换在 CSP 下不可用是另一个独立问题,本次不处理。

## 根因 1(task-051 引入的回归,P0)

`usePdfLoader` 直连流式模式下 `file=null, filePath='', streamUrl=有值`,但
`TextbookPdfViewer` 两处门闸仍只看 file/filePath:

- L305 挂载门闸 `{(file || (filePath && filePath.trim())) && ...}` → **EnhancedPdfViewer 永不挂载** → PDF.js 从未启动 → 零网络请求、零错误、永久空状态;
- L293 空状态门闸 `{!file && !filePath && !error && ...}` → 同时误显示「未加载教材」空状态。

**修复**: 两处门闸均加入 `streamUrl` 判断。

## 根因 2(潜伏,挂载修复后必现)

**CORS 响应头未暴露**: pdfstream 全部响应缺少 `Access-Control-Expose-Headers`。
跨源 fetch(tauri.localhost → pdfstream.localhost)下,PDF.js 读不到
`Accept-Ranges`/`Content-Range` → `validateRangeRequestCapabilities` 判定不支持分段
→ 退化为整读(且拿错长度)→ 必然失败。

**修复**: `CORS_EXPOSE_HEADERS` 常量 + 全部 7 处响应统一走 `with_cors_headers` 注入。

## 根因 3(设计级,task-035 的 4MB 截断前提错误)

旧设计: 无 Range 请求返回 `206 + 4MB 截断 + Content-Range`,假设"PDF.js 会从
Content-Range 读总长再发 Range 请求"。**实证(pdfjs-dist 5.4.296 源码):
`validateRangeRequestCapabilities` 只读 `Content-Length`** → PDF.js 会把文件当成
4MB,到错误的"文件尾"找 xref → Invalid PDF。跨源时(根因 2)甚至直接不可读。

**修复**:
- 后端: 无 Range 请求一律 `200 + 完整 Content-Length + 全量 body`(PDF.js 初始探测
  拿到头即 abort body;图片/媒体消费者本就需要完整 200;4MB 截断对 >4MB 图片同样是坏的);
- 前端 `PDF_OPTIONS`: 新增 `disableStream: true`(探测后纯 Range 模式)+
  `disableAutoFetch: true`(不后台预取整本,大扫描书按需取页)。

## 完整数据链(修复后)

```
点击文件 → TextbookContentView/FileContentView
  → usePdfLoader(preferStreamUrl)
    → invoke vfs_get_blob_pdfstream_url → convertFileSrc → http://pdfstream.localhost/...
    → setStreamUrl(不下载!) + vfs_ensure_ocr_pipeline(后台 OCR 自愈)
  → TextbookPdfViewer(门闸含 streamUrl ✅) → viewerUrl = streamUrl
  → EnhancedPdfViewer → react-pdf Document(file=url, options=PDF_OPTIONS)
  → pdf.js: 初始 GET(无 Range)→ 200 + 全量 Content-Length + Accept-Ranges(已暴露 ✅)
    → 头读到真实总长 → abort body(disableStream)
    → Range 分段取 xref/首屏页(64KB/块,跨源 CORS+Expose ✅)
  → 首屏渲染(大文件秒开)
```

## 验证

- `cargo check --lib` ✅ | `npx tsc --noEmit` ✅
- 依据 pdfjs-dist@5.4.296 源码逐行核对: 初始无 Range、长度取自 Content-Length、
  range  reader 不解析 Content-Range、disableStream 触发探测后 abort。

## 生效条件

前后端均有改动,**必须重新打包**(先 `taskkill /F /IM deep-student-fork.exe`)。

## 遗留(另行处理)

- HEIC 图片解码(libheif)在 CSP `script-src 'self'` 下因 `new Function` 被拦,
  启动时抛 EvalError——与 PDF 无关,需要时单独修(libheif 关 wasm eval 或调整加载方式)。
