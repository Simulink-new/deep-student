/**
 * Chat V2 - 消息图片预览缓存（B3#4）
 *
 * useImagePreviewsFromRefs 的原始取数链（每次组件挂载都完整执行）：
 *   1. 串行 N 次 resourceStoreApi.get（vfs_get_resource 元数据 IPC）
 *   2. 一次 vfs_resolve_resource_refs —— 全量 base64 过 IPC
 *   3. buildImageDataUrl —— 全量字符串再拷贝一次
 * 虚拟列表滚出视口卸载 MessageItem、滚回重挂载，即全链重做，无任何缓存。
 *
 * 本模块在取数链外层加两级模块级 LRU + 并发去重：
 * - 元数据缓存：resourceId → image 类型的 VfsResourceRef[]（小对象，纯防重复 IPC）
 * - 负载缓存：sourceId → 已构建 data URL（容量 100 条 / 64MB 字节上限）
 * - 同 key in-flight Promise 共享：并发请求只发一次后端调用
 *
 * 图片内容不可变（附件上传后 sourceId 内容寻址），因此无需 TTL/失效逻辑。
 * task-035 的 pdfstream 协议迁移只覆盖 PDF 视图渲染链，与本链无关。
 */

import type { ContextRef, VfsResourceRef } from './types';
import { resourceStoreApi } from '../resources';
import { resolveResourceRefsV2 } from './vfsRefApi';
import { buildImageDataUrl } from './imagePayload';

// ============================================================================
// 类型定义
// ============================================================================

/** 已缓存的图片预览负载（存最终 data URL，跨渲染零拷贝复用同一字符串引用） */
export interface CachedImagePreviewEntry {
  name: string;
  mimeType: string;
  previewUrl: string;
}

/** 单条图片引用的解析结果 */
export type ImagePreviewResolution =
  | { status: 'ok'; sourceId: string; entry: CachedImagePreviewEntry; contextRef: ContextRef }
  | { status: 'not_found'; sourceId: string; contextRef: ContextRef }
  | { status: 'invalid'; sourceId: string; contextRef: ContextRef };

/** resolveImagePreviews 的聚合结果 */
export interface ResolveImagePreviewsResult {
  /** 按原始引用顺序排列的成功预览 */
  resolutions: ImagePreviewResolution[];
  /** 未找到的 sourceId 列表（用于上层提示部分缺失） */
  notFoundSourceIds: string[];
}

// ============================================================================
// 缓存状态（模块级单例）
// ============================================================================

/** 元数据 LRU：resourceId → image-only VfsResourceRef[] */
const META_CACHE_MAX_ENTRIES = 200;
const metaCache = new Map<string, VfsResourceRef[]>();
const metaInflight = new Map<string, Promise<VfsResourceRef[]>>();

/** 负载 LRU：sourceId → 已构建 data URL */
const PREVIEW_CACHE_MAX_ENTRIES = 100;
const PREVIEW_CACHE_MAX_BYTES = 64 * 1024 * 1024;
/** 单条超限不缓存（直接返回，避免一图挤掉整缓存） */
const PREVIEW_SINGLE_MAX_BYTES = 8 * 1024 * 1024;

const previewCache = new Map<string, CachedImagePreviewEntry>();
const previewInflight = new Map<string, Promise<CachedImagePreviewEntry | null>>();
let previewCacheBytes = 0;

// ============================================================================
// 内部工具
// ============================================================================

/** LRU 触碰：Map 迭代序即淘汰序，命中时 delete+set 移到最新端 */
function touchCacheEntry<V>(cache: Map<string, V>, key: string, value: V): void {
  cache.delete(key);
  cache.set(key, value);
}

// ============================================================================
// 元数据层：resourceId → image VfsResourceRef[]
// ============================================================================

/**
 * 获取某个 ContextRef 指向的 image 类型 VfsResourceRef 列表（带缓存 + 并发去重）。
 *
 * 与 useFilePreviewsFromRefs / 原 useImagePreviewsFromRefs 的 Step 1 等价：
 * resourceStoreApi.get(resourceId) → JSON.parse(data) → 过滤 type === 'image'，
 * 并强制 image-only 注入模式（预览只要原图，避免 OCR 混合内容）。
 * 失败（资源不存在/格式错误）返回空数组且不缓存，行为与原实现一致。
 */
