/**
 * 媒体处理进度监听 Hook（PDF + 图片）
 * 
 * 监听后端发送的媒体处理进度事件，更新全局状态。
 * 统一事件（media-processing-*）；旧 pdf-processing-* 双发已删除（双写 store/双日志）
 * 
 * @version 2.0 扩展支持图片处理
 */

import { useEffect } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { usePdfProcessingStore, type MediaType, type ProcessingStage } from '@/features/pdf/stores/pdfProcessingStore';
import { invalidateResourceCache } from '@/features/chat/context/vfsRefApiEnhancements';
import { showGlobalNotification } from '@/components/UnifiedNotification';
import { debugLog } from '../debug-panel/debugMasterSwitch';

const console = debugLog as Pick<typeof debugLog, 'log' | 'warn' | 'error' | 'info' | 'debug'>;

/**
 * 媒体处理进度事件 payload
 * 注意：后端使用 #[serde(rename_all = "camelCase")]，所以字段名为 camelCase
 */
interface MediaProcessingProgressPayload {
  fileId: string;
  status: {
    stage: ProcessingStage;
    currentPage?: number;
    totalPages?: number;
    percent: number;
    readyModes: string[];
    mediaType?: MediaType;
  };
  mediaType: MediaType;
}

/**
 * 媒体处理完成事件 payload
 * 注意：后端使用 #[serde(rename_all = "camelCase")]，所以字段名为 camelCase
 */
interface MediaProcessingCompletedPayload {
  fileId: string;
  readyModes: Array<'text' | 'image' | 'ocr'>;
  stage?: 'completed' | 'completed_with_issues';
  mediaType: MediaType;
  /** OCR 完成后自动创建的笔记 ID（note_xxx） */
  noteId?: string;
}

/**
 * 媒体处理错误事件 payload
 * 注意：后端使用 #[serde(rename_all = "camelCase")]，所以字段名为 camelCase
 */
interface MediaProcessingErrorPayload {
  fileId: string;
  error: string;
  stage: string;
  mediaType: MediaType;
}

// 兼容旧类型别名（已随 pdf-processing-* 事件删除，仅保留统一类型）

/**
 * 监听媒体处理进度事件
 * 
 * 在需要监听处理进度的组件中调用此 Hook。
 * 推荐在 InputBarUI 或其父组件中调用。
 * 
 * ★ 关键调试点：
 * - fileId 应为 sourceId (att_xxx)，与后端发送的 file_id 一致
 * - 事件更新的 key 应与前端查询的 key 一致
 */
