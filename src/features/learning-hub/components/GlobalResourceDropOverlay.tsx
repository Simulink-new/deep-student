/**
 * 全局资源仓库拖拽入库 overlay
 *
 * 挂在 ViewLayer 之外，任意桌面页面拖入文件都能看到提示。
 * 落点命中 Chat 输入框、访达、试卷/翻译/作文等专项区域时让路，避免双写。
 */

import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { UploadSimple } from '@phosphor-icons/react';
import { FILE_TYPES } from '@/components/shared/UnifiedDragDropZone';
import { showGlobalNotification } from '@/components/UnifiedNotification';
import { ensureGlobalDragHandlers, isNativeDropRecent, markNativeDrop } from '@/hooks/useTauriDragAndDrop';
import { guardedListen } from '@/utils/guardedListen';
import { debugLog } from '@/debug-panel/debugMasterSwitch';
import { getErrorMessage } from '@/utils/errorUtils';
import { useViewStore } from '@/stores/viewStore';
import { useFinderStore } from '../stores/finderStore';
import { getCreatableFolderId } from '../viewGuards';
import { getQuickAccessTypeFromPath } from '../learningHubContracts';
import { hasVisibleLocalDropClaim, isPointOverLocalDropClaim, isDragDropBlockedView } from '../dragDropRouting';
import {
  classifyDroppedName,
  importDroppedResources,
  isResourceDropImporting,
} from '../resourceDropImport';
import { ImportProgressModal, type ImportProgressState } from './ImportProgressModal';

const ACCEPTED_TYPES = [FILE_TYPES.IMAGE, FILE_TYPES.DOCUMENT, FILE_TYPES.AUDIO, FILE_TYPES.VIDEO];

function isAcceptedDropName(name: string): boolean {
  return classifyDroppedName(name) !== 'unsupported';
}

