/**
 * 资源仓库拖拽入库共享链路
 *
 * 按扩展名自动分类：
 * - PDF / Office / 文本 → textbooks_add（教材/文档）
 * - 图片 → dstu_create image
 * - Markdown（可选）→ 笔记导入
 * - 音视频 / 其余支持类型 → dstu_create file
 *
 * 路径直传优先（task-040：附件走 vfs_upload_attachment_by_path，Rust 侧读盘，前端零字节搬运；Windows 中文/空格路径不再经 IPC 字节往返）。
 */

import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { textbookDstuAdapter } from '@/dstu/adapters/textbookDstuAdapter';
import { attachmentDstuAdapter } from '@/dstu/adapters/attachmentDstuAdapter';
import { dstu } from '@/dstu/api'; // task-040: 路径直传后按 sourceId 取回节点
import { notesDstuAdapter } from '@/dstu/adapters/notesDstuAdapter';
import type { DstuNode } from '@/dstu/types';
import { extractFileName, extractDisplayFileName } from '@/utils/fileManager';
import { pLimit } from '@/utils/concurrency';
import { debugLog } from '@/debug-panel/debugMasterSwitch';
import { partitionMarkdownNoteImports } from './dragDropRouting';
import type { ImportProgressState, ImportStage } from './components/ImportProgressModal';

export const DOCUMENT_EXTENSIONS = new Set([
  'pdf', 'docx', 'txt', 'md', 'markdown', 'html', 'htm',
  'xlsx', 'xls', 'xlsb', 'ods',
  'pptx', 'epub', 'rtf',
  'csv', 'json', 'xml',
]);

export const IMAGE_EXTENSIONS = new Set([
  'jpg', 'jpeg', 'png', 'gif', 'webp', 'svg', 'bmp', 'heic', 'heif',
]);

export const AUDIO_EXTENSIONS = new Set([
  'mp3', 'wav', 'ogg', 'm4a', 'flac', 'aac', 'wma', 'opus',
]);

export const VIDEO_EXTENSIONS = new Set([
  'mp4', 'webm', 'mov', 'avi', 'mkv', 'm4v', 'wmv', 'flv',
]);

export function getFileExtension(name: string): string {
  return (name.split('.').pop() || '').toLowerCase();
}

export function isSupportedResourceDropExtension(ext: string): boolean {
  return (
    DOCUMENT_EXTENSIONS.has(ext) ||
    IMAGE_EXTENSIONS.has(ext) ||
    AUDIO_EXTENSIONS.has(ext) ||
    VIDEO_EXTENSIONS.has(ext)
  );
}

export function classifyDroppedName(name: string): 'document' | 'image' | 'media' | 'unsupported' {
  const ext = getFileExtension(name);
  if (DOCUMENT_EXTENSIONS.has(ext)) return 'document';
  if (IMAGE_EXTENSIONS.has(ext)) return 'image';
  if (AUDIO_EXTENSIONS.has(ext) || VIDEO_EXTENSIONS.has(ext)) return 'media';
  return 'unsupported';
}

interface TextbookImportProgressPayload {
  file_name: string;
  stage: string;
  current_page?: number;
  total_pages?: number;
  progress: number;
  error?: string;
}

export interface ResourceDropImportOptions {
  paths?: string[];
  files?: File[];
  folderId?: string | null;
  markdownAsNotes?: boolean;
  onTextbookProgress?: (state: ImportProgressState) => void;
}

export interface ResourceDropImportResult {
  successCount: number;
  failedCount: number;
  firstImportedNode: DstuNode | null;
  unsupportedNames: string[];
}

function getNativeFilePath(file: File): string | undefined {
  const candidate = (file as File & { path?: string }).path;
  return typeof candidate === 'string' && candidate.trim() ? candidate : undefined;
}

// ★ task-040/A10#1: 旧 fileFromLocalPath 已删——read_file_bytes number[] 过 JSON → 前端组 File
// → fileToBase64 再过 JSON 的三段往返(10MB 图 ≈165MB 瞬时内存);改 vfs_upload_attachment_by_path
// 路径直传,Rust 侧直接读盘+进程内 base64,前端零字节搬运。File 对象路径仅保留给无原生路径的
// 浏览器拖入分支(unsupported 路径,如 webview 内拖拽)。
async function importAttachmentPaths(
  paths: string[],
  folderId: string | null,
): Promise<{ successCount: number; failedCount: number; firstNode: DstuNode | null }> {
  const limit = pLimit(3);
  const results = await Promise.all(
    paths.map((path) => limit(async () => {
      try {
        const kind = classifyDroppedName(extractFileName(path));
        const upload = await invoke<{ sourceId: string }>('vfs_upload_attachment_by_path', {
          params: {
            path,
            attachmentType: kind === 'image' ? 'image' : 'file',
            folderId: folderId ?? undefined,
          },
        });
        const node = await dstu.get(`/${upload.sourceId}`);
        return node.ok && node.value
          ? { ok: true as const, node: node.value }
          : { ok: false as const };
      } catch (error) {
        debugLog.error('[resourceDropImport] 附件导入失败(路径直传):', path, error);
        return { ok: false as const };
      }
    })),
  );

  let successCount = 0;
  let failedCount = 0;
  let firstNode: DstuNode | null = null;
  for (const result of results) {
    if (result.ok) {
      successCount += 1;
      firstNode ??= result.node;
    } else {
      failedCount += 1;
    }
  }
  return { successCount, failedCount, firstNode };
}

