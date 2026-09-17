export const PDF_OPTIONS = {
  cMapUrl: `${import.meta.env.BASE_URL}cmaps/`,
  cMapPacked: true,
  standardFontDataUrl: `${import.meta.env.BASE_URL}standard_fonts/`,
  wasmUrl: `${import.meta.env.BASE_URL}wasm/`,
  // ★ task-052: 直连 pdfstream 流式模式的关键参数——
  // disableStream: 初始探测请求只读响应头即 abort body,之后全部走 Range 分段,
  //   避免 pdf.js 把 175MB 整文件渐进拉进内存(后台白跑流量);
  // disableAutoFetch: 不在后台预取整本,大扫描书按需取页(首屏只取首屏+目录/xref 尾部)。
  disableStream: true,
  disableAutoFetch: true,
};
