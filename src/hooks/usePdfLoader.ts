/**
 * usePdfLoader - 统一的 PDF 文件加载 Hook
 * 
 * 解决的问题：
 * 1. 避免 TextbookContentView 和 FileContentView 中的重复代码
 * 2. 添加请求去重/缓存机制
 * 3. 大文件加载警告
 * 4. 统一的错误处理
 */

import { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getBlobStreamUrl, fetchBlobStreamResponse } from '@/api/vfsFileApi';
import { base64ToFile, estimateBase64Size, LARGE_FILE_THRESHOLD } from '@/utils/base64FileUtils';
import { debugLog } from '@/debug-panel/debugMasterSwitch';
import { classifyPdfError } from '@/features/pdf/types/pdfErrors';
import i18n from '@/i18n';

// 简单的内存缓存，避免重复加载同一文件（真正的 LRU + 内存大小限制）
const pdfCache = new Map<string, File>();
const MAX_CACHE_SIZE = 5; // 最多缓存 5 个文件
let pdfCacheTotalSize = 0;
const MAX_CACHE_BYTES = 100 * 1024 * 1024; // 100MB total limit
const LARGE_FILE_HINT_THRESHOLD = 10 * 1024 * 1024;

const formatBytes = (bytes: number): string => {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let size = bytes;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }
  return `${size.toFixed(unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
};

/**
 * 清理缓存（LRU 策略 + 内存大小限制）
 */
function cleanCacheIfNeeded(newFileSize: number) {
  while (pdfCache.size >= MAX_CACHE_SIZE || pdfCacheTotalSize + newFileSize > MAX_CACHE_BYTES) {
    if (pdfCache.size === 0) break;
    const firstKey = pdfCache.keys().next().value;
    if (firstKey) {
      const evicted = pdfCache.get(firstKey);
      if (evicted) pdfCacheTotalSize -= evicted.size;
      pdfCache.delete(firstKey);
    }
  }
}

/**
 * PDF 加载状态
 */
export interface PdfLoaderState {
  /** PDF File 对象 */
  file: File | null;
  /** 是否正在加载 */
  loading: boolean;
  /** 错误信息 */
  error: string | null;
  /** 是否为大文件（>10MB） */
  isLargeFile: boolean;
  /** 文件大小（字节） */
  fileSize: number;
  /** ★ task-051: 直连流式 URL(pdfstream://)——preferStreamUrl 模式下不下载整文件,
   *  把 URL 直接交给 PDF.js 原生 Range 加载(协议本身支持 Range/206)。
   *  大文件(百 MB 级扫描书)首屏从「整文件下载数十秒」降为「秒开」。 */
  streamUrl: string | null;
  /** 重试加载 */
  retry: () => void;
  /** 当前重试次数（0=未重试） */
  retryAttempt: number;
  /** 重置重试状态 */
  resetRetry: () => void;
}

/**
 * PDF 加载 Hook 参数
 */
export interface UsePdfLoaderOptions {
  /** 节点 ID（用于从数据库加载） */
  nodeId: string;
  /** 文件名 */
  fileName: string;
  /** 本地文件路径（可选，优先使用） */
  filePath?: string;
  /** 缓存 Key（用于内容更新时失效） */
  cacheKey?: string;
  /** 是否启用（用于条件加载） */
  enabled?: boolean;
  /** 原始文件路径（用于第二次重试时触发处理流水线） */
  originalPath?: string;
  /** 大文件熔断时回调：通知父组件切换为 pdfstream:// 协议加载 */
  onNeedStreamFallback?: (originalPath: string) => void;
  /** ★ task-051: 直连流式模式——有 blob 时不下载整文件,
   *  返回 streamUrl 供 PDF.js 原生 Range 加载。教科书/文件查看器应开启。 */
  preferStreamUrl?: boolean;
}

/**
 * 统一的 PDF 文件加载 Hook
 * 
 * 优先使用 filePath 加载本地文件，否则从数据库加载
 */
export function usePdfLoader({
  nodeId,
  fileName,
  filePath,
  cacheKey,
  enabled = true,
  originalPath,
  onNeedStreamFallback,
  preferStreamUrl = false,
}: UsePdfLoaderOptions): PdfLoaderState {
  const [file, setFile] = useState<File | null>(null);
  const [streamUrl, setStreamUrl] = useState<string | null>(null);
  const [retryAttempt, setRetryAttempt] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [isLargeFile, setIsLargeFile] = useState(false);
  const [fileSize, setFileSize] = useState(0);
  
  // 追踪当前加载请求，用于取消
  const abortControllerRef = useRef<AbortController | null>(null);
  const requestIdRef = useRef(0);
  // 追踪上一次加载的 cacheKey，避免重复加载
  const lastLoadedKeyRef = useRef<string | null>(null);
  // ★ 用 ref 追踪当前 file，避免 useCallback 依赖循环
  const fileRef = useRef<File | null>(null);

  // 从缓存获取或加载
  const loadPdf = useCallback(async () => {
    const resolvedCacheKey = cacheKey || nodeId;
    const requestId = ++requestIdRef.current;

    // 取消之前的请求（必须在任何早返回之前执行）
    if (abortControllerRef.current) {
      abortControllerRef.current.abort();
    }
    const controller = new AbortController();
    abortControllerRef.current = controller;

    // 如果有 filePath，不需要从数据库加载
    if (filePath) {
      abortControllerRef.current = null;
      setFile(null);
      setStreamUrl(null);
      setLoading(false);
      setError(null);
      setIsLargeFile(false);
      setFileSize(0);
      return;
    }
    
    // 检查缓存
    const cacheStorageKey = `pdf_${resolvedCacheKey}`;
    const cached = pdfCache.get(cacheStorageKey);
    if (cached) {
      // LRU: move to end
      pdfCache.delete(cacheStorageKey);
      pdfCache.set(cacheStorageKey, cached);
      debugLog.log('[usePdfLoader] Using cached file for:', resolvedCacheKey);
      setFile(cached);
      setLoading(false);
      setError(null);
      setFileSize(cached.size);
      setIsLargeFile(cached.size > LARGE_FILE_HINT_THRESHOLD);
      return;
    }
    
    // 避免重复加载
    if (lastLoadedKeyRef.current === resolvedCacheKey && fileRef.current) {
      return;
    }
    
    setLoading(true);
    setError(null);
    lastLoadedKeyRef.current = resolvedCacheKey;
    
    try {
      debugLog.log('[usePdfLoader] Loading PDF from database for:', resolvedCacheKey);

      // ★ task-035/A4#1: 优先 pdfstream:// 协议流式加载（免整文件 base64 过 IPC），
      //   无 blob / 协议拒绝 / 网络失败时回退下方 base64 路径
      const streamUrl = await getBlobStreamUrl(nodeId);
      if (controller.signal.aborted || requestId !== requestIdRef.current) {
        return;
      }

      if (streamUrl) {
        // ★ task-051: 直连流式模式——不下载整文件,把 pdfstream URL 交给
        // PDF.js 原生 Range 加载(pdfstream 协议本就为 Range/206 设计)。
        // 旧路径对 175MB 级扫描书会先整文件下载数十秒再解析,用户感知为「打不开」。
        if (preferStreamUrl) {
          debugLog.log('[usePdfLoader] Direct stream mode, PDF.js will range-load:', resolvedCacheKey);
          setStreamUrl(streamUrl);
          setFile(null);
          setLoading(false);
          setError(null);
          setRetryAttempt(0);
          // 与下载路径一致:成功获得可读源后触发 OCR 自愈/续跑
          invoke('vfs_ensure_ocr_pipeline', { fileId: nodeId }).catch((ocrErr: unknown) => {
            debugLog.warn('[usePdfLoader] OCR pipeline trigger failed (non-fatal):', ocrErr);
          });
          return;
        }
        try {
          const streamResp = await fetchBlobStreamResponse(streamUrl, controller.signal);
          if (controller.signal.aborted || requestId !== requestIdRef.current) {
            streamResp?.body?.cancel().catch(() => undefined);
            return;
          }

          if (streamResp) {
            // 大文件熔断：大小时机与 base64 路径一致（阈值/提示/流式回调均不变），
            // 优先用 CORS 安全列表内的 Content-Length（Range 全量请求下即完整大小）在消费 body 前熔断
            const applyLargeFileBreaker = (sizeBytes: number): boolean => {
              setFileSize(sizeBytes);
              const isLarge = sizeBytes > LARGE_FILE_HINT_THRESHOLD;
              setIsLargeFile(isLarge);
              if (sizeBytes > LARGE_FILE_THRESHOLD) {
                streamResp.body?.cancel().catch(() => undefined);
                if (originalPath && onNeedStreamFallback) {
                  debugLog.warn('[usePdfLoader] File exceeds base64 threshold, falling back to pdfstream:', originalPath);
                  onNeedStreamFallback(originalPath);
                } else {
                  debugLog.warn('[usePdfLoader] Large file detected:', sizeBytes, 'bytes');
                  setError(
                    `${i18n.t('learningHub:file.previewTooLarge', { defaultValue: 'File is too large to preview' })} (${formatBytes(sizeBytes)})`
                  );
                }
                setLoading(false);
                return true;
              }
              if (isLarge) {
                debugLog.warn('[usePdfLoader] Large file detected:', sizeBytes, 'bytes');
              }
              return false;
            };

            const declaredSize = Number(streamResp.headers.get('Content-Length'));
            const knownSize =
              Number.isFinite(declaredSize) && declaredSize > 0 ? declaredSize : null;

            if (knownSize !== null && applyLargeFileBreaker(knownSize)) {
              return;
            }

            const streamBlob = await streamResp.blob();
            if (controller.signal.aborted || requestId !== requestIdRef.current) {
              return;
            }

            if (knownSize === null && applyLargeFileBreaker(streamBlob.size)) {
              return;
            }

            const streamFile = new File([streamBlob], fileName, { type: 'application/pdf' });

            // 缓存文件（与 base64 路径同一缓存/键）
            cleanCacheIfNeeded(streamFile.size);
            pdfCache.set(cacheStorageKey, streamFile);
            pdfCacheTotalSize += streamFile.size;

            fileRef.current = streamFile;
            setFile(streamFile);
            setLoading(false);
            setRetryAttempt(0);

            // 成功加载后自动触发 OCR 流水线（与 base64 路径一致）
            invoke('vfs_ensure_ocr_pipeline', { fileId: nodeId }).catch((ocrErr: unknown) => {
              debugLog.warn('[usePdfLoader] OCR pipeline trigger failed (non-fatal):', ocrErr);
            });
            debugLog.log('[usePdfLoader] Loaded PDF via pdfstream:', resolvedCacheKey, streamFile.size, 'bytes');
            return; // 流式加载成功，无需再走 base64
          }
          // streamResp 为 null（协议 403/404/请求失败）→ 回退 base64
        } catch (streamErr: unknown) {
          if (controller.signal.aborted || requestId !== requestIdRef.current) {
            return;
          }
          debugLog.warn('[usePdfLoader] pdfstream load failed, falling back to base64:', streamErr);
        }
      }

      const result = await invoke<{ content: string | null; found: boolean }>('vfs_get_attachment_content', {
        attachmentId: nodeId,
      });
      
      // 检查是否被取消
      if (controller.signal.aborted || requestId !== requestIdRef.current) {
        return;
      }
      
      if (result?.found && result?.content) {
        // 检查是否为大文件
        const estimatedSize = estimateBase64Size(result.content);
        setFileSize(estimatedSize);
        const isLarge = estimatedSize > LARGE_FILE_HINT_THRESHOLD;
        setIsLargeFile(isLarge);

        if (isLarge) {
          debugLog.warn('[usePdfLoader] Large file detected:', estimatedSize, 'bytes');
        }

        // 在 base64->Uint8Array 转换前熔断，避免大文件解码导致内存峰值过高
        if (estimatedSize > LARGE_FILE_THRESHOLD) {
          // 有 originalPath 且定义了流式回调节时，切换为 pdfstream:// 协议加载
          if (originalPath && onNeedStreamFallback) {
            debugLog.warn('[usePdfLoader] File exceeds base64 threshold, falling back to pdfstream:', originalPath);
            onNeedStreamFallback(originalPath);
            setLoading(false);
            return;
          }
          setError(
            `${i18n.t('learningHub:file.previewTooLarge', { defaultValue: 'File is too large to preview' })} (${formatBytes(estimatedSize)})`
          );
          setLoading(false);
          return;
        }
        
        // 转换 base64 为 File
        const conversionResult = base64ToFile(result.content, fileName, 'application/pdf');
        
        if (conversionResult.success && conversionResult.file) {
          // 缓存文件
          cleanCacheIfNeeded(conversionResult.file.size);
          pdfCache.set(cacheStorageKey, conversionResult.file);
          pdfCacheTotalSize += conversionResult.file.size;
          
          fileRef.current = conversionResult.file;
          setFile(conversionResult.file);
          setLoading(false);
          setRetryAttempt(0);

          // 成功加载后自动触发 OCR 流水线（扫描文档时后端会处理）
          invoke('vfs_ensure_ocr_pipeline', { fileId: nodeId }).catch((ocrErr: unknown) => {
            debugLog.warn('[usePdfLoader] OCR pipeline trigger failed (non-fatal):', ocrErr);
          });
        } else {
          setError(conversionResult.error || i18n.t('pdf:errors.conversion_failed', { defaultValue: 'File format conversion failed' }));
          setLoading(false);
        }
      } else {
        setError(i18n.t('pdf:errors.content_not_found', { defaultValue: 'Unable to load PDF file content (id: {{id}})', id: nodeId }));
        setLoading(false);
      }
    } catch (err: unknown) {
      // 检查是否被取消
      if (controller.signal.aborted || requestId !== requestIdRef.current) {
        return;
      }
      
      debugLog.error('[usePdfLoader] Failed to load PDF:', err);
      setError(err instanceof Error ? err.message : i18n.t('pdf:errors.load_pdf_failed', { defaultValue: 'Failed to load PDF' }));
      setLoading(false);
    }
  }, [nodeId, fileName, filePath, cacheKey]);

  // 当参数变化时加载
  useEffect(() => {
    if (!enabled) {
      setFile(null);
      setStreamUrl(null);
      setLoading(false);
      setError(null);
      setIsLargeFile(false);
      setFileSize(0);
      return;
    }
    
    void loadPdf();
    
    return () => {
      // 取消进行中的请求
      if (abortControllerRef.current) {
        abortControllerRef.current.abort();
        abortControllerRef.current = null;
      }
    };
  }, [enabled, loadPdf]);

  // 重试：多策略降级
  const retry = useCallback(() => {
    const nextAttempt = retryAttempt + 1;
    setRetryAttempt(nextAttempt);

    if (nextAttempt === 1) {
      // 第一次重试：从数据库重新加载（同原来）
      lastLoadedKeyRef.current = null;
      fileRef.current = null;
      void loadPdf();
    } else if (nextAttempt === 2 && originalPath) {
      // 第二次重试：触发 PDF 处理流水线，重新生成附件内容
      debugLog.log('[usePdfLoader] Second retry: triggering PDF processing pipeline for:', nodeId);
      setLoading(true);
      setError(null);
      invoke('vfs_start_pdf_processing', { fileId: nodeId })
        .then(() => {
          lastLoadedKeyRef.current = null;
          fileRef.current = null;
          void loadPdf();
        })
        .catch((pipelineErr: unknown) => {
          debugLog.error('[usePdfLoader] Pipeline trigger failed:', pipelineErr);
          setError(pipelineErr instanceof Error ? pipelineErr.message : i18n.t('pdf:errors.pipeline_failed', { defaultValue: 'Failed to trigger PDF processing pipeline' }));
          setLoading(false);
        });
    } else {
      // 第三次及以上重试：提示用户重新导入
      debugLog.log('[usePdfLoader] Third+ retry: suggesting re-import for:', nodeId);
      setError(
        i18n.t('pdf:errors.reimport_suggested', {
          defaultValue: 'Unable to load PDF after multiple attempts. Please try re-importing the file.',
          count: nextAttempt,
        })
      );
      setLoading(false);
    }
  }, [loadPdf, nodeId, originalPath, retryAttempt]);

  // 重置重试状态，允许从头开始重新加载
  const resetRetry = useCallback(() => {
    setRetryAttempt(0);
    lastLoadedKeyRef.current = null;
    fileRef.current = null;
    setStreamUrl(null);
    setError(null);
    setLoading(false);
  }, []);

  return {
    file,
    streamUrl,
    loading,
    error,
    isLargeFile,
    fileSize,
    retry,
    retryAttempt,
    resetRetry,
  };
}

/**
 * 清除 PDF 缓存
 * 可在内存压力大时调用
 */
export function clearPdfCache(): void {
  pdfCache.clear();
  pdfCacheTotalSize = 0;
  debugLog.log('[usePdfLoader] Cache cleared');
}

/**
 * 获取缓存状态
 */
export function getPdfCacheInfo(): { size: number; keys: string[] } {
  return {
    size: pdfCache.size,
    keys: Array.from(pdfCache.keys()),
  };
}
