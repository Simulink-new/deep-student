/**
 * 新模型发现区块 (task-046)
 *
 * 后端 model_watch 每日巡检各供应商 /models 端点，把本地尚未配置的新模型
 * 持久化到 settings 并 emit `model-watch:discovered`。本组件展示待处理列表：
 * - 同一模型被多个供应商上架（含 NVIDIA NIM 等聚合平台的带前缀变体）时
 *   合并为一条，来源以 chip 呈现，点击切换「使用」的目标供应商（默认原生
 *   供应商，聚合平台排后）
 * - 使用：在目标供应商下直接创建启用状态的模型条目（model_watch_adopt_model），
 *   成功后广播 api_configurations_changed 让设置页/聊天模型选择器即时刷新
 * - 忽略：按模型加入忽略清单（对该模型的所有供应商来源生效）
 */
import React, { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { ArrowClockwise, Lightning, Sparkle, X } from '@phosphor-icons/react';
import { NotionButton } from '@/components/ui/NotionButton';
import { SettingSection } from './SettingsCommon';
import { TauriAPI } from '@/utils/tauriApi';
import { showGlobalNotification } from '@/components/UnifiedNotification';
import { getErrorMessage } from '@/utils/errorUtils';
import { cn } from '@/lib/utils';

interface VendorSource {
  vendorId: string;
  vendorName: string;
  /** 该供应商下的实际模型 id（聚合平台可能带厂商前缀，如 zai/glm-4.6） */
  modelId: string;
}

interface DiscoveredModel {
  modelId: string;
  label: string;
  discoveredAt: string;
  sources: VendorSource[];
}

interface ModelWatchState {
  lastRunAt: string | null;
  pending: DiscoveredModel[];
  running: boolean;
}

interface ModelWatchRunSummary {
  vendorsChecked: number;
  vendorsFailed: number;
  newFound: number;
}

interface ModelWatchAdoptResult {
  created: boolean;
  vendorHasKey: boolean;
}

const DISCOVERED_EVENT = 'model-watch:discovered';

/** 与后端 short_model_name 一致：去掉聚合平台厂商前缀，小写，作跨来源去重键 */
const shortName = (modelId: string) => {
  const idx = modelId.lastIndexOf('/');
  return (idx >= 0 ? modelId.slice(idx + 1) : modelId).toLowerCase();
};

const itemKey = (item: Pick<DiscoveredModel, 'modelId'>) => shortName(item.modelId);

const ModelWatchSection: React.FC = () => {
  const { t } = useTranslation('modelwatch');
  const [lastRunAt, setLastRunAt] = useState<string | null>(null);
  const [pending, setPending] = useState<DiscoveredModel[]>([]);
  const [running, setRunning] = useState(false);
  const [actingKey, setActingKey] = useState<string | null>(null);
  /** 条目去重键 → 用户选中的目标供应商 id（缺省取来源首位 = 原生供应商） */
  const [selectedVendorByKey, setSelectedVendorByKey] = useState<Record<string, string>>({});

  const loadState = useCallback(async () => {
    try {
      const state = await TauriAPI.invoke<ModelWatchState>('model_watch_get_state');
      setLastRunAt(state.lastRunAt ?? null);
      setPending(Array.isArray(state.pending) ? state.pending : []);
      setRunning(!!state.running);
    } catch (err) {
      // 静默失败：巡检是辅助功能，不打扰设置页主流程
      console.warn('[ModelWatch] get_state failed:', err);
    }
  }, []);

  useEffect(() => {
    void loadState();
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
    listen<DiscoveredModel[]>(DISCOVERED_EVENT, (event) => {
      const fresh = Array.isArray(event.payload) ? event.payload : [];
      if (fresh.length === 0) return;
      setPending((prev) => {
        const existing = new Set(prev.map(itemKey));
        const merged = [...prev];
        for (const item of fresh) {
          if (!existing.has(itemKey(item))) merged.push(item);
        }
        return merged;
      });
      showGlobalNotification('info', t('event_new', { count: fresh.length }));
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [loadState, t]);

  const handleRunNow = useCallback(async () => {
    setRunning(true);
    try {
      const summary = await TauriAPI.invoke<ModelWatchRunSummary>('model_watch_run_now');
      // 以服务端权威状态刷新（事件只推送增量，这里拿全量兜底）
      await loadState();
      if (summary.newFound > 0) {
        showGlobalNotification('success', t('run_done', {
          checked: summary.vendorsChecked,
          count: summary.newFound,
        }));
      } else {
        showGlobalNotification('info', t('run_done_none', { checked: summary.vendorsChecked }));
      }
    } catch (err) {
      showGlobalNotification('error', t('run_failed', { error: getErrorMessage(err) }));
    } finally {
      setRunning(false);
    }
  }, [loadState, t]);

  const handleAdopt = useCallback(async (item: DiscoveredModel, source: VendorSource) => {
    const key = itemKey(item);
    setActingKey(key);
    try {
      const result = await TauriAPI.invoke<ModelWatchAdoptResult>('model_watch_adopt_model', {
        vendorId: source.vendorId,
        modelId: source.modelId,
      });
      setPending((prev) => prev.filter((p) => itemKey(p) !== key));
      // 供应商/模型列表的消费者（设置页 useVendorModels、聊天模型选择器等）
      // 都监听该事件并从后端重拉，这里派发即完成「放入模型供应商」的即时刷新
      window.dispatchEvent(new CustomEvent('api_configurations_changed'));
      showGlobalNotification(
        result.vendorHasKey ? 'success' : 'warning',
        result.vendorHasKey
          ? t('adopted', { vendor: source.vendorName })
          : t('adopted_no_key', { vendor: source.vendorName }),
      );
    } catch (err) {
      showGlobalNotification('error', t('adopt_failed', { error: getErrorMessage(err) }));
    } finally {
      setActingKey(null);
    }
  }, [t]);

  const handleDismiss = useCallback(async (item: DiscoveredModel) => {
    const key = itemKey(item);
    setActingKey(key);
    try {
      await TauriAPI.invoke('model_watch_dismiss', {
        vendorId: item.sources[0]?.vendorId ?? '',
        modelId: item.modelId,
      });
      setPending((prev) => prev.filter((p) => itemKey(p) !== key));
      showGlobalNotification('success', t('dismissed'));
    } catch (err) {
      showGlobalNotification('error', t('adopt_failed', { error: getErrorMessage(err) }));
    } finally {
      setActingKey(null);
    }
  }, [t]);

  const formatTime = (iso: string, dateOnly = false) => {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return dateOnly ? d.toLocaleDateString() : d.toLocaleString();
  };

  return (
    <SettingSection
      title={t('title')}
      description={t('description')}
      rightSlot={
        <NotionButton
          size="sm"
          variant="ghost"
          onClick={handleRunNow}
          disabled={running}
        >
          <ArrowClockwise className={cn('w-4 h-4 mr-1', running && 'animate-spin')} />
          {running ? t('checking') : t('check_now')}
        </NotionButton>
      }
    >
      <div className="text-xs text-muted-foreground mb-2">
        {lastRunAt ? t('last_run', { time: formatTime(lastRunAt) }) : t('never_run')}
      </div>

      {pending.length === 0 ? (
        <div className="text-sm text-muted-foreground/70 py-1">{t('no_new_models')}</div>
      ) : (
        <div className="space-y-1">
          {pending.map((item) => {
            const key = itemKey(item);
            const busy = actingKey === key;
            const selectedVendorId =
              selectedVendorByKey[key] ?? item.sources[0]?.vendorId;
            const selectedSource =
              item.sources.find((s) => s.vendorId === selectedVendorId) ??
              item.sources[0];
            const displayId = selectedSource?.modelId ?? item.modelId;
            return (
              <div
                key={key}
                className="flex items-center gap-2 py-1.5 px-2 rounded-md border border-border/40 bg-muted/20"
              >
                <Sparkle className="w-3.5 h-3.5 text-amber-500 flex-shrink-0" />
                <div className="min-w-0 flex-1">
                  <div className="text-sm truncate" title={displayId}>
                    {item.label && item.label !== shortName(item.modelId) && item.label !== item.modelId
                      ? `${item.label} (${displayId})`
                      : displayId}
                  </div>
                  <div className="text-xs text-muted-foreground truncate flex items-center flex-wrap gap-1">
                    <span className="flex-shrink-0">{formatTime(item.discoveredAt, true)}</span>
                    {item.sources.map((s) => (
                      <button
                        key={s.vendorId}
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          setSelectedVendorByKey((prev) => ({ ...prev, [key]: s.vendorId }))
                        }
                        title={t('choose_vendor', { vendor: s.vendorName })}
                        className={cn(
                          'px-1.5 py-0.5 rounded border text-xs transition-colors',
                          s.vendorId === selectedVendorId
                            ? 'border-primary/50 bg-primary/10 text-foreground'
                            : 'border-border/40 text-muted-foreground hover:text-foreground',
                        )}
                      >
                        {s.vendorName}
                      </button>
                    ))}
                  </div>
                </div>
                <NotionButton
                  size="sm"
                  variant="ghost"
                  disabled={busy || !selectedSource}
                  onClick={() => selectedSource && void handleAdopt(item, selectedSource)}
                >
                  <Lightning className="w-3.5 h-3.5 mr-0.5" />
                  {t('use')}
                </NotionButton>
                <NotionButton
                  size="sm"
                  variant="ghost"
                  iconOnly
                  aria-label={t('dismiss')}
                  disabled={busy}
                  onClick={() => void handleDismiss(item)}
                >
                  <X className="w-3.5 h-3.5" />
                </NotionButton>
              </div>
            );
          })}
        </div>
      )}
    </SettingSection>
  );
};

export default ModelWatchSection;
