import { describe, expect, it, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import { ResizableSplit } from './ResizableSplit';

describe('<ResizableSplit />', () => {
  beforeEach(() => localStorage.clear());

  it('defaults to a 50/50 split', () => {
    const { container } = render(
      <ResizableSplit storageKey="t.split" top={<div>top</div>} bottom={<div>bottom</div>} />,
    );
    const pane = container.querySelector('.rsplit__pane') as HTMLElement;
    expect(pane.style.flex).toBe('0 0 50%');
  });

  it('a stored fraction overrides the default', () => {
    localStorage.setItem('t.split', '0.3');
    const { container } = render(
      <ResizableSplit storageKey="t.split" top={<div>top</div>} bottom={<div>bottom</div>} />,
    );
    const pane = container.querySelector('.rsplit__pane') as HTMLElement;
    expect(pane.style.flex).toBe('0 0 30%');
  });

  it('garbage in storage falls back to the default', () => {
    localStorage.setItem('t.split', 'NaN-nonsense');
    const { container } = render(
      <ResizableSplit storageKey="t.split" top={<div>top</div>} bottom={<div>bottom</div>} />,
    );
    const pane = container.querySelector('.rsplit__pane') as HTMLElement;
    expect(pane.style.flex).toBe('0 0 50%');
  });
});
