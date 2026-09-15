/**
 * 新模型发现区块 (task-046)
 *
 * 后端 model_watch 每日巡检各供应商 /models 端点，把本地尚未配置的新模型
 * 持久化到 settings 并 emit `model-watch:discovered`。本组件展示待处理列表：
 * - 添加：生成「停用状态的草稿」模型条目，用户在模型列表完善参数后启用
 * - 忽略：加入忽略清单，巡检不再报告该模型
 */
import React, { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { ArrowClockwise, Plus, Sparkle, X } from '@phosphor-icons/react';
import { NotionButton } from '@/components/ui/NotionButton';
import { SettingSection } from './SettingsCommon';
import { TauriAPI } from '@/utils/tauriApi';
import { showGlobalNotification } from '@/components/UnifiedNotification';
import { getErrorMessage } from '@/utils/errorUtils';
import { cn } from '@/lib/utils';

interface DiscoveredModel {
  vendorId: string;
  vendorName: string;
  modelId: string;
  label: string;
  discoveredAt: string;
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

const DISCOVERED_EVENT = 'model-watch:discovered';

const itemKey = (item: Pick<DiscoveredModel, 'vendorId' | 'modelId'>) =>
  `${item.vendorId}::${item.modelId.toLowerCase()}`;

const ModelWatchSection: React.FC = () => {
  const { t } = useTranslation('modelwatch');
  const [lastRunAt, setLastRunAt] = useState<string | null>(null);
  const [pending, setPending] = useState<DiscoveredModel[]>([]);
  const [running, setRunning] = useState(false);
  const [actingKey, setActingKey] = useState<string | null>(null);

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

  const handleAdd = useCallback(async (item: DiscoveredModel) => {
    const key = itemKey(item);
    setActingKey(key);
    try {
      await TauriAPI.invoke('model_watch_add_as_draft', {
        vendorId: item.vendorId,
        modelId: item.modelId,
      });
      setPending((prev) => prev.filter((p) => itemKey(p) !== key));
      showGlobalNotification('success', t('added_draft'));
    } catch (err) {
      showGlobalNotification('error', t('add_failed', { error: getErrorMessage(err) }));
    } finally {
      setActingKey(null);
    }
  }, [t]);

  const handleDismiss = useCallback(async (item: DiscoveredModel) => {
    const key = itemKey(item);
    setActingKey(key);
    try {
      await TauriAPI.invoke('model_watch_dismiss', {
        vendorId: item.vendorId,
        modelId: item.modelId,
      });
      setPending((prev) => prev.filter((p) => itemKey(p) !== key));
      showGlobalNotification('success', t('dismissed'));
    } catch (err) {
      showGlobalNotification('error', t('add_failed', { error: getErrorMessage(err) }));
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
            return (
              <div
                key={key}
                className="flex items-center gap-2 py-1.5 px-2 rounded-md border border-border/40 bg-muted/20"
              >
                <Sparkle className="w-3.5 h-3.5 text-amber-500 flex-shrink-0" />
                <div className="min-w-0 flex-1">
                  <div className="text-sm truncate" title={item.modelId}>
                    {item.label && item.label !== item.modelId
                      ? `${item.label} (${item.modelId})`
                      : item.modelId}
                  </div>
                  <div className="text-xs text-muted-foreground truncate">
                    {item.vendorName} · {formatTime(item.discoveredAt, true)}
                  </div>
                </div>
                <NotionButton
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={() => void handleAdd(item)}
                >
                  <Plus className="w-3.5 h-3.5 mr-0.5" />
                  {t('add_draft')}
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