export function usePdfProcessingProgress(): void {
  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;

    const registerListener = async <T,>(
      eventName: Parameters<typeof listen<T>>[0],
      handler: Parameters<typeof listen<T>>[1],
    ) => {
      try {
        const unlisten = await listen<T>(eventName, handler);
        if (disposed) {
          unlisten();
          return;
        }
        unlisteners.push(unlisten);
      } catch (error) {
        console.error(`[MediaProcessing] Failed to register ${eventName} listener:`, error);
      }
    };
    
    console.log('[MediaProcessing] Hook 初始化，开始监听事件...');
    
    // 处理进度事件的通用处理器
    const handleProgress = (payload: MediaProcessingProgressPayload, source: 'unified' | 'legacy') => {
      const { fileId, status, mediaType } = payload;
      
      // ★ 详细日志：方便调试 key 匹配问题
      console.log(`[MediaProcessing] 📥 Progress (${source}):`, {
        fileId,
        mediaType,
        stage: status.stage,
        percent: Math.round(status.percent),
        readyModes: status.readyModes,
        page: status.currentPage && status.totalPages ? `${status.currentPage}/${status.totalPages}` : undefined,
      });
      
      // ★ 检查 Store 更新前后状态
      const beforeState = usePdfProcessingStore.getState().get(fileId);
      
      // ★ N1 修复：当 readyModes 新增时，失效旧的 resolveCache
      const prevModes = new Set(beforeState?.readyModes || []);
      const nextModes = (status.readyModes || []) as Array<'text' | 'ocr' | 'image'>;
      const hasNewModes = nextModes.some(m => !prevModes.has(m));

      // ★ P1-1 修复（二轮审阅）：先更新 Store，再失效缓存
      // 避免竞态窗口：invalidation 和 store.update 之间若有 resolveVfsRefs 调用，
      // 会用旧的 readyModes 重新缓存过期结果。
      usePdfProcessingStore.getState().update(fileId, {
        stage: status.stage,
        currentPage: status.currentPage,
        totalPages: status.totalPages,
        percent: status.percent,
        readyModes: nextModes,
        mediaType: mediaType || status.mediaType,
      });

      if (hasNewModes) {
        const invalidated = invalidateResourceCache(fileId);
        console.log(`[MediaProcessing] 🗑️ New readyModes detected, cache invalidated for ${fileId}: ${invalidated} entries (new: ${nextModes.join(',')})`);
      }
      
      const afterState = usePdfProcessingStore.getState().get(fileId);
      console.log(`[MediaProcessing] 📊 Store 更新:`, {
        fileId,
        before: beforeState?.stage,
        after: afterState?.stage,
        storeSize: usePdfProcessingStore.getState().statusMap.size,
      });
    };
    
    // 处理完成事件的通用处理器
    const handleCompleted = (payload: MediaProcessingCompletedPayload, source: 'unified' | 'legacy') => {
      const { fileId, readyModes, mediaType, stage, noteId } = payload;

      console.log(`[MediaProcessing] ✅ Completed (${source}):`, {
        fileId,
        mediaType,
        stage,
        readyModes,
        noteId,
      });

      usePdfProcessingStore.getState().setCompleted(fileId, readyModes, stage);

      // ★ N1 修复：处理完成时失效 resolveCache，防止后续发送使用旧的无 OCR/text 缓存
      const invalidated = invalidateResourceCache(fileId);
      console.log(`[MediaProcessing] 🗑️ Cache invalidated for ${fileId}: ${invalidated} entries`);

      // ★ OCR→笔记：如果后端创建了 OCR 笔记，通知用户并刷新学习资源库
      if (noteId) {
        console.log(`[MediaProcessing] 📝 OCR note created: ${noteId}`);
        const title = 'OCR 识别完成';
        const msg = '已自动生成笔记文件，可在学习资源库中查看';
        showGlobalNotification('success', msg, title, {
          action: {
            label: '打开笔记',
            onClick: () => {
              window.dispatchEvent(new CustomEvent('learningHubOpenNote', {
                detail: { noteId, title: msg },
              }));
            },
          },
        });
        // ★★ 修复：用事件驱动刷新，比动态 import 更可靠
        // dstu.watch 会自动监听后端变更事件，但加一次主动刷新作为保险
        window.dispatchEvent(new CustomEvent('dstu:refresh', { detail: { source: 'ocr-note' } }));
      }

      console.log(`[MediaProcessing] 📊 Store 完成状态:`, {
        fileId,
        state: usePdfProcessingStore.getState().get(fileId),
      });
    };
    
    // 处理错误事件的通用处理器
    const handleError = (payload: MediaProcessingErrorPayload, source: 'unified' | 'legacy') => {
      const { fileId, error, stage, mediaType } = payload;
      
      console.error(`[MediaProcessing] ❌ Error (${source}):`, {
        fileId,
        mediaType,
        stage,
        error,
      });
      
      usePdfProcessingStore.getState().setError(fileId, error, stage);
    };
    
    // 监听统一事件（后端已停发旧 pdf-processing-* 双发）
    void registerListener<MediaProcessingProgressPayload>('media-processing-progress', (event) => {
      handleProgress(event.payload, 'unified');
    });

    void registerListener<MediaProcessingCompletedPayload>('media-processing-completed', (event) => {
      handleCompleted(event.payload, 'unified');
    });

    void registerListener<MediaProcessingErrorPayload>('media-processing-error', (event) => {
      handleError(event.payload, 'unified');
    });

    // legacy pdf_ocr_service.rs: map pdf_ocr_progress events to store updates
    void registerListener<LegacyOcrProgressPayload>('pdf_ocr_progress', (event) => {
      const payload = event.payload;
      const fileId = ocrSessionToFileId.get(payload.session_id);
      if (!fileId) {
        console.debug('[MediaProcessing] pdf_ocr_progress for unknown session, ignoring:', payload.session_id);
        return;
      }
      const logCtx = { sessionId: payload.session_id, fileId, type: payload.type };

      switch (payload.type) {
        case 'PageRendered':
        case 'Started':
        case 'RenderStarted':
        case 'PageCompleted': {
          const total = payload.total ?? payload.total_pages;
          const completed = payload.completed ?? payload.rendered;
          const percent = total ? Math.min(100, Math.round(((completed ?? 0) / total) * 100)) : 0;
          console.log('[MediaProcessing] pdf_ocr_progress:', logCtx, { percent, completed, total });
          usePdfProcessingStore.getState().update(fileId, {
            stage: 'ocr_processing',
            percent,
            currentPage: payload.page_index ?? 0,
            totalPages: total,
            mediaType: 'pdf',
          });
          break;
        }
        case 'Completed': {
          console.log('[MediaProcessing] pdf_ocr_progress completed:', logCtx, payload);
          const stage = payload.has_failures ? 'completed_with_issues' : 'completed';
          usePdfProcessingStore.getState().setCompleted(fileId, ['ocr'], stage);
          break;
        }
        case 'PageFailed':
        case 'PageRenderFailed': {
          console.warn('[MediaProcessing] pdf_ocr_progress error:', logCtx, payload.error);
          usePdfProcessingStore.getState().setError(fileId, payload.error ?? 'OCR page failed', 'ocr_processing');
          break;
        }
        case 'Retrying':
          console.log('[MediaProcessing] pdf_ocr_progress retrying:', logCtx);
          break;
        default:
          console.debug('[MediaProcessing] pdf_ocr_progress unhandled type:', logCtx);
      }
    });

    console.log('[MediaProcessing] Hook 初始化完成，已注册 7 个事件监听器');
    
    // 清理
    return () => {
      disposed = true;
      console.log('[MediaProcessing] Hook 清理，移除事件监听器...');
      unlisteners.forEach(unlisten => unlisten());
    };
  }, []);
}

