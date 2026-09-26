import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import './theme/fonts';
import './theme/global.css';
import { App } from './app/App';
import { PdfWorker } from './app/PdfWorker';

const root = document.getElementById('root');
if (!root) throw new Error('missing #root');

// Keep polling when the window is in the background (the user is often in their browser).
const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, refetchIntervalInBackground: true } } });

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
      <PdfWorker />
    </QueryClientProvider>
  </StrictMode>,
);
