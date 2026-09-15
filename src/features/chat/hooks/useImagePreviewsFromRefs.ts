/**
 * Chat V2 - useImagePreviewsFromRefs Hook
 *
 * 从上下文引用中提取图片引用，并异步获取图片内容用于预览显示。
 *
 * ★ VFS 引用模式改造（2025-12-10）
 * 新架构下，图片以引用形式存储在 `_meta.contextSnapshot.userRefs` 中：
 * 1. ContextRef.resourceId 指向 resources 表 (res_xxx)
 * 2. Resource.data 存储 VfsContextRefData JSON（只有引用，无实际内容）
 * 3. 需要通过 vfs_resolve_resource_refs 获取真实图片 base64
 *
 * ★ B3#4 图片预览缓存改造（2026-09）
 * 取数链迁移至 context/imagePreviewCache：
 * - 模块级两级 LRU（元数据 + data URL 负载）+ in-flight 并发去重
 * - 元数据层并行获取（原为串行 for-await N 次 IPC）
 * - 虚拟列表滚回重挂载命中缓存，零后端调用
 *
 * @example
 * ```tsx
 * const { imagePreviews, isLoading } = useImagePreviewsFromRefs(message._meta?.contextSnapshot);
 * ```
 */

import { useState, useEffect, useMemo } from 'react';
import i18next from 'i18next';
import type { ContextSnapshot, ContextRef } from '../context/types';
import { getErrorMessage } from '@/utils/errorUtils';
import { VfsErrorCode } from '@/shared/result';
import { resolveImagePreviews } from '../context/imagePreviewCache';

// ============================================================================
// 类型定义
// ============================================================================

/**
 * 图片预览数据
 */
export interface ImagePreview {
  /** 引用 ID（resourceId） */
  id: string;
  /** 图片名称 */
  name: string;
  /** MIME 类型 */
  mimeType: string;
  /** 预览 URL（data URL 或 blob URL） */
  previewUrl: string;
  /** 原始引用 */
  ref: ContextRef;
}

/**
 * Hook 返回值
 */
export interface UseImagePreviewsFromRefsResult {
  /** 图片预览列表 */
  imagePreviews: ImagePreview[];
  /** 是否正在加载 */
  isLoading: boolean;
  /** 加载错误信息 */
  error: string | null;
}

// ============================================================================
// 辅助函数
// ============================================================================

/**
 * 检查是否为图片类型引用
 */
function isImageRef(ref: ContextRef): boolean {
  return ref.typeId === 'image';
}

// ============================================================================
// Hook 实现
// ============================================================================

/**
 * 从上下文引用中获取图片预览
 *
 * @param contextSnapshot 上下文快照
 * @returns 图片预览列表和加载状态
 */
export function useImagePreviewsFromRefs(
  contextSnapshot?: ContextSnapshot
): UseImagePreviewsFromRefsResult {
  const [imagePreviews, setImagePreviews] = useState<ImagePreview[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 提取图片类型的引用
  const imageRefs = useMemo(() => {
    if (!contextSnapshot?.userRefs) return [];
    return contextSnapshot.userRefs.filter(isImageRef);
  }, [contextSnapshot]);

  // 异步加载图片内容
  useEffect(() => {
    if (imageRefs.length === 0) {
      setImagePreviews([]);
      setIsLoading(false);
      setError(null);
      return;
    }

    let isMounted = true;
    const abortController = new AbortController();

    const loadImages = async () => {
      setIsLoading(true);
      setError(null);

      // 🔧 调试：开始加载图片
      window.dispatchEvent(new CustomEvent('debug:chatv2-image-preview', {
        detail: {
          stage: 'load_start',
          imageRefsCount: imageRefs.length,
          imageRefs: imageRefs.map(r => ({ resourceId: r.resourceId, typeId: r.typeId })),
        }
      }));

      try {
        // ★ B3#4: 全链走 imagePreviewCache（元数据/负载 LRU + 并发去重 + 并行元数据获取）
        const { resolutions, notFoundSourceIds } = await resolveImagePreviews(imageRefs);

        if (abortController.signal.aborted || !isMounted) return;

        // 部分资源未找到的提示（不中断整个流程）
        if (notFoundSourceIds.length > 0) {
          setError(i18next.t('chatV2:imagePreview.partialNotFound'));
        }

        // 按解析结果组装预览（顺序与引用顺序一致，同图重复引用保持重复显示）
        const previews: ImagePreview[] = [];
        for (const resolution of resolutions) {
          if (resolution.status !== 'ok') continue;
          previews.push({
            id: resolution.contextRef.resourceId,
            name: resolution.entry.name,
            mimeType: resolution.entry.mimeType,
            previewUrl: resolution.entry.previewUrl,
            ref: resolution.contextRef,
          });
        }

        if (isMounted) {
          // 🔧 调试：加载完成
          window.dispatchEvent(new CustomEvent('debug:chatv2-image-preview', {
            detail: {
              stage: 'load_complete',
              previewsCount: previews.length,
              previews: previews.map(p => ({
                id: p.id,
                name: p.name,
                mimeType: p.mimeType,
                previewUrlLength: p.previewUrl?.length || 0,
                previewUrlPrefix: p.previewUrl?.substring(0, 50),
              })),
            }
          }));

          setImagePreviews(previews);
          setIsLoading(false);
        }
      } catch (err: unknown) {
        // 整批解析失败：按错误类型提示（与原实现一致）
        if (!isMounted) return;
        let errorMessage = i18next.t('chatV2:imagePreview.loadFailed');
        const code = (err as { code?: VfsErrorCode } | null)?.code;
        if (code === VfsErrorCode.NOT_FOUND) {
          errorMessage = i18next.t('chatV2:imagePreview.notFound');
        } else if (code === VfsErrorCode.NETWORK) {
          errorMessage = i18next.t('chatV2:imagePreview.networkError');
        } else if (code === VfsErrorCode.PERMISSION) {
          errorMessage = i18next.t('chatV2:imagePreview.permissionDenied');
        } else {
          console.error('[useImagePreviewsFromRefs] Failed to resolve image refs:', getErrorMessage(err));
        }
        setError(errorMessage);
        setIsLoading(false);
      }
    };

    loadImages();

    return () => {
      isMounted = false;
      abortController.abort();
    };
  }, [imageRefs]);

  return { imagePreviews, isLoading, error };
}

export default useImagePreviewsFromRefs;