// ========================================================================
// 旧版 OCR 服务 (pdf_ocr_service.rs) 事件 session_id → fileId 映射
// ========================================================================

/** 旧版 OCR 服务使用 session_id 而非 fileId，此映射将两者关联 */
const ocrSessionToFileId = new Map<string, string>();

/**
 * 注册 OCR 会话映射（由调用 start_pdf_ocr_backend 的代码在获取 session_id 后调用）
 * @param fileId 文件 ID (sourceId)
 * @param sessionId 后端返回的 OCR 会话 ID
 */
export function registerOcrSession(fileId: string, sessionId: string): void {
  ocrSessionToFileId.set(sessionId, fileId);
}

/**
 * 旧版 pdf_ocr_progress 事件 payload（非严格类型，仅取需要的字段）
 */
interface LegacyOcrProgressPayload {
  type: string;
  session_id: string;
  page_index?: number;
  rendered?: number;
  completed?: number;
  total?: number;
  total_pages?: number;
  error?: string;
  has_failures?: boolean;
  cancelled?: boolean;
  [key: string]: unknown;
}

// ========================================================================
// 兼容旧 Hook 名称
// ========================================================================

export const useMediaProcessingProgress = usePdfProcessingProgress;

/**
 * 获取指定文件的处理状态（非响应式）
 */
export function getPdfProcessingStatusFromStore(fileId: string) {
  return usePdfProcessingStore.getState().get(fileId);
}
