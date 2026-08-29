import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { ApiKeyField } from '../ApiKeyField';

const isMobilePlatformMock = vi.hoisted(() => vi.fn(() => false));
const readTextFromClipboardMock = vi.hoisted(() => vi.fn(async () => 'pasted-from-button'));

vi.mock('@/utils/platform', () => ({
  isMobilePlatform: isMobilePlatformMock,
}));

vi.mock('@/utils/clipboardUtils', () => ({
  readTextFromClipboard: readTextFromClipboardMock,
}));

describe('ApiKeyField paste', () => {
  beforeEach(() => {
    isMobilePlatformMock.mockReturnValue(false);
    readTextFromClipboardMock.mockResolvedValue('pasted-from-button');
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  test('mobile paste does not preventDefault and shows a paste button', async () => {
    isMobilePlatformMock.mockReturnValue(true);
    const onChange = vi.fn();
    render(
      <ApiKeyField
        value=""
        onChange={onChange}
        revealed={false}
        canReveal={false}
        showLabel="show"
        hideLabel="hide"
        onToggle={() => {}}
      />
    );

    const pasteButton = screen.getByRole('button', { name: '粘贴' });
    expect(pasteButton).toBeInTheDocument();

    const input = document.querySelector('input') as HTMLInputElement;
    const preventDefault = vi.fn();
    fireEvent.paste(input, {
      preventDefault,
      clipboardData: { getData: () => '' },
    } as unknown as ClipboardEvent);

    expect(preventDefault).not.toHaveBeenCalled();

    fireEvent.click(pasteButton);
    await vi.waitFor(() => {
      expect(onChange).toHaveBeenCalled();
    });
  });
});
