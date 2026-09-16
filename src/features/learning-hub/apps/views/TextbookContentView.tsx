/**
 * TextbookContentView - 教材内容视图
 *
 * 统一应用面板中的教材阅读视图。
 * 根据 previewType 路由到不同的预览组件：
 * - pdf: PDF 查看器
 * - docx: DOCX 富文本预览
 * - xlsx: Excel 表格预览
 * - text: 纯文本预览
 * 
 * 元数据字段：
 * - filePath: string - 文件路径
 * - readingProgress: { page: number; lastReadAt?: number } - 阅读进度（PDF专用）
 * - pageCount: number - 总页数
 */

import React, { useState, useCallback, useMemo, useRef, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { WarningCircle, FileText, CircleNotch, ArrowClockwise, FileX, Bug, Lock, Timer, UploadSimple, ArrowSquareOut, Scan } from '@phosphor-icons/react';
import { NotionButton } from '@/components/ui/NotionButton';
import { TextbookPdfViewer, type ReadingProgress, type Bookmark } from '@/features/pdf/components/TextbookPdfViewer';
import type { ContentViewProps } from '../UnifiedAppPanel';
import { dstu } from '@/dstu';
import { reportError } from '@/shared/result';
import { showGlobalNotification } from '@/components/UnifiedNotification';
import { invoke } from '@tauri-apps/api/core';
import { open as dialogOpen } from '@tauri-apps/plugin-dialog';
import { CustomScrollArea } from '@/components/custom-scroll-area';
import { vfsFileApi, getBlobStreamUrl, fetchBlobStreamResponse } from '@/api/vfsFileApi';
import { usePdfLoader } from '@/hooks/usePdfLoader';
import { usePdfProcessingProgress } from '@/hooks/usePdfProcessingProgress'; // task-045: 事件喂养进度 store
import { debugLog } from '@/debug-panel/debugMasterSwitch';
import { classifyPdfError, PdfErrorType } from '@/features/pdf/types/pdfErrors';
import {
  setPendingOcrPage,
  getLastOcrPage,
  dispatchOcrPageSync,
  OCR_PAGE_SYNC_EVENT,
  saveLastOcrPage,
  type OcrPageSyncEvent,
} from '@/features/learning-hub/ocrPageSync';
import { registerFlush } from '@/features/learning-hub/progressFlush';
import type { PdfLoadError } from '@/features/pdf/types/pdfErrors';
import {
  decodeBase64ToText,
  estimateBase64Size,
  LARGE_FILE_THRESHOLD,
  uint8ArrayToBase64,
} from '@/utils/base64FileUtils';
import { PreviewProvider, usePreviewContext } from './PreviewContext';
import type { ToolbarPreviewType } from './UnifiedPreviewToolbar';
import { resolveTextbookPreviewType } from './textbookPreviewResolver';
import { RichDocumentPreview } from './RichDocumentPreview';
import { usePdfFocusListener } from './usePdfFocusListener';
import { usePdfProcessingStore, getProcessingHint, TERMINAL_STAGES, isActiveProcessingStage } from '@/features/pdf/stores/pdfProcessingStore';
import { MarkdownPreview } from '@/features/notes/preview/MarkdownPreview';
import { getPdfProcessingStatus } from '@/api/vfsPdfProcessingApi';


const toToolbarPreviewType = (type: string | null): ToolbarPreviewType => {
  if (type === 'docx' || type === 'xlsx' || type === 'pptx' || type === 'text') {
    return type;
  }
  return 'other' as const;
};

/**
 * 教材内容视图
 */
const TextbookContentViewInner: React.FC<ContentViewProps> = ({
  node, isActive,
}) => {
  const { t } = useTranslation(['textbook', 'common', 'learningHub']);
  const {
    zoomScale,
    fontScale,
    previewType,
    setZoomScale,
    setFontScale,
    resetZoom,
    resetFont,
    setPreviewType,
  } = usePreviewContext();

  // 页面选择状态
  const [selectedPages, setSelectedPages] = useState<Set<number>>(new Set());

  // 保存进度的防抖引用
  const saveProgressTimerRef = useRef<number | null>(null);

  // ★ 追踪最新值的 ref（用于 cleanup flush，避免闭包捕获过期值）
  const nodePathRef = useRef(node.path);
  const nodeIdRef = useRef(node.id);
  const nodeMetadataRef = useRef(node.metadata);
  const pendingProgressRef = useRef<ReadingProgress | null>(null);
  const pendingBookmarksRef = useRef<Bookmark[] | null>(null);

  // 同步最新值到 ref
  useEffect(() => {
    nodePathRef.current = node.path;
    nodeIdRef.current = node.id;
    nodeMetadataRef.current = node.metadata;
  }, [node.path, node.id, node.metadata]);
  
  // 非 PDF 文件的内容状态
  const [fileContent, setFileContent] = useState<string | null>(null);
  const [contentLoading, setContentLoading] = useState(false);
  const [contentError, setContentError] = useState<string | null>(null);
  
  // ★ 非 PDF 内容重新加载的触发计数器
  const [contentRetryCount, setContentRetryCount] = useState(0);

  // ★ PDF 初始态 spinner 超时检测（防止无限旋转）
  const [pdfInitTimedOut, setPdfInitTimedOut] = useState(false);

  // ★ PDF stream 协议降级加载标志（当 DB 加载失败时尝试直接文件路径加载）
  const [usePdfStreamFallback, setUsePdfStreamFallback] = useState(false);

  // ======== OCR 相关状态 ========

  /** OCR 可用性检查结果（null=未检查, {configured:false}=未配置, {configured:true}=已配置） */
  const [ocrAvailability, setOcrAvailability] = useState<{ configured: boolean; modelName?: string } | null>(null);
  /** 视图模式: 'pdf'=PDF原图, 'ocr'=OCR文字, 'split'=分屏对比 */
  type ViewMode = 'pdf' | 'ocr' | 'split';
  const [viewMode, setViewMode] = useState<ViewMode>('pdf');
  /** OCR 识别文本内容 */
  const [ocrTextContent, setOcrTextContent] = useState<string | null>(null);

  // 处理页面选择变化 + 广播给 Chat InputBar
  const handlePageSelectionChange = useCallback((pages: Set<number>) => {
    setSelectedPages(pages);
    // 广播选中页码到 Chat InputBar（通过自定义 DOM 事件）
    document.dispatchEvent(new CustomEvent('pdf-page-refs:update', {
      detail: {
        sourceId: node.sourceId,
        sourceName: node.name,
        pages: Array.from(pages).sort((a, b) => a - b),
      },
    }));
  }, [node.sourceId, node.name]);

  // 监听 Chat 侧发来的清除/移除选择事件
  // ★ 标签页：通过 sourceId 过滤，避免多个 PDF tab 互相干扰
  useEffect(() => {
    const handleClear = (event: Event) => {
      const detail = (event as CustomEvent<{ sourceId?: string }>).detail;
      if (detail?.sourceId && detail.sourceId !== node.sourceId) return;
      setSelectedPages(new Set());
    };
    const handleRemove = (event: Event) => {
      const detail = (event as CustomEvent<{ page: number; sourceId?: string }>).detail;
      if (detail?.sourceId && detail.sourceId !== node.sourceId) return;
      setSelectedPages((prev) => {
        const next = new Set(prev);
        next.delete(detail.page);
        return next;
      });
    };
    document.addEventListener('pdf-page-refs:clear', handleClear);
    document.addEventListener('pdf-page-refs:remove', handleRemove);
    return () => {
      document.removeEventListener('pdf-page-refs:clear', handleClear);
      document.removeEventListener('pdf-page-refs:remove', handleRemove);
    };
  }, [node.sourceId]);

  // 处理导出选中页面（已废弃，保留空回调以兼容 TextbookPdfViewer 接口）
  const handleExportSelectedPages = useCallback(() => {}, []);

  // 从 node.metadata.filePath 获取文件路径
  const filePath = node.metadata?.filePath as string | undefined;
  const [filePathStat, setFilePathStat] = useState<{ available: boolean; size?: number } | null>(
    filePath ? { available: true } : { available: false }
  );
  
  // 根据 previewType 确定渲染模式（优先使用数据库值，若为 none 则根据扩展名推断）
  const resolvedPreviewType = resolveTextbookPreviewType(node.previewType, node.name);
  const isPdf = resolvedPreviewType === 'pdf';
  const isDocx = resolvedPreviewType === 'docx';
  const isXlsx = resolvedPreviewType === 'xlsx';
  const isPptx = resolvedPreviewType === 'pptx';
  const isText = resolvedPreviewType === 'text';
  const isUnsupported = resolvedPreviewType === 'none';
  const needsFileContent = isDocx || isXlsx || isPptx || isText;

  /** ★ task-045: 挂载处理进度事件监听——media-processing-* 事件喂养全局 store,
   * learning-hub 此前无人监听(chat InputBar 未挂载时),重 OCR 期间进度条永不更新 */
  usePdfProcessingProgress();

  /** 订阅 PDF 处理状态 Store（响应 OCR/文本提取等处理进度） */
  const processingStatus = usePdfProcessingStore(
    useCallback(
      (state) => (isPdf ? state.statusMap.get(node.sourceId) : undefined),
      [isPdf, node.sourceId],
    ),
  );

  // ★ Poll for live processing status on mount (fixes stale progress when user reopens PDF during OCR)
  useEffect(() => {
    if (!isPdf || !node.sourceId) return;

    // Skip polling if we already have terminal status
    if (processingStatus && TERMINAL_STAGES.has(processingStatus.stage)) return;

    let cancelled = false;

    const pollStatus = async () => {
      try {
        const status = await getPdfProcessingStatus(node.sourceId);
        if (cancelled || !status) return;

        usePdfProcessingStore.getState().setFullStatus(node.sourceId, {
          stage: status.stage,
          percent: status.percent,
          readyModes: (status.readyModes ?? []) as Array<'text' | 'ocr' | 'image'>,
          currentPage: status.currentPage,
          totalPages: status.totalPages,
          error: status.error,
          mediaType: 'pdf',
        });
      } catch (err) {
        if (!cancelled) {
          console.debug('[TextbookContentView] Poll processing status failed (may not be processing):', err);
        }
      }
    };

    void pollStatus();

    return () => { cancelled = true; };
  }, [isPdf, node.sourceId, processingStatus?.stage]);

  // ★ 使用共享 Hook 监听 PDF 页码跳转事件
  const [focusRequest, handleFocusHandled] = usePdfFocusListener({
    enabled: isPdf,
    nodeId: node.id,
    nodeSourceId: node.sourceId,
    nodePath: node.path,
    nodeName: node.name,
  });

  useEffect(() => {
    const contextPreviewType = (isDocx || isXlsx || isPptx || isText)
      ? resolvedPreviewType
      : null;
    setPreviewType(contextPreviewType);
  }, [isDocx, isPptx, isText, isXlsx, resolvedPreviewType, setPreviewType]);

  // 校验 filePath 是否可访问（用于失效回退）
  useEffect(() => {
    let isActive = true;
    if (!filePath) {
      setFilePathStat({ available: false });
      return;
    }

    const checkFilePath = async () => {
      try {
        const size = await invoke<number>('get_file_size', { path: filePath });
        if (!isActive) return;
        setFilePathStat({ available: true, size });
      } catch (err: unknown) {
        if (!isActive) return;
        console.warn('[TextbookContentView] filePath not accessible, fallback to DB:', filePath, err);
        setFilePathStat({ available: false });
      }
    };

    void checkFilePath();
    return () => {
      isActive = false;
    };
  }, [filePath]);

  // ★ PDF-403 修复：教材 PDF 不传 filePath，避免触发 pdfstream:// 协议的目录白名单限制导致 403。
  // 改为始终通过 usePdfLoader → vfs_get_attachment_content 从 VFS blob/DB 加载。
  // 当 usePdfStreamFallback 激活时，允许使用 filePath 直接加载（降级方案）。
  const effectiveFilePath = isPdf && !usePdfStreamFallback
    ? undefined
    : (filePathStat?.available ? filePath : undefined);
  const effectiveFileSize = isPdf ? undefined : (filePathStat?.available ? filePathStat.size : undefined);

  // 使用统一的 PDF 加载 Hook（支持缓存、去重、大文件检测）
  const {
    file: pdfFile,
    streamUrl: pdfStreamUrl,
    loading: pdfLoading,
    error: pdfError,
    isLargeFile: isPdfLargeFile,
    retry: retryPdfLoad,
  } = usePdfLoader({
    nodeId: node.id,
    fileName: node.name,
    filePath: effectiveFilePath,
    cacheKey: `${node.id}:${node.updatedAt || ''}`,
    enabled: isPdf && !effectiveFilePath, // 只有当是 PDF 且没有可用 filePath 时才从数据库加载
    // ★ task-051: 直连流式模式——blob 存在时不整文件下载,
    // 直接把 pdfstream URL 交给 PDF.js Range 加载(大扫描书秒开)
    preferStreamUrl: true,
  });

  // ★ 分类后的 PDF 错误（结构化信息，用于丰富错误 UI）
  const classifiedPdfError = useMemo<PdfLoadError | null>(() => {
    if (!pdfError) return null;
    return classifyPdfError(pdfError);
  }, [pdfError]);

  // ★ 重新导入 PDF 文件（通过文件选择器）
  const handleReimportPdf = useCallback(async () => {
    try {
      const selected = await dialogOpen({
        multiple: false,
        filters: [{ name: 'PDF 文件', extensions: ['pdf'] }],
        title: '重新导入 PDF 文件',
      });
      if (!selected) return;
      const selectedPath = selected as string;
      debugLog.log('[TextbookContentView] Re-import PDF selected:', selectedPath);
      // 触发 PDF 处理流水线以重新导入
      await invoke('vfs_start_pdf_processing', { fileId: node.id });
      // 清除缓存并触发重新加载
      retryPdfLoad();
    } catch (err: unknown) {
      console.error('[TextbookContentView] Re-import failed:', err);
    }
  }, [node.id, retryPdfLoad]);

  // ★ 降级加载：尝试使用 pdfstream:// 协议直接加载文件
  const handleTryAlternativeLoad = useCallback(() => {
    setUsePdfStreamFallback(true);
  }, []);

  // 加载非 PDF 文件内容
  useEffect(() => {
    if (!needsFileContent) return;
    
    let isMounted = true;
    setContentLoading(true);
    setContentError(null);
    
    const loadContent = async () => {
      try {
        let base64Content: string | null = null;
        const knownSize = typeof node.size === 'number' ? node.size : null;
        if (knownSize && knownSize > LARGE_FILE_THRESHOLD) {
          setContentError(t('learningHub:file.previewTooLarge', '文件过大，无法预览'));
          setContentLoading(false);
          return;
        }

        const loadFromVfs = async () => {
          // ★ task-035/A4#1: 优先 pdfstream:// 协议流式取字节（本地转 base64 供下游解析，
          //   免整文件 base64 过 IPC），无 blob / 协议拒绝时回退 base64 路径
          const streamUrl = await getBlobStreamUrl(node.id);
          if (streamUrl) {
            const streamResp = await fetchBlobStreamResponse(streamUrl);
            if (streamResp) {
              const streamBytes = new Uint8Array(await streamResp.arrayBuffer());
              if (!isMounted) return null;
              if (streamBytes.byteLength > LARGE_FILE_THRESHOLD) {
                setContentError(t('learningHub:file.previewTooLarge', '文件过大，无法预览'));
                setContentLoading(false);
                return null;
              }
              return uint8ArrayToBase64(streamBytes);
            }
          }

          const result = await invoke<{ content: string | null; found: boolean }>('vfs_get_attachment_content', {
            attachmentId: node.id,
          });
          if (!isMounted) return null;

          if (result?.found && result?.content) {
            const estimatedSize = estimateBase64Size(result.content);
            if (estimatedSize > LARGE_FILE_THRESHOLD) {
              setContentError(t('learningHub:file.previewTooLarge', '文件过大，无法预览'));
              setContentLoading(false);
              return null;
            }
            return result.content;
          }
          return null;
        };
        
        // ★ 优先使用可用的 filePath 读取本地文件，失败则回退到 VFS
        if (effectiveFilePath) {
          try {
            const fileSize = effectiveFileSize ?? await invoke<number>('get_file_size', { path: effectiveFilePath });
            if (!isMounted) return;
            if (fileSize > LARGE_FILE_THRESHOLD) {
              setContentError(t('learningHub:file.previewTooLarge', '文件过大，无法预览'));
              setContentLoading(false);
              return;
            }

            const bytes = await invoke<number[]>('read_file_bytes', { path: effectiveFilePath });
            if (!isMounted) return;
            // 转换为 base64（分块，避免大数组字符串拼接造成卡顿）
            base64Content = uint8ArrayToBase64(new Uint8Array(bytes));
          } catch (err: unknown) {
            console.warn('[TextbookContentView] Failed to read filePath, fallback to VFS:', err);
            if (!isMounted) return;
            base64Content = await loadFromVfs();
          }
        } else {
          base64Content = await loadFromVfs();
        }
        
        if (base64Content) {
          setFileContent(base64Content);
          setContentLoading(false);
        } else {
          setContentError(t('learningHub:file.contentNotFound', '未找到文件内容 (id: {{id}})', { id: node.id }));
          setContentLoading(false);
        }
      } catch (err: unknown) {
        console.error('[TextbookContentView] Failed to load file:', err);
        if (isMounted) {
          setContentError(err instanceof Error ? err.message : t('learningHub:file.loadFailed', '加载文件失败'));
          setContentLoading(false);
        }
      }
    };
    
    void loadContent();
    
    return () => {
      isMounted = false;
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [needsFileContent, effectiveFilePath, effectiveFileSize, node.id, node.size, t, contentRetryCount]);
  
  // 从 node.metadata 提取阅读进度
  const readingProgress = useMemo<ReadingProgress | undefined>(() => {
    const progress = node.metadata?.readingProgress as { page?: number; lastReadAt?: number } | undefined;
    if (progress && typeof progress.page === 'number' && progress.page > 0) {
      return {
        page: progress.page,
        lastReadAt: progress.lastReadAt,
      };
    }
    return undefined;
  }, [node.metadata?.readingProgress]);

  
  // 从 node.metadata 提取书签列表
  const [bookmarks, setBookmarks] = useState<Bookmark[]>([]);
  
  // 书签保存的防抖引用
  const saveBookmarksTimerRef = useRef<number | null>(null);
  
  // 初始化书签数据
  useEffect(() => {
    const savedBookmarks = node.metadata?.bookmarks as Bookmark[] | undefined;
    if (savedBookmarks && Array.isArray(savedBookmarks)) {
      setBookmarks(savedBookmarks);
    } else {
      setBookmarks([]);
    }
  }, [node.metadata?.bookmarks]);
  
  // 保存阅读进度到 DSTU
  // ★★ per-page OCR: 跟踪当前 PDF 页码，按需加载单页 MD
  // ★ 初始化当前页码：取 sessionStorage 和 DSTU 元数据中最新的值
  const [currentPdfPage, setCurrentPdfPage] = useState<number>(() => {
    const saved = node.sourceId ? getLastOcrPage(node.sourceId) : null;
    const meta = readingProgress?.page || 1;
    return saved && saved > meta ? saved : meta;
  });
  const [ocrPageMd, setOcrPageMd] = useState<string | null>(null);
  const [ocrPageMdLoading, setOcrPageMdLoading] = useState(false);

  // ★★ 实时阅读进度：currentPdfPage 覆盖过期保存值，确保切换视图/模式后定位准确
  const liveReadingProgress = useMemo<ReadingProgress | undefined>(() => ({
    page: currentPdfPage,
    lastReadAt: readingProgress?.lastReadAt ?? Date.now(),
  }), [currentPdfPage, readingProgress?.lastReadAt]);

  const handleProgressChange = useCallback((progress: ReadingProgress) => {
    // ★ 立即更新当前页码（用于 per-page OCR 加载）
    setCurrentPdfPage(progress.page);

    // ★ 记录 pending 值，供 unmount flush 使用
    pendingProgressRef.current = progress;

    // 防抖：清理之前的定时器
    if (saveProgressTimerRef.current) {
      window.clearTimeout(saveProgressTimerRef.current);
    }

    // 延迟保存，避免频繁写入
    saveProgressTimerRef.current = window.setTimeout(async () => {
      saveProgressTimerRef.current = null;
      pendingProgressRef.current = null; // 已提交，清除 pending

      // 构建新的元数据（保留原有字段）
      const newMetadata = {
        ...nodeMetadataRef.current,
        readingProgress: {
          page: progress.page,
          lastReadAt: progress.lastReadAt,
        },
      };

      // 通过 DSTU 保存元数据 (Result模式)
      const result = await dstu.setMetadata(nodePathRef.current, newMetadata);
      if (!result.ok) {
        reportError(result.error, '保存阅读进度');
        console.warn('[TextbookContentView] Failed to save reading progress:', result.error.toUserMessage());
      }
    }, 2000); // 2秒防抖，避免频繁保存
  }, []);
  
  // 保存书签到后端（通过 VFS API）
  const handleBookmarksChange = useCallback((newBookmarks: Bookmark[]) => {
    // 更新本地状态
    setBookmarks(newBookmarks);
    // ★ 记录 pending 值，供 unmount flush 使用
    pendingBookmarksRef.current = newBookmarks;
    
    // 防抖：清理之前的定时器
    if (saveBookmarksTimerRef.current) {
      window.clearTimeout(saveBookmarksTimerRef.current);
    }
    
    // 延迟保存，避免频繁写入
    saveBookmarksTimerRef.current = window.setTimeout(async () => {
      saveBookmarksTimerRef.current = null;
      pendingBookmarksRef.current = null; // 已提交，清除 pending
      
      try {
        const fileId = nodeIdRef.current;
        
        // 调用后端 API 保存书签
        await vfsFileApi.updateBookmarks(fileId, newBookmarks);
        
        // 同时更新 DSTU 元数据，保持数据一致性
        const newMetadata = {
          ...nodeMetadataRef.current,
          bookmarks: newBookmarks,
        };
        await dstu.setMetadata(nodePathRef.current, newMetadata);
      } catch (err: unknown) {
        console.error('[TextbookContentView] Failed to save bookmarks:', err);
        showGlobalNotification('error', t('textbook:bookmarkSaveFailed', '书签保存失败'));
      }
    }, 1000); // 1秒防抖
  }, [t]);
  
  // ★ 清理定时器并 flush 未保存的数据（防止卸载丢失）
  React.useEffect(() => {
    return () => {
      // 清除定时器
      if (saveProgressTimerRef.current) {
        window.clearTimeout(saveProgressTimerRef.current);
        saveProgressTimerRef.current = null;
      }
      if (saveBookmarksTimerRef.current) {
        window.clearTimeout(saveBookmarksTimerRef.current);
        saveBookmarksTimerRef.current = null;
      }

      // ★ 合并 flush 未保存的阅读进度和书签（单次 setMetadata，避免竞态覆盖）
      const pendingProgress = pendingProgressRef.current;
      const pendingBookmarks = pendingBookmarksRef.current;
      pendingProgressRef.current = null;
      pendingBookmarksRef.current = null;

      if (pendingProgress || pendingBookmarks) {
        const mergedMetadata = { ...nodeMetadataRef.current };
        if (pendingProgress) {
          mergedMetadata.readingProgress = {
            page: pendingProgress.page,
            lastReadAt: pendingProgress.lastReadAt,
          };
        }
        if (pendingBookmarks) {
          mergedMetadata.bookmarks = pendingBookmarks;
          // 书签同时保存到 VFS API
          void vfsFileApi.updateBookmarks(nodeIdRef.current, pendingBookmarks);
        }
        dstu.setMetadata(nodePathRef.current, mergedMetadata).then(result => {
          if (!result.ok) {
            reportError(result.error, '保存未持久化的阅读进度/书签');
            console.warn('[TextbookContentView] flush setMetadata failed:', result.error.toUserMessage());
          }
        }).catch(err => {
          console.error('[TextbookContentView] flush setMetadata error:', err);
        });
      }
    };
  }, []);

  /** ★★ 全局 flush：应用关闭/后台时立即保存当前阅读进度到 DSTU */
  useEffect(() => {
    if (!isPdf || !node.sourceId) return;
    const unregister = registerFlush(() => {
      const page = currentPdfPage;
      if (page < 1) return;
      // 1) 同步保存到 sessionStorage（快速，不掉失）
      saveLastOcrPage(node.sourceId!, page);
      // 2) 保存到 DSTU 元数据（fire-and-forget，但 beforeunload 会同步等待）
      const metadata = {
        ...nodeMetadataRef.current,
        readingProgress: { page, lastReadAt: Date.now() },
      };
      dstu.setMetadata(node.path, metadata).catch(() => {});
    });
    return unregister;
  }, [isPdf, node.sourceId, currentPdfPage, node.path]);

  // ======== OCR 检测与处理流水线 ========

  /** 加载 OCR 文本（复用函数，避免 useEffect 闭包陷阱） */
  const loadOcrText = useCallback(async () => {
    if (!node.sourceId && !node.resourceId) return;
    const resourceId = node.resourceId || node.sourceId;
    try {
      const ocrInfo = await invoke<{
        hasOcr: boolean;
        ocrText: string | null;
      }>('vfs_get_resource_ocr_info', { resourceId });
      if (ocrInfo.hasOcr && ocrInfo.ocrText) {
        setOcrTextContent(ocrInfo.ocrText);
      }
    } catch (err: unknown) {
      console.warn('[TextbookContentView] Failed to load OCR text:', err);
    }
  }, [node.resourceId, node.sourceId]);

  /** 检查 OCR 配置状态（仅用于 UI 状态栏展示） */
  useEffect(() => {
    if (!isPdf) return;
    let cancelled = false;
    invoke<{ configured: boolean; modelName?: string }>('check_ocr_availability')
      .then((avail) => { if (!cancelled) setOcrAvailability(avail); })
      .catch(() => { if (!cancelled) setOcrAvailability({ configured: false }); });
    return () => { cancelled = true; };
  }, [isPdf]);

  // ★★ 修复：自动 OCR 不再限制 tb_ 前缀，支持 att_ / file_ / tb_ 所有 PDF
  const [isOcrTriggering, setIsOcrTriggering] = useState(false);
  const [ocrAutoEnabled, setOcrAutoEnabled] = useState(true);
  // ★★ 调试诊断：OCR 笔记存在性检测
  const [ocrNoteId, setOcrNoteId] = useState<string | null>(null);
  const [ocrNoteLoading, setOcrNoteLoading] = useState(false);
  const [ocrNoteError, setOcrNoteError] = useState<string | null>(null);
  const ocrStatus = usePdfProcessingStore((s) => s.statusMap.get(node.sourceId));
  // ★ task-048: 活动阶段改走统一集合——旧硬编码漏掉 text_extraction/vector_indexing,
  // 向量索引期（大文件数十秒）被误判为「不在处理中」,调试条百分比消失、重OCR按钮可误点
  const isOcrProcessing = isActiveProcessingStage(ocrStatus?.stage);
  const isOcrCompleted = ocrStatus?.stage === 'completed' || ocrStatus?.stage === 'completed_with_issues';
  const ocrReady = ocrStatus?.readyModes?.includes('ocr');
  // ★ OCR 内容已加载标记（ocrTextContent 非空即为已有 OCR 内容）
  const hasOcrContent = Boolean(ocrTextContent);
  // ★ 是否需要 OCR 且尚未执行：PDF 文件 + 无 OCR 内容 + 未在处理中
  const needsOcr = isPdf && !hasOcrContent && !isOcrProcessing && !isOcrCompleted && !isOcrTriggering;

  /** 手动触发 OCR 处理（用于非 tb_ 前缀的 PDF 或重试） */
  const handleStartOcr = useCallback(async () => {
    if (!node.sourceId || isOcrTriggering || isOcrProcessing) return;
    setIsOcrTriggering(true);
    try {
      const response = await invoke<{ status: string; message?: string }>(
        'vfs_ensure_ocr_pipeline',
        // ★ task-045: 手动"重新 OCR"强制重启——旧实现已完成 OCR 只补笔记不重启
        { fileId: node.sourceId, force: true },
      );

      // ★ 根据后端返回状态给用户反馈
      switch (response.status) {
        case 'ocr_started':
        case 'ocr_resumed':
          // ★ task-048: 启动/续跑确认后立即把 store 置为处理中——
          // 进度条即时出现，不必等后端第一个进度事件（断点续跑时 store 里
          // 可能还是挂载轮询灌入的 completed,update 的重跑分支会正确重置进度）
          usePdfProcessingStore.getState().update(node.sourceId, {
            stage: 'ocr_processing',
            percent: 1,
            readyModes: [],
            mediaType: 'pdf',
          });
          if (response.status === 'ocr_started') {
            showGlobalNotification('info', t('textbook:ocr.started', 'OCR 识别已启动，请等待处理完成'));
          } else {
            showGlobalNotification('info', t('textbook:ocr.resumed', 'OCR 从检查点恢复，继续处理...'));
          }
          break;
        case 'already_running':
          showGlobalNotification('info', t('textbook:ocr.alreadyRunning', 'OCR 正在处理中，请稍候'));
          break;
        case 'note_created':
          // ★ OCR 数据已存在但笔记缺失 → 已补建
          showGlobalNotification('success', t('textbook:ocr.noteCreated', 'OCR 笔记已生成，可打开查看'));
          // 重新触发笔记查询
          ocrQueryKeyRef.current = null;
          setOcrNoteId(null);
          setOcrNoteLoading(false);
          break;
        case 'ocr_completed':
          // OCR 已完成但笔记创建失败
          if (response.message?.includes('笔记创建失败')) {
            showGlobalNotification('warning', t('textbook:ocr.noteFailed', 'OCR 已完成但笔记创建失败，请重试'));
          } else {
            showGlobalNotification('info', t('textbook:ocr.completed', 'OCR 已处理完成'));
          }
          // 仍然尝试查询笔记（可能之前创建成功）
          ocrQueryKeyRef.current = null;
          setOcrNoteId(null);
          setOcrNoteLoading(false);
          break;
        case 'text_sufficient':
          showGlobalNotification('info', t('textbook:ocr.noNeed', '文本已足够，无需 OCR'));
          break;
        default:
          break;
      }

      // 获取 OCR 文本内容
      // ★ task-045: 内容刷新查询静默失败——旧实现此处抛错会走外层 catch 弹
      // "OCR 启动失败"错误 toast,与上方成功提示同屏双弹窗
      try {
        const ocrInfoResult = await invoke<{ hasOcr: boolean; ocrText: string | null }>(
          'vfs_get_resource_ocr_info',
          { resourceId: node.resourceId || node.sourceId },
        );
        if (ocrInfoResult.hasOcr && ocrInfoResult.ocrText) {
          setOcrTextContent(ocrInfoResult.ocrText);
        }
      } catch (infoErr) {
        console.debug('[TextbookContentView] OCR info refresh failed (non-fatal):', infoErr);
      }
    } catch (err: unknown) {
      console.warn('[TextbookContentView] Manual OCR failed:', err);
      // ★ 提取具体错误信息
      let errMsg = String(err);
      if (typeof err === 'string') {
        errMsg = err;
      } else if (err && typeof err === 'object' && 'message' in err) {
        errMsg = String((err as { message: unknown }).message);
      } else if (err && typeof err === 'object' && 'toString' in err) {
        errMsg = String(err);
      }
      showGlobalNotification('error', `${t('textbook:ocr.failed', 'OCR 启动失败')}: ${errMsg}`);
    } finally {
      setIsOcrTriggering(false);
    }
  }, [node.sourceId, node.resourceId, isOcrTriggering, isOcrProcessing, t]);

  /** 简化后的 OCR 处理：仅对传统 tb_ 教科书自动 OCR，file_/att_ PDF 由用户手动触发 */
  useEffect(() => {
    if (!isPdf || !node.sourceId) return;
    // ★★ 修复：仅对 tb_ 前缀的教科书自动 OCR
    // file_/att_ 前缀的 PDF 由用户通过"开始 OCR"按钮手动触发，避免竞态条件
    const isLegacyTextbook = node.sourceId.startsWith('tb_');
    if (!isLegacyTextbook) return;
    if (!ocrAutoEnabled) return;
    // 等待 OCR 可用性检查完成
    if (ocrAvailability === null) return;
    if (!ocrAvailability.configured) return;
    // 如果已有 OCR 内容或正在处理中，跳过
    if (hasOcrContent || isOcrProcessing || isOcrCompleted) return;

    let cancelled = false;

    const ensureOcr = async () => {
      try {
        await invoke('vfs_ensure_ocr_pipeline', { fileId: node.sourceId });
        if (cancelled) return;

        const ocrInfo = await invoke<{ hasOcr: boolean; ocrText: string | null }>(
          'vfs_get_resource_ocr_info',
          { resourceId: node.resourceId || node.sourceId },
        );
        if (cancelled) return;
        if (ocrInfo.hasOcr && ocrInfo.ocrText) {
          setOcrTextContent(ocrInfo.ocrText);
        }
      } catch (err: unknown) {
        if (!cancelled) {
          console.warn('[TextbookContentView] OCR pipeline failed:', err);
        }
      }
    };

    void ensureOcr();
    return () => { cancelled = true; };
  }, [isPdf, node.sourceId, node.resourceId, ocrAutoEnabled, ocrAvailability, hasOcrContent, isOcrProcessing, isOcrCompleted]);

  /** ★★ 调试：检测是否存在 OCR 笔记（通过文件名匹配查找 note_xxx）
   *  使用 useRef 追踪查询 key，避免 ocrNoteId/ocrNoteLoading 在依赖数组中造成死锁 */
  const ocrQueryKeyRef = useRef<string | null>(null);

  useEffect(() => {
    if (!isPdf || !node.sourceId || !isOcrCompleted) return;

    const queryKey = `${node.sourceId}::${node.name}`;
    // 同一个文件只查一次（除非 sourceId/name 变化导致 ref 被重置）
    if (ocrQueryKeyRef.current === queryKey) return;
    ocrQueryKeyRef.current = queryKey;

    let cancelled = false;
    setOcrNoteLoading(true);
    setOcrNoteError(null);

    const findOcrNote = async () => {
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        // ★ 优先搜索逐页笔记：source:<file_id> + page:<currentPdfPage>
        let results = await invoke<Array<{ id: string; name: string; path: string }>>('dstu_list', {
          path: '/',
          options: {
            typeFilter: 'note',
            tags: [`source:${node.sourceId}`, `page:${currentPdfPage}`],
            limit: 3,
            offset: 0,
          },
        });
        if (cancelled) return;
        // 回退：搜索该文件的所有逐页笔记（不限页数）
        if (results.length === 0) {
          results = await invoke<Array<{ id: string; name: string; path: string }>>('dstu_list', {
            path: '/',
            options: {
              typeFilter: 'note',
              tags: [`source:${node.sourceId}`, 'page:'],
              limit: 1,
              offset: 0,
            },
          });
          if (cancelled) return;
        }
        // 再回退：搜索旧版整体笔记（兼容未拆分的旧 OCR 笔记）
        if (results.length === 0) {
          results = await invoke<Array<{ id: string; name: string; path: string }>>('dstu_list', {
            path: '/',
            options: {
              typeFilter: 'note',
              tags: [`source:${node.sourceId}`],
              limit: 1,
              offset: 0,
            },
          });
          if (cancelled) return;
        }
        if (!cancelled) {
          setOcrNoteId(results.length > 0 ? (results[0].id) : null);
        }
      } catch (err: any) {
        if (!cancelled) {
          setOcrNoteError(err?.message || String(err));
        }
      } finally {
        if (!cancelled) setOcrNoteLoading(false);
      }
    };

    void findOcrNote();
    return () => { cancelled = true; };
  }, [isPdf, node.sourceId, node.name, isOcrCompleted, currentPdfPage]);

  // 切换文件时重置查询状态
  useEffect(() => {
    ocrQueryKeyRef.current = null;
    setOcrNoteId(null);
    setOcrNoteLoading(false);
    setOcrNoteError(null);
  }, [node.sourceId, node.name]);

  /** ★★ 打开 OCR 笔记（传递当前页码，MD 阅读器直接跳转） */
  const handleOpenOcrNote = useCallback(() => {
    if (!ocrNoteId) return;
    // ★ 设置初始页码，NoteContentView 消费后跳转到对应页
    if (node.sourceId) {
      setPendingOcrPage(node.sourceId, currentPdfPage);
    }
    window.dispatchEvent(new CustomEvent('learningHubOpenNote', {
      detail: { noteId: ocrNoteId, source: 'ocr-debug-bar' },
    }));
  }, [ocrNoteId, node.sourceId, currentPdfPage]);

  /** 监听处理状态变化：处理完成时加载 OCR 文本 */
  useEffect(() => {
    if (!processingStatus || ocrTextContent) return;

    const { stage, readyModes } = processingStatus;
    if ((stage === 'completed' || stage === 'completed_with_issues') && readyModes.includes('ocr')) {
      void loadOcrText();
    }
  }, [processingStatus, ocrTextContent, loadOcrText]);

  /** ★★ per-page OCR: 按页加载 MD（避免全量加载卡死编辑器） */
  useEffect(() => {
    if (!isPdf || !node.sourceId || !isOcrCompleted) return;
    if (viewMode !== 'ocr' && viewMode !== 'split') return;

    let cancelled = false;
    setOcrPageMdLoading(true);

    const loadPageMd = async () => {
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        // page_index is 0-based in backend, currentPdfPage is 1-based
        const md = await invoke<string>('get_ocr_page_md', {
          fileId: node.sourceId,
          pageIndex: currentPdfPage - 1,
        });
        if (!cancelled) {
          setOcrPageMd(md);
          setOcrPageMdLoading(false);
        }
      } catch (err: unknown) {
        if (!cancelled) {
          console.warn('[TextbookContentView] Failed to load page MD:', err);
          setOcrPageMd(null);
          setOcrPageMdLoading(false);
        }
      }
    };

    void loadPageMd();
    return () => { cancelled = true; };
  }, [isPdf, node.sourceId, isOcrCompleted, viewMode, currentPdfPage]);

  /** ★★ PDF → MD 页码同步：PDF 翻页时通知 MD 阅读器 */
  useEffect(() => {
    if (!isPdf || !node.sourceId) return;
    dispatchOcrPageSync({ fileId: node.sourceId, pageNumber: currentPdfPage, source: 'pdf' });
    saveLastOcrPage(node.sourceId, currentPdfPage);
  }, [isPdf, node.sourceId, currentPdfPage]);

  /** ★★ MD → PDF 页码同步：MD 阅读器翻页时同步回 PDF */
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<OcrPageSyncEvent>).detail;
      if (!detail || detail.source !== 'md') return;
      if (node.sourceId && detail.fileId !== node.sourceId) return;
      if (detail.pageNumber > 0) {
        setCurrentPdfPage(detail.pageNumber);
      }
    };
    window.addEventListener(OCR_PAGE_SYNC_EVENT, handler);
    return () => window.removeEventListener(OCR_PAGE_SYNC_EVENT, handler);
  }, [node.sourceId]);

  /** ★★ 标签页切换：失活时立即保存进度，激活时恢复最新页码 */
  const prevActiveRef = useRef(isActive);
  useEffect(() => {
    const wasActive = prevActiveRef.current;
    prevActiveRef.current = isActive;
    // 标签页失活 → 立即保存当前进度（绕过 2 秒防抖）
    if (wasActive && !isActive && node.sourceId && currentPdfPage > 0) {
      saveLastOcrPage(node.sourceId, currentPdfPage);
      // 立即提交 pending 进度到 DSTU
      if (pendingProgressRef.current) {
        dstu.setMetadata(node.path, {
          ...nodeMetadataRef.current,
          readingProgress: {
            page: pendingProgressRef.current.page,
            lastReadAt: pendingProgressRef.current.lastReadAt,
          },
        }).catch(() => {});
      }
    }
    // 标签页重新激活 → 恢复最新页码
    if (!wasActive && isActive && node.sourceId) {
      const saved = getLastOcrPage(node.sourceId);
      if (saved && saved > 0 && saved !== currentPdfPage) {
        setCurrentPdfPage(saved);
      }
    }
  }, [isActive, node.sourceId, node.path, currentPdfPage]);
  const retryContentLoad = useCallback(() => {
    setFileContent(null);
    setContentError(null);
    setContentRetryCount((c) => c + 1);
  }, []);

  // ★ PDF 初始态 spinner 超时检测（10 秒后显示提示 + 重试按钮，避免无限旋转）
  useEffect(() => {
    if (!isPdf || effectiveFilePath || pdfFile || pdfStreamUrl || pdfLoading || pdfError) {
      setPdfInitTimedOut(false);
      return;
    }
    const timer = window.setTimeout(() => {
      setPdfInitTimedOut(true);
    }, 10_000);
    return () => window.clearTimeout(timer);
  }, [isPdf, effectiveFilePath, pdfFile, pdfStreamUrl, pdfLoading, pdfError]);

  // ★ 移除 filePath 为空时的硬性错误，改为在内容加载失败时显示错误
  // 因为从 attachments 迁移的文件可能没有 filePath，但可以通过 vfs_get_attachment_content 获取内容
  
  // PDF 文件：如果没有 filePath 且没有 pdfFile/pdfStreamUrl，显示加载中或错误
  if (isPdf && !effectiveFilePath && !pdfFile && !pdfStreamUrl) {
    if (pdfLoading) {
      return (
        <div className="flex flex-col items-center justify-center h-full gap-4">
          <CircleNotch className="h-8 w-8 animate-spin text-primary" />
          {isPdfLargeFile && (
            <p className="text-sm text-muted-foreground">
              {t('textbook:loading.largeFile', '正在加载大文件，请稍候...')}
            </p>
          )}
        </div>
      );
    }
    if (pdfError) {
      // ★ 分类错误以提供丰富的 UI 展示
      const typed = classifiedPdfError ?? classifyPdfError(pdfError);
      // 错误类型 → 图标映射
      const ErrorIconMap: Record<PdfErrorType, React.ElementType> = {
        [PdfErrorType.NotFound]: FileX,
        [PdfErrorType.Corrupted]: Bug,
        [PdfErrorType.FormatError]: FileX,
        [PdfErrorType.LoadTimeout]: Timer,
        [PdfErrorType.ProcessingError]: WarningCircle,
        [PdfErrorType.PermissionDenied]: Lock,
        [PdfErrorType.Unknown]: WarningCircle,
      };
      const ErrorIcon = ErrorIconMap[typed.type] ?? WarningCircle;
      // 错误类型标签
      const typeLabelKey = `pdf:errors.type_${typed.type}`;
      const suggestionKey = `pdf:errors.suggestion_${typed.type}`;
      // 是否显示重新导入按钮
      const canReimport = typed.type === PdfErrorType.NotFound || typed.type === PdfErrorType.Corrupted;
      // 是否显示降级加载按钮（仅当有可用 filePath 时）
      const canTryAlternative = isPdf && !!filePath && !usePdfStreamFallback;

      return (
        <div className="flex flex-col items-center justify-center h-full gap-4">
          <ErrorIcon className="w-12 h-12 text-destructive" />
          <p className="text-destructive font-semibold text-lg">
            {t(typeLabelKey, typed.type)}
          </p>
          <p className="text-destructive text-center max-w-md">
            {typed.detail || pdfError}
          </p>
          <p className="text-muted-foreground text-sm text-center max-w-md">
            {t(suggestionKey, '')}
          </p>
          <div className="flex gap-2 flex-wrap justify-center">
            <NotionButton
              variant="default"
              size="sm"
              onClick={retryPdfLoad}
            >
              <ArrowClockwise className="h-3.5 w-3.5 mr-1.5" />
              {t('common:retry', '重试')}
            </NotionButton>
            {canReimport && (
              <NotionButton
                variant="outline"
                size="sm"
                onClick={handleReimportPdf}
              >
                <UploadSimple className="h-3.5 w-3.5 mr-1.5" />
                {t('pdf:errors.reimport', '重新导入')}
              </NotionButton>
            )}
            {canTryAlternative && (
              <NotionButton
                variant="outline"
                size="sm"
                onClick={handleTryAlternativeLoad}
              >
                <ArrowSquareOut className="h-3.5 w-3.5 mr-1.5" />
                {t('pdf:errors.try_alternative_load', '尝试其他方式加载')}
              </NotionButton>
            )}
          </div>
        </div>
      );
    }
    // 初始状态，等待加载（超时后显示提示 + 重试按钮）
    return (
      <div className="flex flex-col items-center justify-center h-full gap-4">
        <CircleNotch className="h-8 w-8 animate-spin text-primary" />
        {pdfInitTimedOut && (
          <>
            <p className="text-sm text-muted-foreground text-center">
              {t('textbook:loading.timeout', '加载时间较长，可能遇到问题')}
            </p>
            <NotionButton
              variant="default"
              size="sm"
              onClick={retryPdfLoad}
            >
              <ArrowClockwise className="h-3.5 w-3.5 mr-1.5" />
              {t('common:retry', '重试')}
            </NotionButton>
          </>
        )}
      </div>
    );
  }
  
  // 加载中状态
  const LoadingSpinner = () => (
    <div className="flex items-center justify-center h-full">
      <CircleNotch className="h-8 w-8 animate-spin text-primary" />
    </div>
  );
  
  // 错误状态
  if (contentError) {
    return (
      <div className="flex flex-col items-center justify-center h-full gap-4">
        <WarningCircle className="w-12 h-12 text-destructive" />
        <p className="text-destructive text-center">{contentError}</p>
        <NotionButton
          variant="default"
          size="sm"
          onClick={retryContentLoad}
        >
          <ArrowClockwise className="h-3.5 w-3.5 mr-1.5" />
          {t('common:retry', '重试')}
        </NotionButton>
      </div>
    );
  }
  
  const showRichToolbar = (isDocx || isXlsx || isPptx) && !!fileContent && !!previewType;
  const renderRichDocumentPreview = (
    kind: 'docx' | 'xlsx' | 'pptx',
    content: string
  ) => (
    <RichDocumentPreview
      kind={kind}
      base64Content={content}
      fileName={node.name}
      showToolbar={showRichToolbar}
      previewType={toToolbarPreviewType(previewType)}
      zoomScale={zoomScale}
      fontScale={fontScale}
      onZoomChange={setZoomScale}
      onFontChange={setFontScale}
      onZoomReset={resetZoom}
      onFontReset={resetFont}
      fallback={<LoadingSpinner />}
      rootClassName="bg-background"
    />
  );

  // DOCX 预览
  if (isDocx) {
    if (contentLoading || !fileContent) {
      return <LoadingSpinner />;
    }
    return renderRichDocumentPreview('docx', fileContent);
  }
  
  // XLSX 预览
  if (isXlsx) {
    if (contentLoading || !fileContent) {
      return <LoadingSpinner />;
    }
    return renderRichDocumentPreview('xlsx', fileContent);
  }
  
  // PPTX 预览
  if (isPptx) {
    if (contentLoading || !fileContent) {
      return <LoadingSpinner />;
    }
    return renderRichDocumentPreview('pptx', fileContent);
  }

  // 纯文本预览
  if (isText) {
    if (contentLoading || !fileContent) {
      return <LoadingSpinner />;
    }
    const textContent = decodeBase64ToText(fileContent) ?? fileContent;
    return (
      <div className="flex flex-col h-full bg-background overflow-hidden">
        <CustomScrollArea className="flex-1">
          <pre className="whitespace-pre-wrap text-sm p-4 m-0 min-h-full font-mono">
            {textContent}
          </pre>
        </CustomScrollArea>
      </div>
    );
  }
  
  // 不支持预览的文件类型（如 PPTX）
  if (isUnsupported) {
    // 从文件名获取扩展名
    const ext = node.name.split('.').pop()?.toUpperCase() || '';
    return (
      <div className="flex flex-col items-center justify-center h-full gap-4">
        <FileText className="w-16 h-16 text-muted-foreground" />
        <div className="text-center space-y-2">
          <p className="text-lg font-medium text-foreground">{node.name}</p>
          <p className="text-muted-foreground">
            {t('learningHub:textbook.unsupportedPreview', { ext })}
          </p>
        </div>
      </div>
    );
  }

  // ======== OCR 状态栏渲染函数 ========

  const renderOcrStatusBar = () => {
    if (ocrAvailability === null) return null;

    // ① OCR 未配置 — 提示用户前往设置
    if (!ocrAvailability.configured) {
      return (
        <div className="flex items-center gap-2 px-4 py-2 bg-amber-50 dark:bg-amber-950/20 border-b border-amber-200 dark:border-amber-800/40">
          <Scan className="h-4 w-4 text-amber-600 dark:text-amber-400 shrink-0" />
          <span className="text-xs text-amber-700 dark:text-amber-300 leading-relaxed">
            {t('textbook:ocr.notConfigured', '这是一份 PDF 文件，如需 OCR 文本提取，请前往「设置 > OCR 引擎」配置模型')}
          </span>
        </div>
      );
    }

    // ★★ 新增状态 0：PDF 已加载，OCR 未开始 — 显示"开始 OCR"按钮
    if (needsOcr && !processingStatus) {
      return (
        <div className="flex items-center gap-2 px-4 py-2 bg-blue-50 dark:bg-blue-950/20 border-b border-blue-200 dark:border-blue-800/40">
          <Scan className="h-4 w-4 text-blue-600 dark:text-blue-400 shrink-0" />
          <span className="text-xs text-blue-700 dark:text-blue-300 leading-relaxed flex-1">
            {t('textbook:ocr.notStarted', '此扫描件 PDF 尚未 OCR 识别，文字内容不可搜索。建议进行 OCR 处理以提取文字')}
          </span>
          <button
            type="button"
            onClick={handleStartOcr}
            disabled={isOcrTriggering}
            className="px-3 py-1 text-xs font-medium rounded-md bg-blue-600 text-white hover:bg-blue-700 transition-colors disabled:opacity-50 shrink-0"
          >
            {isOcrTriggering ? (
              <span className="flex items-center gap-1">
                <CircleNotch className="h-3 w-3 animate-spin" />
                {t('textbook:ocr.starting', '启动中...')}
              </span>
            ) : (
              t('textbook:ocr.start', '开始 OCR 识别')
            )}
          </button>
        </div>
      );
    }

    // ★★ 状态 0.5：pending / error — 可恢复状态，提供手动重试入口
    // pending：进程被杀后 recover_stuck_tasks 重置的残留状态（无任务在跑，进度条会永远卡住）
    // error：上次处理失败（此前该状态不渲染任何状态栏，用户无处重试）
    // 后端 vfs_ensure_ocr_pipeline 已按内存真实运行状态判定，点击即可自愈
    if (processingStatus
      && (processingStatus.stage === 'pending' || processingStatus.stage === 'error')
      && !hasOcrContent
    ) {
      const isError = processingStatus.stage === 'error';
      return (
        <div className="flex items-center gap-2 px-4 py-2 bg-blue-50 dark:bg-blue-950/20 border-b border-blue-200 dark:border-blue-800/40">
          <Scan className="h-4 w-4 text-blue-600 dark:text-blue-400 shrink-0" />
          <span className="text-xs text-blue-700 dark:text-blue-300 leading-relaxed flex-1">
            {isError
              ? (processingStatus.error || t('textbook:ocr.failed', 'OCR 启动失败'))
              : t('textbook:ocr.interrupted', '上次 OCR 处理被中断（如应用被关闭），点击重新继续')}
          </span>
          <button
            type="button"
            onClick={handleStartOcr}
            disabled={isOcrTriggering}
            className="px-3 py-1 text-xs font-medium rounded-md bg-blue-600 text-white hover:bg-blue-700 transition-colors disabled:opacity-50 shrink-0"
          >
            {isOcrTriggering ? (
              <span className="flex items-center gap-1">
                <CircleNotch className="h-3 w-3 animate-spin" />
                {t('textbook:ocr.starting', '启动中...')}
              </span>
            ) : (
              t('textbook:ocr.retry', '重新 OCR 识别')
            )}
          </button>
        </div>
      );
    }

    // ② 处理中 — 显示进度条
    if (processingStatus
      && processingStatus.stage !== 'completed'
      && processingStatus.stage !== 'completed_with_issues'
      && processingStatus.stage !== 'error'
      && processingStatus.stage !== 'pending'
    ) {
      const hint = getProcessingHint(processingStatus);
      const progressPercent = Math.min(100, Math.max(0, processingStatus.percent));
      const currentPage = processingStatus.currentPage;
      const totalPages = processingStatus.totalPages;
      return (
        <div className="px-4 py-2.5 border-b border-border bg-muted/30">
          <div className="flex items-center gap-2 mb-1.5">
            <CircleNotch className="h-3.5 w-3.5 animate-spin text-primary shrink-0" />
            <span className="text-xs text-muted-foreground">{hint}</span>
            {(typeof currentPage === 'number' && typeof totalPages === 'number') && (
              <span className="text-xs text-muted-foreground ml-auto">
                {currentPage}/{totalPages}
              </span>
            )}
          </div>
          <div className="w-full h-1.5 bg-muted rounded-full overflow-hidden">
            <div
              className="h-full bg-primary rounded-full transition-all duration-300 ease-out"
              style={{ width: `${progressPercent}%` }}
            />
          </div>
        </div>
      );
    }

    // ③ OCR 文本就绪（completed / completed_with_issues）— 显示切换开关
    if ((processingStatus?.stage === 'completed' || processingStatus?.stage === 'completed_with_issues' || ocrTextContent)
      && ocrTextContent
    ) {
      return (
        <div className="flex items-center justify-between px-4 py-1.5 border-b border-border bg-muted/20">
          <div className="flex items-center gap-2">
            {processingStatus?.stage === 'completed_with_issues' && (
              <WarningCircle className="h-3.5 w-3.5 text-amber-500 shrink-0" />
            )}
            <span className="text-xs text-muted-foreground">
              {t('textbook:ocr.ready', 'OCR 文本已就绪')}
            </span>
          </div>
          <div className="flex items-center bg-muted rounded-lg p-0.5 gap-0.5">
            <button
              type="button"
              className={`px-2.5 py-1 rounded-md text-xs transition-colors ${
                viewMode === 'pdf'
                  ? 'bg-background shadow-sm font-medium text-foreground'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
              onClick={() => setViewMode('pdf')}
            >
              {t('textbook:ocr.pdfView', 'PDF原图')}
            </button>
            <button
              type="button"
              className={`px-2.5 py-1 rounded-md text-xs transition-colors ${
                viewMode === 'ocr'
                  ? 'bg-background shadow-sm font-medium text-foreground'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
              onClick={() => setViewMode('ocr')}
            >
              {t('textbook:ocr.textView', 'OCR文字')}
            </button>
            <button
              type="button"
              className={`px-2.5 py-1 rounded-md text-xs transition-colors ${
                viewMode === 'split'
                  ? 'bg-background shadow-sm font-medium text-foreground'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
              onClick={() => setViewMode('split')}
            >
              {t('textbook:ocr.splitView', '分屏对比')}
            </button>
            {/* ★★ 重试 OCR 按钮 */}
            <button
              type="button"
              onClick={handleStartOcr}
              disabled={isOcrTriggering}
              className="px-2 py-1 rounded-md text-xs text-muted-foreground hover:text-foreground hover:bg-background transition-colors disabled:opacity-50"
              title={t('textbook:ocr.retry', '重新 OCR 识别')}
            >
              {isOcrTriggering ? (
                <CircleNotch className="h-3 w-3 animate-spin" />
              ) : (
                <ArrowClockwise className="h-3 w-3" />
              )}
            </button>
          </div>
        </div>
      );
    }

    return null;
  };

  // ======== 视图渲染函数 ========

  const renderPdfOnly = () => (
    <TextbookPdfViewer
      file={usePdfStreamFallback ? null : pdfFile}
      // ★ task-051: 直连流式 URL(file 优先,其次 streamUrl,最后 filePath)
      streamUrl={usePdfStreamFallback ? null : pdfStreamUrl}
      // ★ PDF-403 修复：不传 filePath 给教材 PDF，避免触发 pdfstream:// 协议导致 403
      // file 优先于 filePath（TextbookPdfViewer 内部逻辑：file 存在时使用 Blob URL，
      // 仅 file 为 null 时才回退到 filePath 的 pdfstream://）
      // 当 usePdfStreamFallback 激活时，传 filePath 以尝试降级加载。
      filePath={usePdfStreamFallback && filePath ? filePath : ''}
      fileName={node.name}
      // ★ task-048: 补传 fileId(store 键 = sourceId)——此前缺失导致
      // EnhancedPdfViewer 的 OCR 横幅（扫描件提示+启动按钮）在教材页永远不显示
      fileId={node.sourceId}
      selectedPages={selectedPages}
      onPageSelectionChange={handlePageSelectionChange}
      onExportSelectedPages={handleExportSelectedPages}
      focusRequest={focusRequest}
      onFocusHandled={handleFocusHandled}
      readingProgress={liveReadingProgress}
      onProgressChange={handleProgressChange}
      resourcePath={node.path}
      bookmarks={bookmarks}
      onBookmarksChange={handleBookmarksChange}
    />
  );

  // ★ per-page OCR display content: 优先用按页加载的 MD
  // ★ task-048 修复（OCR 文本预览空白根因）：旧逻辑只在 pdf 模式回退全量文本,
  // ocr/split 模式下页 MD 加载失败(旧 schema/缺页/网络)即 content=null → 整页空白。
  // 现在:页 MD 有正文 → 页 MD;页 MD 仅标题(空白页) → 标题+占位说明;
  // 页 MD 不可用 → 回退全量 OCR 文本,绝不无声空白。
  const ocrDisplayContent = useMemo(() => {
    if (ocrPageMd) {
      const body = ocrPageMd.replace(/^# Page \d+\s*/, '').trim();
      if (body) return ocrPageMd;
      return `${ocrPageMd.trimEnd()}\n\n*${t('textbook:ocr.emptyPage', '本页无 OCR 识别文本（可能是空白页或纯图片页）')}*`;
    }
    return ocrTextContent;
  }, [ocrPageMd, ocrTextContent, t]);

  const renderOcrOnly = () => (
    <div className="flex-1 overflow-hidden flex flex-col">
      {isOcrCompleted && (
        <div className="px-3 py-1 text-[10px] text-muted-foreground bg-muted/30 border-b">
          📄 第 {currentPdfPage} 页 OCR
          {ocrPageMdLoading && <span className="ml-2 inline-block animate-pulse">⏳</span>}
        </div>
      )}
      <CustomScrollArea className="flex-1">
        <div className="p-4">
          <MarkdownPreview
            content={ocrDisplayContent || ''}
            loading={ocrPageMdLoading}
            className="text-sm leading-relaxed"
          />
        </div>
      </CustomScrollArea>
    </div>
  );

  const renderSplitView = () => (
    <div className="flex-1 flex overflow-hidden">
      <div className="w-[60%] overflow-hidden border-r border-border">
        <TextbookPdfViewer
          file={usePdfStreamFallback ? null : pdfFile}
          streamUrl={usePdfStreamFallback ? null : pdfStreamUrl}
          filePath={usePdfStreamFallback && filePath ? filePath : ''}
          fileName={node.name}
          fileId={node.sourceId}
          selectedPages={selectedPages}
          onPageSelectionChange={handlePageSelectionChange}
          onExportSelectedPages={handleExportSelectedPages}
          focusRequest={focusRequest}
          onFocusHandled={handleFocusHandled}
          readingProgress={liveReadingProgress}
          onProgressChange={handleProgressChange}
          resourcePath={node.path}
          bookmarks={bookmarks}
          onBookmarksChange={handleBookmarksChange}
        />
      </div>
      <div className="w-[40%] overflow-hidden flex flex-col">
        {isOcrCompleted && (
          <div className="px-3 py-1 text-[10px] text-muted-foreground bg-muted/30 border-b">
            📄 第 {currentPdfPage} 页 OCR
            {ocrPageMdLoading && <span className="ml-2 inline-block animate-pulse">⏳</span>}
          </div>
        )}
        <CustomScrollArea className="flex-1">
          <div className="p-4">
            <MarkdownPreview
              content={ocrDisplayContent || ''}
              loading={ocrPageMdLoading}
              className="text-sm leading-relaxed"
            />
          </div>
        </CustomScrollArea>
      </div>
    </div>
  );

  // PDF 预览（含调试诊断栏）
  return (
    <div className="flex flex-col h-full bg-background">
      {/* ★★ 调试诊断栏：始终显示 OCR 状态和 MD 笔记存在性 */}
      {isPdf && (
        <div className="flex flex-col bg-slate-50 dark:bg-slate-800/30 border-b border-slate-200 dark:border-slate-700 text-[11px] font-mono text-slate-600 dark:text-slate-400 select-none">
          {/* 主状态行 */}
          <div className="flex items-center gap-2 px-3 py-1">
            <span title={node.sourceId}>📄 {node.sourceId?.substring(0, 14) || '?'}...</span>
            <span className="text-slate-300 dark:text-slate-600">|</span>
            <span>OCR:{' '}
              <span className={
                processingStatus?.stage === 'completed' || processingStatus?.stage === 'completed_with_issues'
                  ? 'text-green-600 dark:text-green-400 font-medium'
                  : processingStatus?.stage
                    ? 'text-amber-600 dark:text-amber-400'
                    : 'text-slate-400'
              }>
                {isOcrTriggering ? '⏳ 启动中...' : processingStatus?.stage || 'idle'}
              </span>
            </span>
            {processingStatus?.readyModes?.length > 0 && (
              <span className="text-slate-400">({processingStatus.readyModes.join('/')})</span>
            )}
            {isOcrProcessing && typeof processingStatus?.percent === 'number' && (
              <span className="text-amber-600 dark:text-amber-400">{processingStatus.percent}%</span>
            )}
            <span className="text-slate-300 dark:text-slate-600">|</span>
            <span>MD:{' '}
              {ocrNoteId ? (
                <button
                  onClick={handleOpenOcrNote}
                  className="text-green-600 dark:text-green-400 hover:underline cursor-pointer"
                  title={`第 ${currentPdfPage} 页笔记: ${ocrNoteId}`}
                >
                  ✅ 第{currentPdfPage}页
                </button>
              ) : ocrNoteLoading ? (
                <span className="text-slate-400">⏳</span>
              ) : (
                <span className="text-slate-400">❌ 未创建</span>
              )}
            </span>
            {ocrNoteError && <span className="text-red-400">({String(ocrNoteError)})</span>}
            <span className="text-slate-300 dark:text-slate-600">|</span>
            <span>页码:{' '}{(node.metadata?.pageCount as number | undefined) || processingStatus?.totalPages || '?'}</span>
            <span className="flex-1" />
            <button
              onClick={handleStartOcr}
              disabled={isOcrTriggering || isOcrProcessing}
              className="px-2 py-0.5 rounded text-[10px] bg-slate-200 dark:bg-slate-700 hover:bg-slate-300 dark:hover:bg-slate-600 disabled:opacity-40 transition-colors"
              title={isOcrProcessing ? 'OCR 处理中...' : isOcrTriggering ? '正在启动...' : '重新 OCR'}
            >
              {(isOcrTriggering || isOcrProcessing) ? '⏳' : '🔄'} 重OCR
            </button>
            {ocrNoteId && (
              <button
                onClick={handleOpenOcrNote}
                className="px-2 py-0.5 rounded text-[10px] bg-blue-100 dark:bg-blue-900 hover:bg-blue-200 dark:hover:bg-blue-800 transition-colors"
                title="打开 OCR 笔记"
              >
                📝 打开笔记
              </button>
            )}
          </div>
          {/* ★ 进度条：OCR 处理中时显示 */}
          {(isOcrProcessing || isOcrTriggering) && typeof processingStatus?.percent === 'number' && (
            <div className="w-full h-1 bg-slate-200 dark:bg-slate-700">
              <div
                className="h-full bg-amber-500 dark:bg-amber-400 transition-all duration-500 ease-out"
                style={{ width: `${Math.min(100, Math.max(0, processingStatus.percent))}%` }}
              />
            </div>
          )}
        </div>
      )}
      {isPdf && renderOcrStatusBar()}
      {viewMode === 'split' && (ocrTextContent || ocrPageMd)
        ? renderSplitView()
        : viewMode === 'ocr' && (ocrTextContent || ocrPageMd)
          ? renderOcrOnly()
          : renderPdfOnly()}
    </div>
  );
};

const TextbookContentView: React.FC<ContentViewProps> = (props) => (
  <PreviewProvider>
    <TextbookContentViewInner {...props} />
  </PreviewProvider>
);

export default TextbookContentView;
