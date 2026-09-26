import { create } from 'zustand';
import type { StageId } from '../lib/stages';

export type Screen =
  | { name: 'start' }
  /** `from` is the review the user came from, so Settings can show its spend and return to it. */
  | { name: 'settings'; from: string | null }
  | { name: 'review'; reviewId: string; stage: StageId };

interface UiState {
  screen: Screen;
  go: (screen: Screen) => void;
  /** The activity side panel on review screens. */
  activityOpen: boolean;
  toggleActivity: () => void;
}

const ACTIVITY_KEY = 'sift.activityOpen';

// Storage can be unavailable; the panel then just starts closed.
function readActivityOpen(): boolean {
  try {
    return window.localStorage.getItem(ACTIVITY_KEY) === 'true';
  } catch {
    return false;
  }
}

function writeActivityOpen(open: boolean) {
  try {
    window.localStorage.setItem(ACTIVITY_KEY, String(open));
  } catch {
    // Remembering the panel is a convenience only.
  }
}

export const useUi = create<UiState>((set, get) => ({
  screen: { name: 'start' },
  go: (screen) => set({ screen }),
  activityOpen: readActivityOpen(),
  toggleActivity: () => {
    const open = !get().activityOpen;
    writeActivityOpen(open);
    set({ activityOpen: open });
  },
}));
