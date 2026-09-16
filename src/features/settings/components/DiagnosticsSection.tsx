/**
 * 日志与诊断区块 (task-047)
 *
 * 对标 JetBrains「Help → Diagnostic Tools」与 VS Code「Developer: Set Log Level」：
 * - 日志级别 / Webview 镜像：写 logging.json，重启后生效（tauri-plugin-log 级别构建期固化）
 * - 打开日志目录：统一日志根目录（主日志/crash/frontend 同树）
 * - 导出诊断包：主日志+归档、崩溃日志、前端日志（脱敏+截尾）+ 系统信息打包 zip
 */
import React, { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { FolderOpen, Package } from '@phosphor-icons/react';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { NotionButton } from '@/components/ui/NotionButton';
import { SettingSection } from './SettingsCommon';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/shad/Select';
import { Switch } from '@/components/ui/shad/Switch';
import { TauriAPI } from '@/utils/tauriApi';
import { showGlobalNotification } from '@/components/UnifiedNotification';
import { getErrorMessage } from '@/utils/errorUtils';

interface LoggingPrefs {
  level: string;
  webviewMirror: boolean;
}

const LEVELS = ['error', 'warn', 'info', 'debug', 'trace'] as const;

const DiagnosticsSection: React.FC = () => {
  const { t } = useTranslation('diagnostics');
  const [prefs, setPrefs] = useState<LoggingPrefs>({ level: 'info', webviewMirror: false });
  const [exporting, setExporting] = useState(false);

  useEffect(() => {
    (async () => {
      try {
        const p = await TauriAPI.invoke<LoggingPrefs>('logging_get_prefs');
        setPrefs({ level: p.level || 'info', webviewMirror: !!p.webviewMirror });
      } catch (err) {
        console.warn('[Diagnostics] load prefs failed:', err);
      }
    })();
  }, []);

  const savePrefs = useCallback(async (next: LoggingPrefs) => {
    setPrefs(next);
    try {
      await TauriAPI.invoke('logging_set_prefs', {
        level: next.level,
        webviewMirror: next.webviewMirror,
      });
      showGlobalNotification('success', t('saved_restart_note'));
    } catch (err) {
      showGlobalNotification('error', t('save_failed', { error: getErrorMessage(err) }));
    }
  }, [t]);

  const handleOpenDir = useCallback(async () => {
    try {
      await TauriAPI.invoke<string>('open_log_dir');
      showGlobalNotification('success', t('open_dir_done'));
    } catch (err) {
      showGlobalNotification('error', getErrorMessage(err));
    }
  }, [t]);

  const handleExport = useCallback(async () => {
    setExporting(true);
    try {
      const path = await TauriAPI.invoke<string>('export_diagnostics_bundle');
      showGlobalNotification('success', t('export_done', { path }));
      // 在文件管理器中选中该 zip（失败不打断——导出已成功）
      try {
        await revealItemInDir(path);
      } catch {
        /* best-effort */
      }
    } catch (err) {
      showGlobalNotification('error', t('export_failed', { error: getErrorMessage(err) }));
    } finally {
      setExporting(false);
    }
  }, [t]);

  return (
    <SettingSection title={t('title')} description={t('description')}>
      <div className="space-y-4">
        {/* 日志级别 */}
        <div className="flex items-center justify-between gap-4">
          <div className="min-w-0">
            <div className="text-sm text-foreground">{t('level_label')}</div>
            <div className="text-xs text-muted-foreground mt-0.5">{t('level_note')}</div>
          </div>
          <Select
            value={prefs.level}
            onValueChange={(level) => void savePrefs({ ...prefs, level })}
          >
            <SelectTrigger className="w-44 shrink-0">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {LEVELS.map((lv) => (
                <SelectItem key={lv} value={lv}>
                  {t(`levels.${lv}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        {/* Webview 镜像 */}
        <div className="flex items-center justify-between gap-4">
          <div className="min-w-0">
            <div className="text-sm text-foreground">{t('mirror_label')}</div>
            <div className="text-xs text-muted-foreground mt-0.5">{t('mirror_note')}</div>
          </div>
          <Switch
            checked={prefs.webviewMirror}
            onCheckedChange={(webviewMirror) => void savePrefs({ ...prefs, webviewMirror })}
          />
        </div>

        {/* 操作按钮 */}
        <div className="flex items-center gap-2 pt-1">
          <NotionButton size="sm" variant="ghost" onClick={() => void handleOpenDir()}>
            <FolderOpen className="w-4 h-4 mr-1" />
            {t('open_dir')}
          </NotionButton>
          <NotionButton
            size="sm"
            variant="ghost"
            disabled={exporting}
            onClick={() => void handleExport()}
          >
            <Package className="w-4 h-4 mr-1" />
            {exporting ? t('exporting') : t('export')}
          </NotionButton>
        </div>
      </div>
    </SettingSection>
  );
};

export default DiagnosticsSection;
