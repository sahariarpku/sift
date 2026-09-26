import { ReviewShell } from '../screens/review/ReviewShell';
import { Settings } from '../screens/settings/Settings';
import { Start } from '../screens/Start';
import { useUi } from './store';

export function App() {
  const screen = useUi((s) => s.screen);
  switch (screen.name) {
    case 'start':
      return <Start />;
    case 'settings':
      return <Settings from={screen.from} />;
    case 'review':
      return <ReviewShell reviewId={screen.reviewId} stage={screen.stage} />;
  }
}
