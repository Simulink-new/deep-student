/**
 * Token 估算工具函数 —— 前端唯一权威启发式实现
 *
 * 逐字符权重与 Rust 侧 src-tauri/src/utils/token_budget.rs 的 estimate_tokens
 * 完全对齐（ASCII 字母数字 0.25 / ASCII 空白 0.05 / ASCII 标点 0.30 /
 * CJK·假名·谚文 1.0 / 其它多字节 0.8），双端估算不再互相漂移。
 *
 * 历史教训：本仓库曾有 4 份互相漂移的 TS 估算（contextHelper 比例插值、
 * tokenUtils 中文计数、CardAgent 逐字符 0.25/0.5、SegmentEngine 英文词 1.3），
 * 2026-09-10 执行层审计后全部收敛到本文件。
 *
 * 需要精确计数时走后端 estimate_tokens IPC（tiktoken），
 * 本函数只用于 UI 即时预览/分段等同步场景。
 */

const isCjk = (cp: number): boolean =>
  // 中日韩统一表意及扩展块 + 兼容表意
  (cp >= 0x4e00 && cp <= 0x9fff) ||
  (cp >= 0x3400 && cp <= 0x4dbf) ||
  (cp >= 0x20000 && cp <= 0x2a6df) ||
  (cp >= 0x2a700 && cp <= 0x2b73f) ||
  (cp >= 0x2b740 && cp <= 0x2b81f) ||
  (cp >= 0x2b820 && cp <= 0x2ceaf) ||
  (cp >= 0x2ceb0 && cp <= 0x2ebef) ||
  (cp >= 0xf900 && cp <= 0xfaff);

const isKana = (cp: number): boolean =>
  (cp >= 0x3040 && cp <= 0x309f) || // 平假名
  (cp >= 0x30a0 && cp <= 0x30ff); // 片假名

const isHangul = (cp: number): boolean =>
  (cp >= 0xac00 && cp <= 0xd7af) || // 谚文音节
  (cp >= 0x1100 && cp <= 0x11ff) || // 谚文字母
  (cp >= 0xa960 && cp <= 0xa97f) || // 谚文字母扩展-A
  (cp >= 0xd7b0 && cp <= 0xd7ff); // 谚文字母扩展-B

/**
 * 前端 token 估算（与后端 token_budget.rs estimate_tokens 同公式）
 *
 * @param text - 需要估算的文本内容
 * @returns 估算的 token 数量（至少为 1，与后端一致）
 */
export function estimateTokenCount(text: string): number {
  if (!text || text.length === 0) {
    return 0;
  }
  let sum = 0;
  // 展开运算符按码点迭代（正确处理 emoji 等代理对，与 Rust chars() 等价）
  for (const ch of text) {
    const code = ch.codePointAt(0)!;
    if (code <= 0x7f) {
      if (/[a-z0-9]/i.test(ch)) {
        sum += 0.25;
      } else if (/\s/.test(ch)) {
        sum += 0.05;
      } else {
        sum += 0.3; // 标点/其它 ASCII
      }
      continue;
    }
    if (isCjk(code) || isKana(code) || isHangul(code)) {
      sum += 1;
    } else {
      sum += 0.8;
    }
  }
  return Math.max(1, Math.round(sum));
}
