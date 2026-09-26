import css from './global.css?raw';
import fonts from './fonts.ts?raw';
import { color, cssVar, font, radius } from './tokens';

describe('design tokens', () => {
  it('match CLAUDE.md §10', () => {
    expect(color.accent).toBe('#1F5F5B');
    expect(color.ground).toBe('#F6F4EE');
    expect(Object.keys(color)).toHaveLength(20);
    expect(radius).toEqual({ sm: 8, md: 10, lg: 12 });
    expect(font.display.startsWith("'Source Serif 4'")).toBe(true);
  });

  it('are mirrored exactly in global.css', () => {
    for (const [name, hex] of Object.entries(color)) {
      expect(css).toContain(`${cssVar(name as keyof typeof color)}: ${hex};`);
    }
    expect(css).toContain(`--font-display: ${font.display};`);
    expect(css).toContain(`--font-ui: ${font.ui};`);
    expect(css).toContain(`--font-mono: ${font.mono};`);
  });
});

describe('fonts', () => {
  it('are bundled, never loaded from Google at runtime', () => {
    expect(fonts).not.toMatch(/googleapis|gstatic/);
    expect(css).not.toMatch(/googleapis|gstatic/);
  });
});
