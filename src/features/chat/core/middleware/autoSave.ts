/**
 * Chat V2 - 自动保存中间件
 *
 * 提供节流保存和强制立即保存功能。
 *
 * 约束：
 * 1. 节流保存：500ms 内最多保存一次
 * 2. 流式结束时调用 forceImmediateSave
 * 3. 保存操作不应阻塞 UI
 */

import i18next from 'i18next';
import type { ChatStore } from '../types';
import { showGlobalNotification } from '@/components/UnifiedNotification';
import { debugLog } from '@/debug-panel/debugMasterSwitch';
import {
  AUTO_SAVE_THROTTLE_MS,
  SAVE_FAILURE_NOTIFICATION_THROTTLE_MS,
} from '../constants';

export interface AutoSaveMiddleware {
  scheduleAutoSave(store: ChatStore): void;
  forceImmediateSave(store: ChatStore): Promise<void>;
  cancelPendingSave(sessionId: string): void;
  hasPendingSave(sessionId: string): boolean;
  cleanup(sessionId: string): void;
}

export interface AutoSaveConfig {
  throttleMs: number;
  debug: boolean;
}

const DEFAULT_CONFIG: AutoSaveConfig = {
  throttleMs: AUTO_SAVE_THROTTLE_MS,
  debug: false,
};

const console = debugLog as Pick<typeof debugLog, 'log' | 'warn' | 'error' | 'info' | 'debug'>;

// ============================================================================
// 实现
// ============================================================================

/**
 * 自动保存中间件实现
 */
class AutoSaveMiddlewareImpl implements AutoSaveMiddleware {
  private config: AutoSaveConfig;
  private pendingTimers: Map<string, ReturnType<typeof setTimeout>> = new Map();
  private lastSaveTimes: Map<string, number> = new Map();
  private savingPromises: Map<string, Promise<void>> = new Map();

  constructor(config: Partial<AutoSaveConfig> = {}) {
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 调度节流保存
   */
  scheduleAutoSave(store: ChatStore): void {
    const sessionId = store.sessionId;
    const now = Date.now();
    const lastSaveTime = this.lastSaveTimes.get(sessionId) ?? 0;
    const timeSinceLastSave = now - lastSaveTime;

    // 如果距离上次保存不足 throttleMs，设置延迟保存
    if (timeSinceLastSave < this.config.throttleMs) {
      // 取消之前的待执行保存
      this.cancelPendingSave(sessionId);

      // 计算需要延迟的时间
      const delay = this.config.throttleMs - timeSinceLastSave;

      if (this.config.debug) {
        console.log(
          `[AutoSave] Scheduling save for session ${sessionId} in ${delay}ms`
        );
      }

      const timer = setTimeout(() => {
        this.pendingTimers.delete(sessionId);
        this.executeSave(store);
      }, delay);

      this.pendingTimers.set(sessionId, timer);
    } else {
      // 立即执行保存
      this.executeSave(store);
    }
  }

  /**
   * 强制立即保存
   */
  async forceImmediateSave(store: ChatStore): Promise<void> {
    const sessionId = store.sessionId;

    // 取消待执行的保存
    this.cancelPendingSave(sessionId);

    if (this.config.debug) {
      console.log(`[AutoSave] Force immediate save for session ${sessionId}`);
    }

    // 等待正在进行的保存完成
    const existingPromise = this.savingPromises.get(sessionId);
    if (existingPromise) {
      await existingPromise;
    }

    // 执行保存
    await this.executeSaveAsync(store);
  }

  /**
   * 取消待执行的保存
   */
  cancelPendingSave(sessionId: string): void {
    const timer = this.pendingTimers.get(sessionId);
    if (timer) {
      clearTimeout(timer);
      this.pendingTimers.delete(sessionId);

      if (this.config.debug) {
        console.log(`[AutoSave] Cancelled pending save for session ${sessionId}`);
      }
    }
  }

  /**
   * 检查是否有待执行的保存
   */
  hasPendingSave(sessionId: string): boolean {
    return this.pendingTimers.has(sessionId);
  }

  /**
   * 执行保存（同步调用，不等待）
   */
  private executeSave(store: ChatStore): void {
    const sessionId = store.sessionId;

    // 如果正在保存，跳过
    if (this.savingPromises.has(sessionId)) {
      if (this.config.debug) {
        console.log(`[AutoSave] Save already in progress for session ${sessionId}`);
      }
      return;
    }

    // 更新最后保存时间
    this.lastSaveTimes.set(sessionId, Date.now());

    // 异步执行保存，支持失败重试（最多1次）
    const attemptSave = async (retryCount = 0): Promise<void> => {
      try {
        await store.saveSession();
      } catch (error) {
        if (retryCount < 1) {
          console.warn(`[AutoSave] Save failed for session ${sessionId}, retrying in 2s...`, error);
          await new Promise(resolve => setTimeout(resolve, 2000));
          return attemptSave(retryCount + 1);
        }
        console.error(`[AutoSave] Save failed after retry for session ${sessionId}:`, error);
        const now = Date.now();
        const lastNotifyKey = `autoSave_lastNotify_${sessionId}`;
        const lastNotify = (this as any)[lastNotifyKey] || 0;
        if (now - lastNotify > SAVE_FAILURE_NOTIFICATION_THROTTLE_MS) {
          (this as any)[lastNotifyKey] = now;
          showGlobalNotification('warning', i18next.t('chatV2:error.saveFailedDesc'));
        }
      }
    };
    const savePromise = attemptSave()
      .finally(() => {
        this.savingPromises.delete(sessionId);
      });

    this.savingPromises.set(sessionId, savePromise);

    if (this.config.debug) {
      console.log(`[AutoSave] Executing save for session ${sessionId}`);
    }
  }

  /**
   * 执行保存（异步，等待完成）
   */
  private async executeSaveAsync(store: ChatStore): Promise<void> {
    const sessionId = store.sessionId;

    // 更新最后保存时间
    this.lastSaveTimes.set(sessionId, Date.now());

    const savePromise = store.saveSession().finally(() => {
      this.savingPromises.delete(sessionId);
    });

    this.savingPromises.set(sessionId, savePromise);

    await savePromise;

    if (this.config.debug) {
      console.log(`[AutoSave] Save completed for session ${sessionId}`);
    }
  }

  /**
   * 清理会话相关的状态
   */
  cleanup(sessionId: string): void {
    this.cancelPendingSave(sessionId);
    this.lastSaveTimes.delete(sessionId);
    this.savingPromises.delete(sessionId);
  }

  /**
   * 更新配置
   */
  updateConfig(config: Partial<AutoSaveConfig>): void {
    this.config = { ...this.config, ...config };
  }
}

// ============================================================================
// 单例导出
// ============================================================================

/**
 * 自动保存中间件单例
 */
export const autoSave: AutoSaveMiddleware = new AutoSaveMiddlewareImpl();

/**
 * 创建自动保存中间件实例（用于测试）
 */
export function createAutoSaveMiddleware(
  config?: Partial<AutoSaveConfig>
): AutoSaveMiddleware & { cleanup: (sessionId: string) => void; updateConfig: (config: Partial<AutoSaveConfig>) => void } {
  return new AutoSaveMiddlewareImpl(config);
}


// ============================================================================
// A11/B3: StreamingBlockSaver(流式块防闪退保存器)已删除
// —— 5s 全量回声 IPC 链路早已在 P0-b 下沉为 Rust 侧 PeriodicBlockPersister,
//    本文件的单例(scheduleBlockSave 全库零调用,仅 60s 空转 interval)与
//    TauriAdapter 侧的回调接线一并移除。
// ============================================================================