async function getImageRefsForResource(resourceId: string): Promise<VfsResourceRef[]> {
  const cached = metaCache.get(resourceId);
  if (cached) {
    touchCacheEntry(metaCache, resourceId, cached);
    return cached;
  }

  const inflight = metaInflight.get(resourceId);
  if (inflight) return inflight;

  const promise = (async (): Promise<VfsResourceRef[]> => {
    try {
      const resource = await resourceStoreApi.get(resourceId);
      if (!resource?.data) {
        console.warn('[imagePreviewCache] Resource not found or empty:', resourceId);
        return [];
      }
      const refData = JSON.parse(resource.data) as { refs?: VfsResourceRef[] };
      const imageRefs = (refData.refs ?? []).filter((r) => r.type === 'image');
      if (imageRefs.length > 0) {
        while (metaCache.size >= META_CACHE_MAX_ENTRIES) {
          const oldest = metaCache.keys().next().value;
          if (oldest === undefined) break;
          metaCache.delete(oldest);
        }
        metaCache.set(resourceId, imageRefs);
      }
      return imageRefs;
    } catch (err) {
      console.error('[imagePreviewCache] Failed to get resource:', resourceId, err);
      return [];
    } finally {
      metaInflight.delete(resourceId);
    }
  })();

  metaInflight.set(resourceId, promise);
  return promise;
}

// ============================================================================
// 负载层：sourceId → data URL
// ============================================================================

function cachePreviewEntry(sourceId: string, entry: CachedImagePreviewEntry): void {
  const itemBytes = entry.previewUrl.length;
  if (itemBytes > PREVIEW_SINGLE_MAX_BYTES) return;

  while (previewCache.size > 0 && previewCacheBytes + itemBytes > PREVIEW_CACHE_MAX_BYTES) {
    const oldest = previewCache.keys().next().value;
    if (oldest === undefined) break;
    const evicted = previewCache.get(oldest);
    previewCache.delete(oldest);
    previewCacheBytes -= evicted ? evicted.previewUrl.length : 0;
  }
  while (previewCache.size >= PREVIEW_CACHE_MAX_ENTRIES) {
    const oldest = previewCache.keys().next().value;
    if (oldest === undefined) break;
    const evicted = previewCache.get(oldest);
    previewCache.delete(oldest);
    previewCacheBytes -= evicted ? evicted.previewUrl.length : 0;
  }

  const existing = previewCache.get(sourceId);
  if (existing) previewCacheBytes -= existing.previewUrl.length;

  previewCache.set(sourceId, entry);
  previewCacheBytes += itemBytes;
}

/**
 * 批量解析未命中的图片引用（一次 invoke），结果写入负载缓存。
 * 返回 sourceId → entry 映射；未找到/负载非法的 sourceId 不在映射中。
 */
async function fetchPreviewBatch(
  missing: VfsResourceRef[]
): Promise<Map<string, CachedImagePreviewEntry>> {
  const resolvedMap = new Map<string, CachedImagePreviewEntry>();
  if (missing.length === 0) return resolvedMap;

  const result = await resolveResourceRefsV2(missing);
  if (!result.ok) {
    // 解析失败：不缓存，交由上层按错误码提示（与原实现的错误路径一致）
    console.error('[imagePreviewCache] Failed to resolve VFS refs:', result.error);
    throw result.error ?? new Error('resolve failed');
  }

  for (const resolved of result.value) {
    if (!resolved.found || !resolved.content) {
      console.warn('[imagePreviewCache] VFS resource not found:', resolved.sourceId);
      continue;
    }
    const mimeType =
      (resolved.metadata as { mimeType?: string } | undefined)?.mimeType || 'image/png';
    const previewUrl = buildImageDataUrl(resolved.content, mimeType);
    if (!previewUrl) {
      console.warn(
        '[imagePreviewCache] Skip preview due to invalid image payload:',
        resolved.sourceId
      );
      continue;
    }
    const entry: CachedImagePreviewEntry = { name: resolved.name, mimeType, previewUrl };
    cachePreviewEntry(resolved.sourceId, entry);
    resolvedMap.set(resolved.sourceId, entry);
  }
  return resolvedMap;
}

// ============================================================================
// 对外 API
// ============================================================================

/**
 * 解析一组图片 ContextRef 为预览负载（全链缓存 + 并发去重）。
 *
 * 流程：
 * 1. 元数据层并行获取（替代原串行 for-await，且命中缓存零 IPC）
 * 2. 负载层先查缓存/在途请求，未命中的 sourceId 合并为一次批量 invoke
 * 3. 按原始引用顺序组装结果
 *
 * @param imageRefs 图片类型的 ContextRef 列表
 * @throws 当批量解析整体失败时（个别缺失/非法不抛，走 not_found/invalid 状态）
 */
