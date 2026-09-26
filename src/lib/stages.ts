// Review screens shown in the sidebar, in pipeline order (CLAUDE.md §8).
export type StageId =
  | 'overview'
  | 'protocol'
  | 'search'
  | 'screening'
  | 'fulltext'
  | 'extraction'
  | 'appraisal'
  | 'synthesis'
  | 'report';

export type StageStatus = 'done' | 'running' | 'waiting' | 'pending' | 'skipped';

export const STAGES: ReadonlyArray<{ id: StageId; label: string }> = [
  { id: 'overview', label: 'Overview' },
  { id: 'protocol', label: 'Protocol' },
  { id: 'search', label: 'Search' },
  { id: 'screening', label: 'Screening' },
  { id: 'fulltext', label: 'Full text' },
  { id: 'extraction', label: 'Extraction' },
  { id: 'appraisal', label: 'Appraisal' },
  { id: 'synthesis', label: 'Synthesis' },
  { id: 'report', label: 'Report' },
];
