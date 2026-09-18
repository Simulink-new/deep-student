# PDF 打开崩溃 React #310 修复 (task-054)

> 日期: 2026-09-18 | 基准: d4a04eb6 | 症状: 点开任意 PDF 一直显示
> `Minified React error #310; visit reactjs.org/docs/error-decoder.html?invariant=310`
> 验证: `npx tsc --noEmit` ✅ + `node scripts/scan-hook-order.mjs` 0 findings ✅

## 根因

React error #310 = **"Rendered more hooks than during the previous render."**
(同一组件实例相邻两次渲染的 hooks 数量不一致,React 直接中断渲染,由 ErrorBoundary 兜底显示)

**`TextbookContentViewInner`** (src/features/learning-hub/apps/views/TextbookContentView.tsx):

```
L996  if (isPdf && !effectiveFilePath && !pdfFile && !pdfStreamUrl) { ... return ... }  // 加载/错误门闸
...
L1420 const ocrDisplayContent = useMemo(...)   // ← 门闸之后还有 hook!(task-048 引入)
```

每次点开 PDF 的必然路径:

1. 第 1 次渲染: `pdfStreamUrl=null, pdfLoading=true` → 命中门闸 early return → 只渲染到 L990 的 hooks;
2. 流式 URL 就绪 → 第 2 次渲染越过门闸 → 执行 L1420 的 useMemo → **hooks 数量 N → N+1 → #310**。

即「每个 PDF 必崩」。task-052 修通挂载门闸后 viewer 真正开始渲染,此潜伏 bug 才暴露。

## 修复

将 `ocrDisplayContent` useMemo **上移至所有 early return 之前**(紧随最后一个 hook),
依赖 `ocrPageMd/ocrTextContent/t` 在该点均可用,行为等价。

## 同类隐患排查(AST 扫描器)

为此写了精确扫描器 `scripts/scan-hook-order.mjs`(TypeScript AST,检测组件/hook 顶层
「可能 return 的语句之后仍有 hook 调用」),全库 999 个 ts/tsx 文件共发现 **6 处**,
已全部修复:

| 文件 | 违规 | 修法 |
|------|------|------|
| learning-hub/apps/views/TextbookContentView.tsx | useMemo 在 PDF 门闸后 | 上移 useMemo |
| chat/plugins/modes/components/PageNavigator.tsx | 4 hooks 在 `!modeState→null` 门闸后 | 门闸字段改可选链派生(`?.`+`??`),hooks 上移 |
| chat/plugins/modes/components/OcrResultHeader.tsx | 2 useCallback 在模式门闸后 | 上移 |
| chat/components/Variant/VariantActions.tsx | 3 useCallback 在「无可用操作」门闸后 | 门闸下移 |
| chat/components/panels/UnifiedSourcePanel.tsx | useCallback+useEffect 在 `!groups.length` 门闸后 | 门闸下移 |
| chat/components/BlockRenderer.tsx | useMemo 在 pending/SOURCE 双门闸后 | 上移 |

后 5 处是潜伏弹:门闸条件翻转(modeState 就绪/来源列表空↔非空/变体状态变化)即触发
同款 #310 崩溃。

## 回归防护

`node scripts/scan-hook-order.mjs` — 退出码=违规数,可接 CI。
当前全库 0 findings。

## 验证

- `npx tsc --noEmit`: exit 0
- `node scripts/scan-hook-order.mjs`: 999 files, 0 findings
- ⚠️ 打包生效需先 `taskkill /F /IM deep-student-fork.exe`
