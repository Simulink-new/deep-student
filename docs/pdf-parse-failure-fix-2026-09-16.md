# 后端 PDF 解析失败：日志调查与重新修复（task-049）

> 日期: 2026-09-16 ｜ 触发: 用户报「后端 PDF 解析失败」,要求查明与近期修复(task-045 强制重启流水线)的关系并重新修复
> 结论: **task-045 的 ensure 层修复被 pipeline 层一个存在性门控完全中和**,另发现 2026-06 旧版渲染 50 页上限遗留的大面积数据损坏

## 1. 日志证据链

日志: `%LOCALAPPDATA%\com.lanxia.deepstudent\logs\deep-student.log`(19:15 / 19:28 / 22:24 三个会话)

**主日志零 [ERROR]** —— 失败是「静默」的,这正是它活了三个月的原因。

关键日志序列(file_N7Bb7cQ9Ay, 413 页 PDF, 19:28:07 用户点「重新 OCR」):

```
[Textbooks] Found incomplete OCR checkpoint for file ..., resuming pipeline   ← ensure 正确识别半成品
[PdfProcessingService] Marked file ... for force OCR (manual trigger)          ← task-045 的 force 标记生效
[MediaProcessingService] Pipeline started ... from stage: OcrProcessing        ← 流水线重启
[PdfProcessingService] Pages already compressed ..., ready_modes: ["ocr", ...] ← ❌ OCR 阶段整个消失
[PdfProcessingService] Starting vector indexing                                ← 直接跳到索引
[resolve_indexable_pages] Extracted 4 pages from attachment res_njgMqssjRO     ← 413 页只索引出 4 页
```

## 2. 数据取证(vfs.db 只读查询)

| 文件 | 真实页数 | preview 页数 | OCR 页数 | completedAt | 损坏类型 |
|------|---------|-------------|---------|-------------|---------|
| file_N7Bb7cQ9Ay | 413 | 413 | **4**(idx 0,2,3,4) | `''` | 半成品检查点 |
| file_4uRQ-rxN7p | 350 | 350 | **186**(有洞) | `''` | 半成品检查点 |
| file_TUqsgNPub8 | 353 | **50** | 50 | 2026-06-05 ✓ | preview 截断 |
| file_fTyUaQtxgK | 226 | **50** | 50 | 2026-06-05 ✓ | preview 截断 |
| 其余 12 个文件 | 61~753 | **50** | 50 | 2026-06 上周 ✓ | preview 截断 |
| file_UcCnRegcYp 等 7 个 | =页数 | =页数 | 全量 | ✓ | 健康 |

**受损合计 16 个文件**:2 个半成品检查点 + 14 个「50 页截断 preview」(含 1 个 legacy 数组格式 att_ 附件)。

## 3. 根因(两条独立链路)

### R1: stage-3 门控纯存在性判断 —— task-045 修复被中和

`run_pdf_pipeline_internal`:

```rust
has_ocr = ocr_pages_json IS NOT NULL            // 存在即"有 OCR",不看内容
if start_stage <= OcrProcessing && !has_ocr {   // ❌ force_ocr 在门内才读取,开不了门
    // stage 3 OCR
}
```

- **半成品检查点**(OCR 到第 4 页被杀,completedAt=''):启动自愈(`recover_stuck_tasks` → `auto_resume_ocr_tasks`)与 ensure 续跑**都正确识别并重启流水线**,但门控 `!has_ocr` 把 stage 3 跳过 → 4/413 页被索引并标记 completed → 残缺永久固化。
- **用户手动「重新 OCR」**(task-045 加的 force 路径):`mark_force_ocr` 生效、流水线重启,但 `is_force_ocr` 在门控**内部**才参与 `should_run_pdf_ocr` 计算 —— 门都进不去。且 stage-3 的 `load_existing_ocr_results` 无条件加载检查点,即使进了门也不会「忽略已有结果」。

**这就是「解析失败和修复的关系」**: task-045 修了 ensure 层(重启+标记),pipeline 层门控让所有重启/续跑/强制请求全部空转。

### R2: 2026-06 旧版渲染 50 页上限遗留数据

14 个文件的 preview_json 只有 50 页(`PdfPreviewConfig.max_pages` 当时默认 50,2026-06 已改为 0=无限制,见 pdf_preview.rs:230 修复注释)。OCR 忠实处理了全部 50 个已渲染页 → 100% 成功 → completedAt 落库 → 一切「完成」,但 353 页的书只有前 50 页有文字。

## 4. 修复内容(全部后端,2 文件)

### pdf_processing_service.rs
1. **门控语义**:`has_ocr`(存在性)旁新增 `ocr_completed`(`json_extract(ocr_pages_json,'$.completedAt') != ''`);stage-3 门控改为 `(!ocr_completed || is_force_ocr)` —— 半成品自动续跑,force 真正重入。
2. **ready_modes**:初始与收尾两处 `"ocr"` 推送改用 `ocr_completed` —— 半成品不再冒充完成态。
3. **stage_ocr_processing**:force 时丢弃检查点全页重跑;非 force 保留断点续传。
4. **`regenerate_pdf_preview` helper**:从 stage-3 内联动态生成代码提取(语义保持:DB 缓存写失败传播 Err,渲染失败/无字节返回 Ok(None))。
5. **截断 preview 自愈**(压缩块内):缓存 preview 页数 < `page_count` → 全量重渲染 + `ocr_completed=false` 重开门控 → stage-3 以续跑模式只补缺失页。**注意**: `ocr_completed` 是外层变量不可遮蔽(本次修复中抓到一个 shadow 导致的死赋值,cargo warning 暴露)。

### textbooks.rs (vfs_ensure_ocr_pipeline)
6. **半成品续跑不再 `mark_force_ocr`**:恢复断点续传语义(旧实现与「用户强制重跑」共用标记,若 force 一律清检查点,续传会被自己的恢复逻辑毁灭)。
7. **is_completed 分支加截断检测**:preview 页数 < 真实页数 → 重启流水线(非 force,流水线内自愈),返回 `ocr_resumed`。没有这一步,14 个截断文件永远进不了流水线。
8. **OCR_DIAG 增强**:增加 `ocr_completed` / `preview_pages` 两个字段,本类故障日后在日志中直接可见。

## 5. 自愈路径(用户侧无需任何操作)

- **半成品 2 个**:打开文件 → ensure 识别 completedAt='' → 续跑补齐(其中 vector_indexing 卡死的 file_4uRQ-rxN7p 先由启动自愈 reset→pending)。
- **截断 14 个**:打开文件 → ensure 识别 preview 截断 → 重渲染全量页 → 压缩新页 → OCR 补缺 → 重建索引。
- **用户手动「重新 OCR」**:force → 全页重跑(task-045 的承诺至此兑现)。
- 全程有进度事件(压缩 5-20% → OCR 20-75% → 索引 75-95%),task-048 的前端修复保证进度条可见。
- 重渲染期间(大书约数分钟)无新事件,进度条停在 20% 属预期。

## 6. 验证

- `cargo check --lib` 双绿(0 error,两个改动文件 0 新 warning)。
- `npx tsc --noEmit` 通过(本任务无前端改动)。
- 生效需重新打包;打包后打开任一受损文件观察日志:`Stale preview detected` / `Regenerated full preview` / `Resuming OCR ... N remaining`。