async function importMarkdownPathNotes(
  filePaths: string[],
  folderId: string | null,
): Promise<{ importedNodes: DstuNode[]; failedCount: number }> {
  if (filePaths.length === 0) {
    return { importedNodes: [], failedCount: 0 };
  }

  const result = await notesDstuAdapter.importMarkdownFiles(
    filePaths.map((filePath) => ({
      filePath,
      titleHint: extractDisplayFileName(filePath),
    })),
    folderId,
  );

  if (!result.ok) {
    return { importedNodes: [], failedCount: filePaths.length };
  }

  return {
    importedNodes: result.value.imported,
    failedCount: result.value.failed.length,
  };
}

async function importMarkdownFileObjects(
  files: File[],
  folderId: string | null,
): Promise<{ importedNodes: DstuNode[]; failedCount: number }> {
  if (files.length === 0) {
    return { importedNodes: [], failedCount: 0 };
  }

  const limit = pLimit(3);
  const results = await Promise.all(
    files.map((file) => limit(async () => {
      try {
        const content = await file.text();
        return notesDstuAdapter.importMarkdownContent(file.name, content, folderId);
      } catch {
        return { ok: false as const };
      }
    })),
  );

  const importedNodes: DstuNode[] = [];
  let failedCount = 0;
  for (const result of results) {
    if (result.ok) importedNodes.push(result.value);
    else failedCount += 1;
  }
  return { importedNodes, failedCount };
}

async function importAttachmentFiles(
  files: File[],
  folderId: string | null,
): Promise<{ successCount: number; failedCount: number; firstNode: DstuNode | null }> {
  const limit = pLimit(3);
  const results = await Promise.all(
    files.map((file) => limit(async () => {
      const kind = classifyDroppedName(file.name);
      const attachmentType = kind === 'image' ? 'image' : 'file';
      try {
        return await attachmentDstuAdapter.create(
          file,
          attachmentType,
          folderId ? { folderId } : undefined,
        );
      } catch (error) {
        debugLog.error('[resourceDropImport] 附件导入失败:', file.name, error);
        return { ok: false as const };
      }
    })),
  );

  let successCount = 0;
  let failedCount = 0;
  let firstNode: DstuNode | null = null;
  for (const result of results) {
    if (result.ok) {
      successCount += 1;
      firstNode ??= result.value;
    } else {
      failedCount += 1;
    }
  }
  return { successCount, failedCount, firstNode };
}

let importInFlight = false;

export function isResourceDropImporting(): boolean {
  return importInFlight;
}

const EMPTY_IMPORT_RESULT: ResourceDropImportResult = {
  successCount: 0,
  failedCount: 0,
  firstImportedNode: null,
  unsupportedNames: [],
};

/**
 * 将拖入的本地文件按类型分类并写入资源仓库。
 * 路径分支优先；无路径时回退到浏览器 File 对象。
 */
export async function importDroppedResources(
  options: ResourceDropImportOptions,
): Promise<ResourceDropImportResult> {
  const uniquePaths = [...new Set((options.paths ?? []).filter(Boolean))];
  const files = options.files ?? [];

  if (uniquePaths.length === 0 && files.length === 0) {
    return EMPTY_IMPORT_RESULT;
  }

  if (importInFlight) {
    return EMPTY_IMPORT_RESULT;
  }

  importInFlight = true;
  try {
    return await runDroppedResourceImport(options, uniquePaths, files);
  } finally {
    importInFlight = false;
  }
}

