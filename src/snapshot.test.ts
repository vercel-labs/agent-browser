import { describe, expect, it, vi } from 'vitest';

import { getEnhancedSnapshot } from './snapshot.js';

describe('getEnhancedSnapshot', () => {
  const ariaTree = `
- document:
  - dialog "Welcome to Børsen Consent Management":
    - document:
      - paragraph:
        - button "our 89 partners"
      - 'button "Agree and close: Agree to our data processing and close"': Agree and close
      - 'button "Disagree and close: Disagree to our data processing and close"': Disagree and close
      - 'button "Learn More: Configure your consents"': Learn More →
`.trim();

  function createPage() {
    const ariaSnapshot = vi.fn().mockResolvedValue(ariaTree);
    const locator = vi.fn().mockReturnValue({ ariaSnapshot });
    return { locator } as any;
  }

  it('keeps quoted ARIA buttons in interactive snapshots', async () => {
    const page = createPage();

    const { tree, refs } = await getEnhancedSnapshot(page, { interactive: true });

    expect(tree).toContain(
      '- button "Agree and close: Agree to our data processing and close" [ref=e2]: Agree and close'
    );
    expect(tree).toContain(
      '- button "Disagree and close: Disagree to our data processing and close" [ref=e3]: Disagree and close'
    );
    expect(tree).toContain('- button "Learn More: Configure your consents" [ref=e4]: Learn More →');
    expect(refs.e2).toMatchObject({
      role: 'button',
      name: 'Agree and close: Agree to our data processing and close',
    });
    expect(refs.e4).toMatchObject({
      role: 'button',
      name: 'Learn More: Configure your consents',
    });
  });

  it('keeps quoted ARIA buttons in full snapshots', async () => {
    const page = createPage();

    const { tree, refs } = await getEnhancedSnapshot(page);

    expect(tree).toContain(
      '- button "Agree and close: Agree to our data processing and close" [ref=e2]: Agree and close'
    );
    expect(tree).toContain(
      '- button "Disagree and close: Disagree to our data processing and close" [ref=e3]: Disagree and close'
    );
    expect(tree).toContain('- button "Learn More: Configure your consents" [ref=e4]: Learn More →');
    expect(refs.e2).toMatchObject({
      role: 'button',
      name: 'Agree and close: Agree to our data processing and close',
    });
    expect(refs.e4).toMatchObject({
      role: 'button',
      name: 'Learn More: Configure your consents',
    });
  });
});
