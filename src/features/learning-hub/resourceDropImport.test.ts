import { describe, expect, it } from 'vitest';

import {
  classifyDroppedName,
  getFileExtension,
  isSupportedResourceDropExtension,
} from './resourceDropImport';

describe('resourceDropImport classification', () => {
  it('classifies documents, images, media and unsupported names', () => {
    expect(classifyDroppedName('paper.pdf')).toBe('document');
    expect(classifyDroppedName('notes.markdown')).toBe('document');
    expect(classifyDroppedName('sheet.xlsb')).toBe('document');
    expect(classifyDroppedName('photo.HEIC')).toBe('image');
    expect(classifyDroppedName('clip.mp4')).toBe('media');
    expect(classifyDroppedName('song.mp3')).toBe('media');
    expect(classifyDroppedName('archive.zip')).toBe('unsupported');
    expect(classifyDroppedName('README')).toBe('unsupported');
  });

  it('exposes extension helpers used by the drop overlay', () => {
    expect(getFileExtension('C:\\\\Users\\\\a\\\\b.PNG')).toBe('png');
    expect(isSupportedResourceDropExtension('pdf')).toBe(true);
    expect(isSupportedResourceDropExtension('zip')).toBe(false);
  });
});
