/**
 * Chat V2 统计数据 Hook
 *
 * 提供 Chat V2 会话的统计数据，用于数据统计页面展示
 */

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { t } from '../utils/i18n';

// ============================================================================
// 类型定义
// ============================================================================
// (ChatSession/MessageSummary 接口已删——统计聚合下推 SQL 后不再拉会话/消息明细, task-038)

/**
 * 会话活动数据（按天统计）
 */
export interface DailyActivity {
  date: string;
  displayDate: string;
  sessions: number;
  messages: number;
}

/**
 * 会话模式分布
 */
export interface ModeDistribution {
  mode: string;
  count: number;
  label: string;
}

/**
 * 时间段分布（按小时）
 */
export interface HourlyDistribution {
  hour: number;
  count: number;
}

/**
 * Chat V2 完整统计数据
 */
export interface ChatV2Stats {
  // 总体统计
  totalSessions: number;
  activeSessions: number;
  archivedSessions: number;
  totalMessages: number;
  userMessages: number;
  assistantMessages: number;

  // 近期统计
  recentSessions: number; // 最近7天创建的会话
  recentMessages: number; // 最近7天的消息

  // 分布统计
  modeDistribution: ModeDistribution[];
  dailyActivity: DailyActivity[];
  hourlyDistribution: HourlyDistribution[];

  // 计算指标
  avgMessagesPerSession: number;
  avgSessionsPerDay: number;

  // 状态
  loading: boolean;
  error: string | null;
}

// ============================================================================
// 模式标签映射
// ============================================================================

const getModeLabel = (mode: string): string => {
  const key = `chat_modes.${mode}`;
  const translated = t(key);
  return translated !== key ? translated : mode;
};

const getWeekdayLabel = (dayIndex: number): string => {
  const days = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];
  return t(`weekdays.${days[dayIndex]}`);
};

// ============================================================================
// Hook 实现
// ============================================================================

/**
 * 获取 Chat V2 统计数据
 */
export function useChatV2Stats(autoRefresh = false, refreshInterval = 30000): ChatV2Stats {
  const [stats, setStats] = useState<ChatV2Stats>({
    totalSessions: 0,
    activeSessions: 0,
    archivedSessions: 0,
    totalMessages: 0,
    userMessages: 0,
    assistantMessages: 0,
    recentSessions: 0,
    recentMessages: 0,
    modeDistribution: [],
    dailyActivity: [],
    hourlyDistribution: [],
    avgMessagesPerSession: 0,
    avgSessionsPerDay: 0,
    loading: true,
    error: null,
  });

  const loadStats = useCallback(async () => {
    try {
      // ★ perf-audit B1/task-038: 聚合全部下推 SQL（chat_v2_get_session_stats）
      // 旧路径每次拉 2×1000 全量会话到前端内存聚合（~0.6MB IPC），
      // 且依赖的 chat_v2_get_message_summary 为幽灵命令（后端不存在，invoke 必失败走估算）
      const s = await invoke<{
        totalSessions: number;
        activeSessions: number;
        archivedSessions: number;
        recentSessions7d: number;
        modeDistribution: Array<{ mode: string; count: number }>;
        dailyActivity7d: Array<{ date: string; sessions: number }>;
        hourlyDistribution: number[];
        messageSummary: { totalMessages: number; userMessages: number; assistantMessages: number };
      }>('chat_v2_get_session_stats');

      const now = new Date();
      const modeDistribution: ModeDistribution[] = (s.modeDistribution || [])
        .map((m) => ({ mode: m.mode, count: m.count, label: getModeLabel(m.mode) }));

      // 每日活动: SQL 稀疏结果填充 7 天脚手架(展示语义不变)
      const dailyMap = new Map((s.dailyActivity7d || []).map((d) => [d.date, d.sessions]));
      const dailyActivity: DailyActivity[] = [];
      for (let i = 6; i >= 0; i--) {
        const date = new Date(now);
        date.setDate(date.getDate() - i);
        const dateStr = date.toISOString().split('T')[0];
        dailyActivity.push({
          date: dateStr,
          displayDate: `${t('weekdays.prefix', { defaultValue: '周' })}${getWeekdayLabel(date.getDay())}`,
          sessions: dailyMap.get(dateStr) ?? 0,
          messages: 0, // 消息数按天统计仍不可用(与旧行为一致)
        });
      }

      const hourlyDistribution: HourlyDistribution[] = (s.hourlyDistribution || new Array(24).fill(0)).map(
        (count, hour) => ({ hour, count })
      );

      const { totalMessages, userMessages, assistantMessages } = s.messageSummary;
      const recentSessions = s.recentSessions7d;

      // 计算平均值
      const avgMessagesPerSession =
        s.totalSessions > 0
          ? Math.round((totalMessages / s.totalSessions) * 10) / 10
          : 0;

      const avgSessionsPerDay = Math.round((recentSessions / 7) * 10) / 10;

      setStats({
        totalSessions: s.totalSessions,
        activeSessions: s.activeSessions,
        archivedSessions: s.archivedSessions,
        totalMessages,
        userMessages,
        assistantMessages,
        recentSessions,
        recentMessages: Math.floor(totalMessages * 0.3), // 估算(与旧行为一致)
        modeDistribution,
        dailyActivity,
        hourlyDistribution,
        avgMessagesPerSession,
        avgSessionsPerDay,
        loading: false,
        error: null,
      });
    } catch (error: unknown) {
      console.error('[useChatV2Stats] Failed to load stats:', error);
      setStats((prev) => ({
        ...prev,
        loading: false,
        error: error instanceof Error ? error.message : t('messages.error.load_stats_failed'),
      }));
    }
  }, []);

  // 初始加载
  useEffect(() => {
    loadStats();
  }, [loadStats]);

  // 自动刷新
  useEffect(() => {
    if (!autoRefresh) return;

    const interval = setInterval(loadStats, refreshInterval);
    return () => clearInterval(interval);
  }, [autoRefresh, refreshInterval, loadStats]);

  return stats;
}

/**
 * 手动刷新统计数据的 Hook
 */
export function useChatV2StatsRefresh() {
  const [refreshKey, setRefreshKey] = useState(0);

  const refresh = useCallback(() => {
    setRefreshKey((k) => k + 1);
  }, []);

  return { refreshKey, refresh };
}

export default useChatV2Stats;