async function runDroppedResourceImport(
  options: ResourceDropImportOptions,
  uniquePaths: string[],
  files: File[],
): Promise<ResourceDropImportResult> {
  const folderId = options.folderId ?? null;
  const markdownAsNotes = options.markdownAsNotes ?? false;
  let unlisten: UnlistenFn | null = null;

  try {
    const unsupportedNames: string[] = [];
    let successCount = 0;
    let failedCount = 0;
    let firstImportedNode: DstuNode | null = null;

    if (uniquePaths.length > 0) {
      const docPaths: string[] = [];
      const imagePaths: string[] = [];
      const mediaPaths: string[] = [];

      for (const path of uniquePaths) {
        const name = extractFileName(path);
        const kind = classifyDroppedName(name);
        if (kind === 'document') docPaths.push(path);
        else if (kind === 'image') imagePaths.push(path);
        else if (kind === 'media') mediaPaths.push(path);
        else unsupportedNames.push(name);
      }

      const { markdownItems: markdownNotePaths, otherItems: textbookPaths } = partitionMarkdownNoteImports(
        docPaths,
        (path) => extractFileName(path),
        markdownAsNotes,
      );

      if (markdownNotePaths.length > 0) {
        const markdownResult = await importMarkdownPathNotes(markdownNotePaths, folderId);
        successCount += markdownResult.importedNodes.length;
        failedCount += markdownResult.failedCount;
        firstImportedNode = markdownResult.importedNodes[0] ?? firstImportedNode;
      }

      if (textbookPaths.length > 0) {
        const firstFileName = textbookPaths[0] ? extractDisplayFileName(textbookPaths[0]) : '';
        options.onTextbookProgress?.({
          isImporting: true,
          fileName: firstFileName,
          stage: 'hashing',
          progress: 0,
        });

        unlisten = await listen<TextbookImportProgressPayload>('textbook-import-progress', (event) => {
          const payload = event.payload;
          options.onTextbookProgress?.({
            isImporting: true,
            fileName: payload.file_name,
            stage: payload.stage as ImportStage,
            currentPage: payload.current_page,
            totalPages: payload.total_pages,
            progress: payload.progress,
            error: payload.error,
          });
        });

        const docResult = await textbookDstuAdapter.addTextbooks(textbookPaths, folderId);
        if (docResult.ok) {
          successCount += docResult.value.length;
          firstImportedNode = firstImportedNode ?? docResult.value[0] ?? null;
          if (docResult.value.length < textbookPaths.length) {
            failedCount += textbookPaths.length - docResult.value.length;
          }
        } else {
          failedCount += textbookPaths.length;
          debugLog.error('[resourceDropImport] 文档导入失败:', docResult.error.toUserMessage());
        }

        options.onTextbookProgress?.({
          isImporting: false,
          fileName: firstFileName,
          stage: docResult.ok ? 'done' : 'error',
          progress: docResult.ok ? 100 : 0,
        });
      }

      const attachmentPaths = [...imagePaths, ...mediaPaths];
      if (attachmentPaths.length > 0) {
        // ★ task-040: 路径直传导入(见 importAttachmentPaths 注释)
        const attachResult = await importAttachmentPaths(attachmentPaths, folderId);
        successCount += attachResult.successCount;
        failedCount += attachResult.failedCount;
        firstImportedNode = firstImportedNode ?? attachResult.firstNode;
      }

      return { successCount, failedCount, firstImportedNode, unsupportedNames };
    }

    const supportedFiles: File[] = [];
    const pathBackedFiles: string[] = [];
    for (const file of files) {
      const nativePath = getNativeFilePath(file);
      if (nativePath) {
        pathBackedFiles.push(nativePath);
        continue;
      }
      const kind = classifyDroppedName(file.name);
      if (kind === 'unsupported') {
        unsupportedNames.push(file.name);
      } else {
        supportedFiles.push(file);
      }
    }

    if (pathBackedFiles.length > 0) {
      const nested = await runDroppedResourceImport(
        {
          paths: pathBackedFiles,
          folderId,
          markdownAsNotes,
          onTextbookProgress: options.onTextbookProgress,
        },
        [...new Set(pathBackedFiles)],
        [],
      );
      successCount += nested.successCount;
      failedCount += nested.failedCount;
      firstImportedNode = firstImportedNode ?? nested.firstImportedNode;
      unsupportedNames.push(...nested.unsupportedNames);
    }

    const { markdownItems: markdownFiles, otherItems: attachmentFiles } = partitionMarkdownNoteImports(
      supportedFiles,
      (file) => file.name,
      markdownAsNotes,
    );

    if (markdownFiles.length > 0) {
      const markdownResult = await importMarkdownFileObjects(markdownFiles, folderId);
      successCount += markdownResult.importedNodes.length;
      failedCount += markdownResult.failedCount;
      firstImportedNode = markdownResult.importedNodes[0] ?? firstImportedNode;
    }

    if (attachmentFiles.length > 0) {
      const attachResult = await importAttachmentFiles(attachmentFiles, folderId);
      successCount += attachResult.successCount;
      failedCount += attachResult.failedCount;
      firstImportedNode = firstImportedNode ?? attachResult.firstNode;
    }

    return { successCount, failedCount, firstImportedNode, unsupportedNames };
  } finally {
    if (unlisten) {
      try { unlisten(); } catch { /* ignore */ }
    }
  }
}
