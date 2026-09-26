// A small set of line icons drawn inline, so nothing loads at runtime.
// Icons are decorative: every control that shows one also has a text label.

export type IconName =
  | 'settings'
  | 'home'
  | 'pause'
  | 'play'
  | 'skip'
  | 'undo'
  | 'check'
  | 'arrow'
  | 'alert'
  | 'shield'
  | 'panel'
  | 'close'
  | 'bolt';

const PATHS: Record<IconName, string> = {
  settings:
    'M12 15.2a3.2 3.2 0 1 0 0-6.4 3.2 3.2 0 0 0 0 6.4Z M19.4 13.5a7.6 7.6 0 0 0 0-3l2-1.5-2-3.5-2.4 1a7.4 7.4 0 0 0-2.6-1.5L14 2.5h-4l-.4 2.5A7.4 7.4 0 0 0 7 6.5l-2.4-1-2 3.5 2 1.5a7.6 7.6 0 0 0 0 3l-2 1.5 2 3.5 2.4-1a7.4 7.4 0 0 0 2.6 1.5l.4 2.5h4l.4-2.5a7.4 7.4 0 0 0 2.6-1.5l2.4 1 2-3.5-2-1.5Z',
  home: 'M3.5 10.5 12 3.5l8.5 7 M5.5 9v11h13V9',
  pause: 'M9 5.5v13 M15 5.5v13',
  play: 'M7.5 5v14l11-7-11-7Z',
  skip: 'M5.5 5.5v13l9-6.5-9-6.5Z M18.5 5.5v13',
  undo: 'M8.5 7.5 4.5 11.5l4 4 M4.5 11.5h10a5 5 0 0 1 0 10h-3',
  check: 'M5 12.5 10 17.5 19 7',
  arrow: 'M5 12h14 M13 6l6 6-6 6',
  alert: 'M12 4 2.8 19.5h18.4L12 4Z M12 10v4.5 M12 17.2v.3',
  shield: 'M12 3 4.5 6v5.5c0 4.6 3.1 8.1 7.5 9.5 4.4-1.4 7.5-4.9 7.5-9.5V6L12 3Z',
  panel: 'M4 5h16v14H4Z M15 5v14',
  close: 'M6 6l12 12 M18 6 6 18',
  bolt: 'M13 3 5 13.5h6L10 21l8-10.5h-6L13 3Z',
};

export function Icon({ name, size = 16, className }: { name: IconName; size?: number; className?: string | undefined }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}