export const GlobalResourceDropOverlay: React.FC = () => {
  const { t } = useTranslation(['drag_drop', 'learningHub']);
  const [isDragging, setIsDragging] = useState(false);
  const [importProgress, setImportProgress] = useState<ImportProgressState>({
    isImporting: false,
    fileName: '',
    stage: 'hashing',
    progress: 0,
  });
  const lastProcessedRef = useRef<{ key: string; timestamp: number } | null>(null);

  const handleImportComplete = useCallback((successCount: number) => {
    if (successCount <= 0) return;
    const currentView = useViewStore.getState().currentView;
    if (currentView !== 'learning-hub') {
      window.dispatchEvent(new CustomEvent('NAVIGATE_TO_VIEW', { detail: { view: 'learning-hub' } }));
    }
    void useFinderStore.getState().refresh();
  }, []);

  const runImport = useCallback(async (payload: { paths?: string[]; files?: File[] }) => {
    if (isResourceDropImporting()) return;

    const currentView = useViewStore.getState().currentView;
    const finderState = useFinderStore.getState();
    const currentPath = finderState.currentPath;
    if (currentView === 'learning-hub' && isDragDropBlockedView(currentPath)) {
      showGlobalNotification('warning', t('learningHub:finder.dragDrop.notAllowedHere', '当前视图不支持拖入文件'));
      return;
    }

    const folderId = getCreatableFolderId(currentPath);
    const markdownAsNotes = currentView === 'learning-hub' && getQuickAccessTypeFromPath(currentPath) === 'notes';

    try {
      const result = await importDroppedResources({
        paths: payload.paths,
        files: payload.files,
        folderId,
        markdownAsNotes,
        onTextbookProgress: setImportProgress,
      });

      const unsupportedHint = result.unsupportedNames.length > 0
        ? `\n${t('learningHub:finder.dragDrop.unsupportedFiles', '不支持：{{names}}', {
            names: result.unsupportedNames.slice(0, 3).join('、') + (result.unsupportedNames.length > 3 ? '…' : ''),
          })}`
        : '';

      if (result.successCount > 0 && result.failedCount === 0) {
        showGlobalNotification('success',
          t('learningHub:finder.dragDrop.importSuccess', '已导入 {{count}} 个文件', { count: result.successCount }) + unsupportedHint
        );
      } else if (result.successCount > 0) {
        showGlobalNotification('warning',
          t('learningHub:finder.dragDrop.importPartial', '导入 {{success}} 个成功，{{failed}} 个失败', {
            success: result.successCount,
            failed: result.failedCount,
          }) + unsupportedHint
        );
      } else if (result.unsupportedNames.length > 0 && result.failedCount === 0) {
        showGlobalNotification('warning', t('drag_drop:errors.unsupported_type') + unsupportedHint);
      } else if (result.failedCount > 0 || result.unsupportedNames.length > 0) {
        showGlobalNotification('error', t('learningHub:finder.dragDrop.importFailed', '文件导入失败') + unsupportedHint);
      }

      if (result.successCount > 0) {
        handleImportComplete(result.successCount);
        const first = result.firstImportedNode;
        if (first) {
          window.dispatchEvent(new CustomEvent('learningHubOpenResource', {
            detail: { dstuPath: first.path || `/${first.id}` },
          }));
        }
      }
    } catch (error) {
      debugLog.error('[GlobalResourceDrop] 导入失败:', error);
      setImportProgress(prev => ({ ...prev, isImporting: false }));
      showGlobalNotification('error', t('drag_drop:errors.processing_failed', { error: getErrorMessage(error) }));
    }
  }, [handleImportComplete, t]);

  const processPaths = useCallback((paths: string[]) => {
    const unique = [...new Set(paths.filter(Boolean))];
    if (unique.length === 0) return;

    const now = Date.now();
    const key = JSON.stringify([...unique].sort());
    if (lastProcessedRef.current && lastProcessedRef.current.key === key && now - lastProcessedRef.current.timestamp < 400) {
      return;
    }
    lastProcessedRef.current = { key, timestamp: now };

    const accepted = unique.filter((path) => isAcceptedDropName(path.split(/[/\\]/).pop() || path));
    const rejected = unique.length - accepted.length;
    if (rejected > 0) {
      showGlobalNotification('warning', t('drag_drop:errors.some_files_rejected', { count: rejected }));
    }
    if (accepted.length === 0) {
      showGlobalNotification('warning', t('drag_drop:errors.unsupported_type'));
      return;
    }
    void runImport({ paths: accepted });
  }, [runImport, t]);

  useEffect(() => {
    ensureGlobalDragHandlers();

    let unlisten: (() => void) | undefined;
    const unlisteners: Array<() => void> = [];
    let disposed = false;

    const shouldClaim = (pos?: { x: number; y: number }): boolean => {
      if (!pos) return !hasVisibleLocalDropClaim();
      return !isPointOverLocalDropClaim(pos.x, pos.y);
    };

    const setup = async () => {
      try {
        const webview = getCurrentWebview();
        const nextUnlisten = await webview.onDragDropEvent((event) => {
          const payload = event.payload as {
            type: 'enter' | 'over' | 'leave' | 'drop' | 'cancel';
            paths?: string[];
            position?: { x: number; y: number };
          };
          const isEndEvent = payload.type === 'leave' || payload.type === 'cancel' || payload.type === 'drop';
          const claim = shouldClaim(payload.position);

          switch (payload.type) {
            case 'enter':
            case 'over':
              setIsDragging(claim);
              break;
            case 'leave':
            case 'cancel':
              setIsDragging(false);
              break;
            case 'drop':
              setIsDragging(false);
              if (!claim) return;
              markNativeDrop();
              if (payload.paths?.length) processPaths(payload.paths);
              break;
          }

          if (isEndEvent && !claim) {
            setIsDragging(false);
          }
        });
        if (disposed) {
          nextUnlisten();
          return;
        }
        unlisten = nextUnlisten;
      } catch {
        unlisteners.push(await guardedListen('tauri://drag-enter', () => {
          if (!hasVisibleLocalDropClaim()) setIsDragging(true);
        }));
        unlisteners.push(await guardedListen('tauri://drag-leave', () => setIsDragging(false)));
        unlisteners.push(await guardedListen('tauri://drag-drop', (event: { payload?: { paths?: string[] } }) => {
          setIsDragging(false);
          if (hasVisibleLocalDropClaim()) return;
          markNativeDrop();
          const paths = event?.payload?.paths;
          if (paths?.length) processPaths(paths);
        }));
        unlisteners.push(await guardedListen('tauri://file-drop-hover', () => {
          if (!hasVisibleLocalDropClaim()) setIsDragging(true);
        }));
        unlisteners.push(await guardedListen('tauri://file-drop-cancelled', () => setIsDragging(false)));
        unlisteners.push(await guardedListen('tauri://file-drop', (event: { payload?: string[] | { paths?: string[] } }) => {
          setIsDragging(false);
          if (hasVisibleLocalDropClaim()) return;
          markNativeDrop();
          const paths = Array.isArray(event?.payload) ? event.payload : event?.payload?.paths;
          if (paths?.length) processPaths(paths);
        }));
      }
    };

    const handleWebDrop = (e: DragEvent) => {
      if (!e.dataTransfer?.types?.includes('Files')) return;
      if (isNativeDropRecent()) return;
      if (isPointOverLocalDropClaim(e.clientX, e.clientY)) return;
      e.preventDefault();
      setIsDragging(false);
      const files = Array.from(e.dataTransfer.files || []);
      if (files.length === 0) return;
      const accepted = files.filter((file) => isAcceptedDropName(file.name));
      const rejected = files.length - accepted.length;
      if (rejected > 0) {
        showGlobalNotification('warning', t('drag_drop:errors.some_files_rejected', { count: rejected }));
      }
      if (accepted.length === 0) {
        showGlobalNotification('warning', t('drag_drop:errors.unsupported_type'));
        return;
      }
      void runImport({ files: accepted });
    };

    const handleWebDragOver = (e: DragEvent) => {
      if (!e.dataTransfer?.types?.includes('Files')) return;
      const claim = !isPointOverLocalDropClaim(e.clientX, e.clientY);
      setIsDragging(claim);
    };

    const handleWebDragLeave = (e: DragEvent) => {
      if (e.relatedTarget == null) setIsDragging(false);
    };

    document.addEventListener('drop', handleWebDrop, true);
    document.addEventListener('dragover', handleWebDragOver);
    document.addEventListener('dragleave', handleWebDragLeave);
    void setup();

    return () => {
      disposed = true;
      try { unlisten?.(); } catch { /* ignore */ }
      unlisteners.forEach((fn) => { try { fn(); } catch { /* ignore */ } });
      document.removeEventListener('drop', handleWebDrop, true);
      document.removeEventListener('dragover', handleWebDragOver);
      document.removeEventListener('dragleave', handleWebDragLeave);
    };
  }, [processPaths, runImport, t]);

  const formats = ACCEPTED_TYPES.map((ft) => t(`drag_drop:file_types.${ft.description.toLowerCase()}`, ft.description)).join(' / ');

  return (
    <>
      {isDragging && (
        <div
          className="pointer-events-none fixed inset-0 z-[80] flex items-center justify-center"
          style={{ backgroundColor: 'hsl(var(--primary) / 0.12)', backdropFilter: 'blur(6px)' }}
          data-zone-id="global-resource-drop"
        >
          <div
            className="flex flex-col items-center gap-3 rounded-2xl px-10 py-8 shadow-xl"
            style={{ backgroundColor: 'hsl(var(--background))', border: '2px dashed hsl(var(--primary))' }}
          >
            <UploadSimple size={36} className="text-primary" />
            <div className="text-lg font-medium text-center" style={{ color: 'hsl(var(--foreground))' }}>
              {t('drag_drop:overlay.drop_to_library', '拖放到此处导入资源仓库')}
            </div>
            <div className="text-sm text-center" style={{ color: 'hsl(var(--muted-foreground))' }}>
              {t('drag_drop:overlay.supported_types', '支持 {{formats}}', { formats })}
            </div>
          </div>
        </div>
      )}
      <ImportProgressModal
        state={importProgress}
        onClose={() => setImportProgress(prev => ({ ...prev, isImporting: false }))}
      />
    </>
  );
};

export default GlobalResourceDropOverlay;
