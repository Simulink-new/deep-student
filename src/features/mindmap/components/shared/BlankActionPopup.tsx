import React, { useEffect, useRef, useCallback, useLayoutEffect, useState } from 'react';
import { NotionButton } from '@/components/ui/NotionButton';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { EyeSlash, Eye } from '@phosphor-icons/react';

interface BlankActionPopupProps {
  x: number;
  y: number;
  isAlreadyBlanked: boolean;
  onBlank: () => void;
  onUnblank: () => void;
  onClose: () => void;
}

export const BlankActionPopup: React.FC<BlankActionPopupProps> = ({
  x,
  y,
  isAlreadyBlanked,
  onBlank,
  onUnblank,
  onClose,
}) => {
  const { t } = useTranslation('mindmap');
  const ref = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);

  useLayoutEffect(() => {
    const popup = ref.current;
    if (!popup) return;
    const width = popup.offsetWidth;
    const height = popup.offsetHeight;
    const padding = 8;
    const maxLeft = Math.max(padding, window.innerWidth - width - padding);
    const maxTop = Math.max(padding, window.innerHeight - height - padding);
    const left = Math.min(Math.max(x - width / 2, padding), maxLeft);
    const top = Math.min(Math.max(y - 36, padding), maxTop);
    setPosition((current) => current?.left === left && current.top === top ? current : { left, top });
  }, [x, y, mode, isAlreadyBlanked, isBold, onToggleBold, t]);

  const handleClickOutside = useCallback((e: MouseEvent) => {
    if (ref.current && !ref.current.contains(e.target as Node)) {
      onClose();
    }
  }, [onClose]);

  const handleKeyDown = useCallback((e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      onClose();
    }
  }, [onClose]);

  useEffect(() => {
    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [handleClickOutside, handleKeyDown]);

  return createPortal(
    <div
      ref={ref}
      className="fixed z-[9999] flex items-center rounded-full shadow-lg ring-1 ring-border/40 animate-in fade-in-0 zoom-in-95 duration-150 backdrop-blur-sm"
      style={{
        left: `${position?.left ?? -9999}px`,
        top: `${position?.top ?? -9999}px`,
        visibility: position ? 'visible' : 'hidden',
      }}
    >
      {isAlreadyBlanked ? (
        <NotionButton variant="ghost" size="sm" className="!px-3 !py-1.5 !h-auto !rounded-full bg-zinc-700/90 text-zinc-200 hover:bg-[var(--interactive-hover)] text-xs font-medium whitespace-nowrap" onClick={(e) => { e.stopPropagation(); onUnblank(); }}>
          <Eye size={12} />
          {t('recite.unblank')}
        </NotionButton>
      ) : (
        <NotionButton variant="ghost" size="sm" className="!px-3 !py-1.5 !h-auto !rounded-full bg-amber-500/90 text-white hover:bg-amber-500 text-xs font-medium whitespace-nowrap" onClick={(e) => { e.stopPropagation(); onBlank(); }}>
          <EyeSlash size={12} />
          {t('recite.blank')}
        </NotionButton>
      )}
    </div>,
    document.body,
  );
};
