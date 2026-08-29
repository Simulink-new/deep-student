import { useState, useEffect } from 'react';

/**
 * 响应式媒体查询Hook
 * @param query - CSS媒体查询字符串，如 '(max-width: 768px)'
 * @returns 是否匹配该媒体查询
 * 
 * @example
 * const isMobile = useMediaQuery('(max-width: 768px)');
 * const isDarkMode = useMediaQuery('(prefers-color-scheme: dark)');
 */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => {
    if (typeof window === 'undefined') return false;
    return window.matchMedia(query).matches;
  });

  useEffect(() => {
    if (typeof window === 'undefined') return;

    const mediaQuery = window.matchMedia(query);
    let lastMatches = mediaQuery.matches;

    // 初始化时立即更新状态
    setMatches(lastMatches);

    const applyIfChanged = (nextMatches: boolean) => {
      if (nextMatches === lastMatches) return;
      lastMatches = nextMatches;
      setMatches(nextMatches);
    };

    // 监听变化
    const handleChange = (event: MediaQueryListEvent) => {
      applyIfChanged(event.matches);
    };

    // 安卓键盘/粘贴面板弹出时，部分 WebView 会短暂改写 layout viewport，
    // 导致 matchMedia('(min-width: 768px)') 误报为桌面。用 visualViewport 宽度
    // 再校验一次：真正的横屏/桌面切换才会跨过断点。
    const handleViewportChange = () => {
      const viewportWidth = window.visualViewport?.width ?? window.innerWidth;
      const minWidthMatch = query.match(/\(min-width:\s*(\d+(?:\.\d+)?)px\)/i);
      const maxWidthMatch = query.match(/\(max-width:\s*(\d+(?:\.\d+)?)px\)/i);
      if (!minWidthMatch && !maxWidthMatch) {
        applyIfChanged(window.matchMedia(query).matches);
        return;
      }
      let next = true;
      if (minWidthMatch) next = next && viewportWidth >= Number(minWidthMatch[1]);
      if (maxWidthMatch) next = next && viewportWidth <= Number(maxWidthMatch[1]);
      applyIfChanged(next);
    };

    if (mediaQuery.addEventListener) {
      mediaQuery.addEventListener('change', handleChange);
    } else {
      mediaQuery.addListener(handleChange);
    }
    window.visualViewport?.addEventListener('resize', handleViewportChange);

    return () => {
      if (mediaQuery.removeEventListener) {
        mediaQuery.removeEventListener('change', handleChange);
      } else {
        mediaQuery.removeListener(handleChange);
      }
      window.visualViewport?.removeEventListener('resize', handleViewportChange);
    };
  }, [query]);

  return matches;
}

export default useMediaQuery;

