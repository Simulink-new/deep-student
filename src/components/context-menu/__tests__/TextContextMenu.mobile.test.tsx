import React from 'react';
import { render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { TextContextMenuProvider } from '../TextContextMenu';

const isMobilePlatformMock = vi.hoisted(() => vi.fn(() => false));

vi.mock('@/utils/platform', () => ({
  isMobilePlatform: isMobilePlatformMock,
}));

vi.mock('@/utils/clipboardUtils', () => ({
  copyTextToClipboard: vi.fn(),
  readTextFromClipboard: vi.fn(async () => ''),
}));

vi.mock('@/components/shared/OverlayCoordinator', () => ({
  useOverlayCoordinator: () => ({
    dismissTooltips: vi.fn(),
    registerInteractiveOverlay: vi.fn(() => vi.fn()),
  }),
}));

describe('TextContextMenuProvider mobile', () => {
  beforeEach(() => {
    isMobilePlatformMock.mockReturnValue(true);
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  test('does not intercept contextmenu on mobile so the system paste menu can appear', () => {
    render(
      <TextContextMenuProvider>
        <input aria-label="password" />
      </TextContextMenuProvider>
    );

    const input = document.querySelector('input') as HTMLInputElement;
    const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
    input.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(false);
    expect(document.querySelector('[data-context-menu-handled="true"]')).toBeNull();
  });
});
