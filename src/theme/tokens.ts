// Design tokens from CLAUDE.md §10. global.css mirrors these as CSS custom
// properties; tokens.test.ts keeps the two in sync.
export const color = {
  ground: '#F6F4EE',
  sidebar: '#EFECE3',
  surface: '#FFFFFF',
  surfaceMuted: '#F4F2EC',
  line: '#DDD8CC',
  lineStrong: '#C8C2B4',
  ink: '#1C1B19',
  inkBody: '#2A2824',
  inkSecondary: '#3D3A34',
  inkMuted: '#5E5A52',
  accent: '#1F5F5B',
  accentDeep: '#14403D',
  accentSoft: '#E3EEEC',
  accentBorder: '#B9D3CF',
  exclude: '#8A4A12',
  warnText: '#6B4510',
  warnSoft: '#F7EBD5',
  warnBorder: '#C9A56A',
  highlight: '#FCE9C8',
  pending: '#BDB7A8',
} as const;

export const font = {
  display: "'Source Serif 4', Georgia, serif",
  ui: "'IBM Plex Sans', system-ui, sans-serif",
  mono: "'IBM Plex Mono', monospace",
} as const;

export const radius = { sm: 8, md: 10, lg: 12 } as const;

export type ColorToken = keyof typeof color;

/** camelCase token name to the CSS custom property that carries it. */
export function cssVar(name: ColorToken): string {
  return `--color-${name.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`;
}
