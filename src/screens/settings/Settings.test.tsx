import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { AppSettings, McpStatus, Preset, ProviderSummary, SourceStatus } from '../../lib/ipc';

const presets: Preset[] = [
  {
    id: 'anthropic', displayName: 'Anthropic', protocol: 'anthropic', baseUrl: 'https://api.anthropic.com',
    requiresKey: true, local: false, getKeyUrl: 'https://platform.claude.com/settings/keys',
    dataPolicyWarning: false, structuredOutput: true, maxTokensParam: 'max_tokens',
  },
  {
    id: 'ollama', displayName: 'Ollama (on this computer)', protocol: 'openai_compat', baseUrl: 'http://localhost:11434/v1',
    requiresKey: false, local: true, getKeyUrl: null, dataPolicyWarning: false, structuredOutput: true, maxTokensParam: 'max_tokens',
  },
  {
    id: 'custom', displayName: 'Custom (OpenAI-compatible)', protocol: 'openai_compat', baseUrl: 'https://',
    requiresKey: false, local: false, getKeyUrl: null, dataPolicyWarning: true, structuredOutput: true, maxTokensParam: 'max_tokens',
  },
];

const anthropic: ProviderSummary = {
  id: 'p1', preset: 'anthropic', displayName: 'Anthropic', protocol: 'anthropic', baseUrl: 'https://api.anthropic.com',
  hostLabel: 'api.anthropic.com', key: { present: true, hint: 'key ending 4Kf2' }, requiresKey: true, local: false,
  models: [{ id: 'claude-sonnet-5', displayName: 'Claude Sonnet 5', structuredOutput: true }],
  dataPolicyWarning: false, concurrency: 4, status: 'connected', statusMessage: 'Connected.', urlWarning: null,
  getKeyUrl: 'https://platform.claude.com/settings/keys',
};
const ollama: ProviderSummary = {
  ...anthropic, id: 'p2', preset: 'ollama', displayName: 'Ollama (on this computer)', protocol: 'openai_compat',
  baseUrl: 'http://localhost:11434/v1', hostLabel: 'localhost:11434/v1', key: { present: false, hint: null },
  requiresKey: false, local: true, models: [{ id: 'llama3.2:latest', displayName: null, structuredOutput: null }],
  status: 'not_running', statusMessage: 'Not running. Start the local server, then test again.',
};

const settings: AppSettings = {
  strictMode: true, warnDataPolicy: true, unpaywallEmail: null, libraryProxyPrefix: null, spendingCapUsd: 50,
  layaPython: null,
  institutionalAccess: false,
  bypassByDefault: false, authorName: null, authorAffiliation: null, fundingStatement: null, competingInterests: null, mcpEnabled: false, mcpPort: 31480,
};

const sources: SourceStatus[] = [
  { id: 'eric', name: 'ERIC', auth: 'none', note: 'No key needed', getKeyUrl: null, docsUrl: 'https://eric.ed.gov/?api',
    key: { present: false, hint: null }, instToken: null, ready: true },
  { id: 'openalex', name: 'OpenAlex', auth: 'key', note: 'Free key', getKeyUrl: 'https://openalex.org/settings/api',
    docsUrl: 'https://help.openalex.org/api/authentication/', key: { present: false, hint: null }, instToken: null, ready: false },
];

const ipc = vi.hoisted(() => ({
  listPresets: vi.fn(),
  listProviders: vi.fn(),
  checkBaseUrl: vi.fn(),
  fetchModels: vi.fn(),
  testProvider: vi.fn(),
  saveProvider: vi.fn(),
  deleteProvider: vi.fn(),
  listTaskAssignments: vi.fn(),
  setTaskAssignment: vi.fn(),
  listPrices: vi.fn(),
  setPrice: vi.fn(),
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
  listSources: vi.fn(),
  setSourceKey: vi.fn(),
  deleteSourceKey: vi.fn(),
  testSource: vi.fn(),
  openExternal: vi.fn(),
  reviewSpend: vi.fn(),
  layaStatus: vi.fn(),
  layaTest: vi.fn(),
  mcpStatus: vi.fn(),
  regenerateMcpToken: vi.fn(),
}));

