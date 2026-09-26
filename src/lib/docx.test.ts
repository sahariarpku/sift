import { describe, expect, it } from 'vitest';
import { location, runningHead, titleCase } from './docx';

describe('APA title case', () => {
  it('capitalises major words and keeps minor words lower case', () => {
    expect(titleCase('characteristics of included studies')).toBe('Characteristics of Included Studies');
  });
  it('capitalises the first word after a colon and keeps acronyms', () => {
    expect(titleCase('search strategies (PRISMA-S): an overview of the ERIC string')).toBe('Search Strategies (PRISMA-S): An Overview of the ERIC String');
  });
  it('capitalises both parts of a hyphenated major word', () => {
    expect(titleCase('self-report measures')).toBe('Self-Report Measures');
  });
});

describe('running head', () => {
  it('uses the short title in capitals', () => {
    expect(runningHead('Peer Tutoring in Schools: A Systematic Review')).toBe('PEER TUTORING IN SCHOOLS');
  });
  it('stays within 50 characters at a word boundary', () => {
    const h = runningHead('How Do First-Generation University Students Experience Academic Advising in Their First Year');
    expect(h.length).toBeLessThanOrEqual(50);
    expect(h.endsWith(' ')).toBe(false);
  });
});

describe('checklist locations', () => {
  it('puts items 24 to 27 in the author note', () => {
    expect(location('3')).toBe('Introduction');
    expect(location('10')).toBe('Method');
    expect(location('25')).toBe('Author note');
  });
});
