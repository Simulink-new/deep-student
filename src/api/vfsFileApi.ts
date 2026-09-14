import { invoke, convertFileSrc } from '@tauri-apps/api/core';

export type FileType = 'document' | 'image' | 'audio' | 'video';

export interface VfsFile {
  id: string;
  resourceId?: string;
  blobHash?: string;
  sha256: string;
  fileName: string;
  originalPath?: string;
  size: number;
  pageCount?: number;
  fileType: FileType;
  mimeType?: string;
  tags: string[];
  isFavorite: boolean;
  lastOpenedAt?: string;
  lastPage?: number;
  bookmarks: unknown[];
  coverKey?: string;
  extractedText?: string;
  previewJson?: string;
  ocrPagesJson?: string;
  description?: string;
  status: string;
  createdAt: string;
  updatedAt: string;
  deletedAt?: string;
}

export interface UploadFileParams {
  name: string;
  mimeType: string;
  base64Content: string;
  fileType?: FileType;
  folderId?: string;
}

/**
 * ★ 2026-01 新增：OCR 处理状态
 */
export interface OcrStatus {
  /** OCR 是否被执行 */
  performed: boolean;
  /** 跳过原因（如果跳过） */
  skipReason?: string;
  /** 成功的页数（PDF） */
  successCount: number;
  /** 失败的页数（PDF） */
  failedCount: number;
  /** Blob 缺失的页数（PDF） */
  blobMissingCount: number;
  /** 总页数（PDF） */
  totalPages: number;
  /** 是否全部成功 */
  allSuccess: boolean;
  /** 用户可见的状态消息 */
  message: string;
}

/**
 * ★ 2026-01 新增：索引处理状态
 */
export interface IndexStatus {
  /** 是否已加入索引队列 */
  queued: boolean;
  /** 创建的索引单元数量 */
  unitsCreated: number;
  /** 用户可见的状态消息 */
  message: string;
}

export interface UploadFileResult {
  file: VfsFile;
  sourceId: string;
  resourceHash: string;
  isNew: boolean;
  /** ★ 2026-01 新增：OCR 处理状态 */
  ocrStatus?: OcrStatus;
  /** ★ 2026-01 新增：索引处理状态 */
  indexStatus?: IndexStatus;
}

export interface FileContentResult {
  content: string | null;
  found: boolean;
}

export const vfsFileApi = {
  async upload(params: UploadFileParams): Promise<UploadFileResult> {
    return invoke('vfs_upload_file', { params });
  },

  async get(fileId: string): Promise<VfsFile | null> {
    return invoke('vfs_get_file', { fileId });
  },

  async list(options?: {
    fileType?: FileType;
    limit?: number;
    offset?: number;
  }): Promise<VfsFile[]> {
    return invoke('vfs_list_files', {
      fileType: options?.fileType,
      limit: options?.limit,
      offset: options?.offset,
    });
  },

  async delete(fileId: string): Promise<void> {
    return invoke('vfs_delete_file', { fileId });
  },

  async getContent(fileId: string): Promise<FileContentResult> {
    return invoke('vfs_get_file_content', { fileId });
  },

  /**
   * 更新文件书签
   * @param fileId 文件 ID（textbook ID）
   * @param bookmarks 书签数组
   */
  async updateBookmarks(fileId: string, bookmarks: Bookmark[]): Promise<boolean> {
    return invoke('textbooks_update_bookmarks', { id: fileId, bookmarks });
  },
};

/** PDF 书签类型 */
export interface Bookmark {
  id: string;
  page: number;
  title: string;
  createdAt: number;
}

export function inferFileType(mimeType: string): FileType {
  if (mimeType.startsWith('image/')) return 'image';
  if (mimeType.startsWith('audio/')) return 'audio';
  if (mimeType.startsWith('video/')) return 'video';
  return 'document';
}

// ============================================================================
// pdfstream:// 协议流式加载（perf task-035/A4#1）
// 替代 vfs_get_attachment_content 的整文件 base64 IPC 拷贝：
// 后端命令返回 blob 绝对路径，前端 convertFileSrc 转为协议 URL 后由 WebView
// 直接经自定义协议读取（支持 HTTP Range，协议带 CORS 头）。
// ============================================================================

/**
 * 获取文件 blob 的 pdfstream:// 协议 URL。
 *
 * @param fileId 文件 ID（与旧 attachmentId 同值；支持 file_/tb_/att_/img_ 前缀）
 * @returns 协议 URL；无 blob 存储、ID 不存在或查询失败时返回 null（调用方应回退 base64 路径）
 */
export async function getBlobStreamUrl(fileId: string): Promise<string | null> {
  try {
    const blobPath = await invoke<string | null>('vfs_get_blob_pdfstream_url', { fileId });
    if (!blobPath) return null;
    return convertFileSrc(blobPath, 'pdfstream');
  } catch {
    return null;
  }
}

/**
 * 通过 pdfstream:// URL 发起完整内容的流式请求，返回 Response（供调用方按需取字节/大小）。
 *
 * 注意：协议对无 Range 头的 GET 有 4MB 截断（返回 206 首段），
 * 因此这里显式携带 `Range: bytes=0-` 以一次性取回完整内容；
 * 协议的 CORS 配置（Access-Control-Allow-Headers: Range）已放行该请求头。
 *
 * @returns 可用的 Response（状态 200/206，headers 含完整 Content-Length）；
 *          请求失败或被协议拒绝（如非 .pdf 扩展名 403）时返回 null
 */
export async function fetchBlobStreamResponse(
  url: string,
  signal?: AbortSignal
): Promise<Response | null> {
  try {
    const resp = await fetch(url, { headers: { Range: 'bytes=0-' }, signal });
    if (!resp.ok && resp.status !== 206) return null;
    return resp;
  } catch {
    return null;
  }
}

/**
 * 探测 pdfstream:// URL 是否可被 <img>/<audio>/<video> 等元素直接消费。
 * 以 `Range: bytes=0-0` 取 1 字节探测（协议目前仅放行 .pdf 扩展名，
 * 非 PDF blob 会 403，探测失败即由调用方回退 base64 路径）。
 *
 * @returns 可用时返回原 URL，否则返回 null
 */
export async function probeBlobStreamUrl(url: string): Promise<string | null> {
  try {
    const resp = await fetch(url, { headers: { Range: 'bytes=0-0' } });
    if (!resp.ok && resp.status !== 206) return null;
    // 丢弃 1 字节探测体
    await resp.arrayBuffer().catch(() => undefined);
    return url;
  } catch {
    return null;
  }
}
