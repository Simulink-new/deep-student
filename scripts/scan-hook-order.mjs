#!/usr/bin/env node
/**
 * scan-hook-order.mjs — React hooks 顺序违规静态扫描(防 minified react error #310)
 *
 * 背景(task-054): TextbookContentView 的 useMemo 位于 PDF 加载门闸 early return 之后,
 * 加载态/就绪态 hook 数量不一致 → 每个 PDF 打开必崩(#310 Rendered more hooks)。
 * 本脚本用 TypeScript AST 精确检测「组件/hook 顶层语句序列中,可能 return 的语句之后
 * 仍出现 hook 调用」的模式。
 *
 * 用法: node scripts/scan-hook-order.mjs   (退出码 = 违规数,CI 可接)
 */
import ts from 'typescript';
import fs from 'fs';
import path from 'path';

const roots = ['src/features', 'src/hooks', 'src/components'];
const files = [];
const walk = (dir) => {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p);
    else if (/\.(tsx|ts)$/.test(e.name)) files.push(p);
  }
};
roots.forEach(walk);

const HOOK_RE = /^(use[A-Z]|use$)/;

function containsHookCall(node, sourceText) {
  let hit = null;
  const visit = (n) => {
    if (hit) return;
    if (ts.isCallExpression(n)) {
      const expr = n.expression;
      const name = ts.isIdentifier(expr) ? expr.text
        : ts.isPropertyAccessExpression(expr) && ts.isIdentifier(expr.name) ? expr.name.text : '';
      if (HOOK_RE.test(name)) hit = name;
    }
    // 不深入嵌套函数——嵌套函数体内的调用不算顶层
    if (!ts.isFunctionLike(n) || ts.isCallExpression(n)) ts.forEachChild(n, visit);
  };
  ts.forEachChild(node, visit);
  return hit;
}

function mayReturn(stmt) {
  // 顶层 return / if(...return) / if/else 双分支均 return / try 块含 return 等
  let found = false;
  const visit = (n, depth) => {
    if (found) return;
    if (ts.isReturnStatement(n)) { found = true; return; }
    if (ts.isFunctionLike(n)) return; // 不进入嵌套函数
    ts.forEachChild(n, (c) => visit(c, depth + 1));
  };
  visit(stmt, 0);
  return found;
}

let total = 0;
for (const file of files) {
  const text = fs.readFileSync(file, 'utf8');
  const sf = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const checkBody = (body, label) => {
    if (!body || !ts.isBlock(body)) return;
    let returnLine = null;
    for (const stmt of body.statements) {
      const pos = sf.getLineAndCharacterOfPosition(stmt.getStart());
      if (returnLine === null && mayReturn(stmt)) {
        returnLine = pos.line + 1;
      } else if (returnLine !== null) {
        const hook = containsHookCall(stmt, text);
        if (hook) {
          console.log(`${file}:${pos.line + 1} [${label}] hook "${hook}" AFTER possible return at L${returnLine}`);
          total++;
        }
      }
    }
  };
  const visit = (n) => {
    // function 声明且名字大写开头 或 useXxx
    if (ts.isFunctionDeclaration(n) && n.name && /^([A-Z]|use[A-Z])/.test(n.name.text)) {
      checkBody(n.body, n.name.text);
    }
    // const X = (…) => / function —— 变量名大写或 useXxx
    if (ts.isVariableStatement(n)) {
      for (const decl of n.declarationList.declarations) {
        if (ts.isIdentifier(decl.name) && /^([A-Z]|use[A-Z])/.test(decl.name.text)
            && decl.initializer && ts.isFunctionLike(decl.initializer)) {
          checkBody(decl.initializer.body, decl.name.text);
        }
      }
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
}
console.log(`\nscan done: ${files.length} files, ${total} findings`);

process.exitCode = Math.min(total, 125);