vi.mock('../../lib/ipc', async (orig) => ({ ...(await orig<typeof import('../../lib/ipc')>()), ipc }));

import { Settings } from './Settings';

function renderWithClient(ui: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

const mcp: McpStatus = {
  enabled: true, running: true, port: 31480, url: 'http://127.0.0.1:31480/mcp',
  bridgeUrl: 'http://localhost:31480/bridge/v1', token: 'sift_secrettoken00000000000000000000000000000000000000000000000abcd',
  stdioCommand: '/Applications/Sift.app/Contents/MacOS/sift --mcp-stdio', error: null,
  bridge: { waiting: 1, claimed: 0, lastPollSecs: 4 },
};

const assignments = [
  { task: 'screener_a', label: 'Screener A', providerId: 'p1', modelId: 'claude-sonnet-5' },
  { task: 'screener_b', label: 'Screener B', providerId: null, modelId: null },
];

beforeEach(() => {
  vi.clearAllMocks();
  ipc.listPresets.mockResolvedValue(presets);
  ipc.listProviders.mockResolvedValue([anthropic, ollama]);
  ipc.checkBaseUrl.mockResolvedValue(null);
  ipc.listTaskAssignments.mockResolvedValue(assignments);
  ipc.setTaskAssignment.mockResolvedValue(assignments);
  ipc.listPrices.mockResolvedValue([]);
  ipc.getSettings.mockResolvedValue(settings);
  ipc.updateSettings.mockImplementation(async (s: AppSettings) => s);
  ipc.listSources.mockResolvedValue(sources);
  ipc.reviewSpend.mockResolvedValue(1.234);
  ipc.layaStatus.mockResolvedValue({
    available: true, python: '/opt/homebrew/bin/python3', version: '0.2.0', model: 'aac6fef/laya-mlx',
    modelDownloaded: true, message: 'Ready. Runs offline on this Mac.',
  });
  ipc.mcpStatus.mockResolvedValue(mcp);
  ipc.regenerateMcpToken.mockResolvedValue({ ...mcp, token: 'sift_newtoken0000000000000000000000000000000000000000000000009999' });
});

describe('Settings', () => {
  it('shows the local pre-screen status and tests it', async () => {
    ipc.layaTest.mockResolvedValue({ ok: true, message: 'Connected: laya-mlx 0.2.0 answered P(true) = 0.91.', loadMs: 700, answerMs: 140 });
    renderWithClient(<Settings from={null} />);
    expect(await screen.findByText(/Ready. Runs offline on this Mac. laya-mlx 0.2.0/)).toBeInTheDocument();
    fireEvent.click(await screen.findByRole('button', { name: 'Test Laya' }));
    expect(await screen.findByText(/Loaded in 700 ms, answered in 140 ms/)).toBeInTheDocument();
  });

  it('lists providers with key hints and status, never a key', async () => {
    renderWithClient(<Settings from={null} />);
    const list = await screen.findByRole('list', { name: 'Saved providers' });
    expect(within(list).getByText('api.anthropic.com · key ending 4Kf2')).toBeInTheDocument();
    expect(within(list).getByText('localhost:11434/v1 · no key needed')).toBeInTheDocument();
    expect(within(list).getByText('Connected')).toBeInTheDocument();
    expect(within(list).getByText('Not running')).toBeInTheDocument();
  });

  it('fills the form from a preset and saves a draft with the typed key', async () => {
    ipc.saveProvider.mockResolvedValue(anthropic);
    renderWithClient(<Settings from={null} />);
    const preset = await screen.findByLabelText('Preset');
    fireEvent.change(preset, { target: { value: 'anthropic' } });
    expect(screen.getByLabelText('Base URL')).toHaveValue('https://api.anthropic.com');
    expect(screen.getByLabelText('Display name')).toHaveValue('Anthropic');

    const key = screen.getByLabelText('API key');
    expect(key).toHaveAttribute('type', 'password');
    fireEvent.change(key, { target: { value: 'sk-ant-api03-typed' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));

    await waitFor(() => expect(ipc.saveProvider).toHaveBeenCalledTimes(1));
    expect(ipc.saveProvider.mock.calls[0]?.[0]).toMatchObject({
      id: null,
      preset: 'anthropic',
      baseUrl: 'https://api.anthropic.com',
      apiKey: 'sk-ant-api03-typed',
    });
  });

  it('shows the test result from Rust', async () => {
    ipc.testProvider.mockResolvedValue({
      ok: true, status: 'connected', message: 'Connected. claude-sonnet-5 replied.', result: null,
    });
    renderWithClient(<Settings from={null} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Test connection' }));
    expect(await screen.findByText('Connected. claude-sonnet-5 replied.')).toBeInTheDocument();
  });

  it('warns about plain-http remote base URLs', async () => {
    ipc.checkBaseUrl.mockResolvedValue('gateway.example.edu is reached over plain http.');
    renderWithClient(<Settings from={null} />);
    expect(await screen.findByText('gateway.example.edu is reached over plain http.')).toBeInTheDocument();
  });

  it('offers provider models per task and saves the choice', async () => {
    renderWithClient(<Settings from={null} />);
    const select = await screen.findByLabelText('Screener B');
    await within(select).findByRole('option', { name: 'Ollama (on this computer) · llama3.2:latest' });
    fireEvent.change(select, { target: { value: 'p2\u0000llama3.2:latest' } });
    await waitFor(() => expect(ipc.setTaskAssignment).toHaveBeenCalledWith('screener_b', 'p2', 'llama3.2:latest'));
    expect(screen.getByLabelText('Screener A')).toHaveValue('p1\u0000claude-sonnet-5');
  });

  it('toggles strict mode through settings', async () => {
    renderWithClient(<Settings from={null} />);
    const strict = await screen.findByRole('checkbox', { name: /Strict mode/ });
    expect(strict).toBeChecked();
    fireEvent.click(strict);
    await waitFor(() => expect(ipc.updateSettings).toHaveBeenCalledWith({ ...settings, strictMode: false }));
  });

  it('adds a database key and opens Get key pages through Rust', async () => {
    ipc.setSourceKey.mockResolvedValue(sources);
    renderWithClient(<Settings from={null} />);
    await screen.findByText('Ready');
    const dbs = screen.getByRole('list', { name: 'Databases' });

    fireEvent.click(within(dbs).getByRole('button', { name: 'Get key for OpenAlex' }));
    expect(ipc.openExternal).toHaveBeenCalledWith('https://openalex.org/settings/api');

    fireEvent.click(within(dbs).getByRole('button', { name: 'Add OpenAlex' }));
    fireEvent.change(screen.getByLabelText('OpenAlex API key'), { target: { value: 'oa-key' } });
    fireEvent.click(within(dbs).getByRole('button', { name: 'Save OpenAlex key' }));
    await waitFor(() => expect(ipc.setSourceKey).toHaveBeenCalledWith('openalex', 'oa-key', null));
  });

  it('shows the current review spend when opened from a review', async () => {
    renderWithClient(<Settings from="review-1" />);
    expect(await screen.findByText('$1.23')).toBeInTheDocument();
    expect(ipc.reviewSpend).toHaveBeenCalledWith('review-1');
  });

  it('shows the MCP server status with a masked token and confirms a new token inline', async () => {
    renderWithClient(<Settings from={null} />);
    expect(await screen.findByText(/Running at http:\/\/127.0.0.1:31480\/mcp. An assistant asked for work 4 s ago. 1 model call waiting/)).toBeInTheDocument();
    expect(screen.queryByText(/secrettoken/)).not.toBeInTheDocument();
    expect(screen.getByText(/abcd$/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'New token' }));
    expect(ipc.regenerateMcpToken).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Make new token' }));
    expect(await screen.findByText(/New token made/)).toBeInTheDocument();
    expect(ipc.regenerateMcpToken).toHaveBeenCalledTimes(1);
  });

  it('rejects an MCP port outside 1024 to 65535', async () => {
    renderWithClient(<Settings from={null} />);
    const port = await screen.findByRole('textbox', { name: 'Port' });
    fireEvent.change(port, { target: { value: '80' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save MCP server' }));
    expect(await screen.findByText('Port must be a whole number from 1024 to 65535.')).toBeInTheDocument();
    expect(ipc.updateSettings).not.toHaveBeenCalled();
  });
});
