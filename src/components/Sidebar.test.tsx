import { fireEvent, render, screen } from '@testing-library/react';
import { Sidebar } from './Sidebar';

function renderSidebar(onSelect = vi.fn()) {
  render(
    <Sidebar
      question="Belonging among first-generation students"
      current="overview"
      statuses={{ overview: 'done', protocol: 'done', extraction: 'running' }}
      verification="unverified"
      gatesPassed={2}
      onSelect={onSelect}
      onHome={vi.fn()}
      onSettings={vi.fn()}
    />,
  );
  return onSelect;
}

describe('Sidebar', () => {
  it('lists every stage with its status in text, not colour alone', () => {
    renderSidebar();
    expect(screen.getByRole('button', { name: 'Overview, done' })).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('button', { name: 'Extraction, in progress' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Report, not started' })).toBeInTheDocument();
    expect(screen.getAllByRole('listitem')).toHaveLength(9);
  });

  it('shows the verification badge', () => {
    renderSidebar();
    expect(screen.getByText('Unverified · 2 of 3 gates passed')).toBeInTheDocument();
  });

  it('selects a stage', () => {
    const onSelect = renderSidebar();
    fireEvent.click(screen.getByRole('button', { name: 'Search, not started' }));
    expect(onSelect).toHaveBeenCalledWith('search');
  });
});