export async function resolveImagePreviews(imageRefs: ContextRef[]): Promise<ResolveImagePreviewsResult> {
  // Phase 1: 元数据（并行 + 缓存 + 去重）
  const refPairsList = await Promise.all(
    imageRefs.map(async (contextRef) => {
      const refs = await getImageRefsForResource(contextRef.resourceId);
      return refs.map((vfsRef) => ({
        contextRef,
        vfsRef: { ...vfsRef, injectModes: { image: ['image'] } } as VfsResourceRef,
      }));
    })
  );
  const refPairs = refPairsList.flat();

  // Phase 2: 负载——先缓存/在途，再合并剩余为一批
  const resolvedEntries = new Map<string, Promise<CachedImagePreviewEntry | null>>();
  const toFetch: VfsResourceRef[] = [];

  for (const { vfsRef } of refPairs) {
    if (resolvedEntries.has(vfsRef.sourceId)) continue;
    const cached = previewCache.get(vfsRef.sourceId);
    if (cached) {
      touchCacheEntry(previewCache, vfsRef.sourceId, cached);
      resolvedEntries.set(vfsRef.sourceId, Promise.resolve(cached));
      continue;
    }
    const inflight = previewInflight.get(vfsRef.sourceId);
    if (inflight) {
      resolvedEntries.set(vfsRef.sourceId, inflight);
      continue;
    }
    toFetch.push(vfsRef);
  }

  if (toFetch.length > 0) {
    // 整批失败时向上传播：调用方（hook）按 VfsErrorCode 分类提示，
    // 与原实现"整次 resolve 失败 → loadFailed"的语义一致。
    // Phase 3 的 Promise.all 会给每个 perSource 挂 handler，无 unhandled rejection。
    const batchPromise = fetchPreviewBatch(toFetch).finally(() => {
      for (const ref of toFetch) previewInflight.delete(ref.sourceId);
    });

    for (const ref of toFetch) {
      const perSource = batchPromise.then((m) => m.get(ref.sourceId) ?? null);
      previewInflight.set(ref.sourceId, perSource);
      resolvedEntries.set(ref.sourceId, perSource);
    }
  }

  // Phase 3: 等待全部落定，按原顺序组装
  const resolutions: ImagePreviewResolution[] = [];
  const notFoundSourceIds: string[] = [];
  const invalidSourceIds = new Set<string>();
  const entryById = new Map<string, CachedImagePreviewEntry>();

  await Promise.all(
    Array.from(resolvedEntries.entries()).map(async ([sourceId, promise]) => {
      const entry = await promise;
      if (entry) entryById.set(sourceId, entry);
      else invalidSourceIds.add(sourceId);
    })
  );

  for (const { vfsRef, contextRef } of refPairs) {
    const entry = entryById.get(vfsRef.sourceId);
    if (entry) {
      resolutions.push({ status: 'ok', sourceId: vfsRef.sourceId, entry, contextRef });
    } else if (invalidSourceIds.has(vfsRef.sourceId)) {
      // 区分不出"未找到"与"负载非法"（两者都不产生 entry）；统一按未找到上报，
      // 上层提示语义不变（partialNotFound）
      resolutions.push({ status: 'not_found', sourceId: vfsRef.sourceId, contextRef });
    }
  }
  for (const id of invalidSourceIds) notFoundSourceIds.push(id);

  return { resolutions, notFoundSourceIds };
}

/** 清空图片预览缓存（调试/测试用） */
export function clearImagePreviewCache(): void {
  metaCache.clear();
  metaInflight.clear();
  previewCache.clear();
  previewInflight.clear();
  previewCacheBytes = 0;
}

/** 缓存统计（调试用） */
export function getImagePreviewCacheStats(): {
  metaEntries: number;
  previewEntries: number;
  previewBytes: number;
  previewMaxEntries: number;
  previewMaxBytes: number;
} {
  return {
    metaEntries: metaCache.size,
    previewEntries: previewCache.size,
    previewBytes: previewCacheBytes,
    previewMaxEntries: PREVIEW_CACHE_MAX_ENTRIES,
    previewMaxBytes: PREVIEW_CACHE_MAX_BYTES,
  };
}
