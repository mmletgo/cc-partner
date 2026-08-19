// @vitest-environment jsdom
/**
 * AgentHubEntry 把旧项目查看上下文转到 Workbench 项目 Agent。
 *
 * Business Logic: `/agent-hub?scope=project` 不得再渲染 Hub 项目切换器。
 * Code Logic: MemoryRouter 断言 replace-navigate 到 view=projectAgent。
 */

import { afterEach, describe, expect, test } from 'vitest';
import type { ReactElement } from 'react';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router-dom';
import { cleanup, render, screen } from '@testing-library/react';
import { AgentHubEntry } from './AgentHubEntry';

function LocationProbe(): ReactElement {
  const location = useLocation();
  return <div data-testid="location">{`${location.pathname}${location.search}`}</div>;
}

afterEach(() => {
  cleanup();
});

describe('AgentHubEntry', () => {
  test('redirects project scope to Workbench Project Agent', () => {
    render(
      <MemoryRouter
        initialEntries={['/agent-hub?scope=project&project=local-1&tab=skill&agent=codex']}
      >
        <Routes>
          <Route path="/agent-hub" element={<AgentHubEntry />} />
          <Route path="/workbench" element={<LocationProbe />} />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByTestId('location').textContent).toContain('/workbench?');
    expect(screen.getByTestId('location').textContent).toContain('projectId=local-1');
    expect(screen.getByTestId('location').textContent).toContain('view=projectAgent');
    expect(screen.getByTestId('location').textContent).toContain('tab=skill');
    expect(screen.getByTestId('location').textContent).toContain('agent=codex');
  });

  test('project scope without project key goes to Workbench without opening the console', () => {
    render(
      <MemoryRouter initialEntries={['/agent-hub?scope=project']}>
        <Routes>
          <Route path="/agent-hub" element={<AgentHubEntry />} />
          <Route path="/workbench" element={<LocationProbe />} />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByTestId('location').textContent).toBe('/workbench');
  });

  test('legacy section=projectInstructions with a project key opens Workbench Project Agent', () => {
    render(
      <MemoryRouter initialEntries={['/agent-hub?section=projectInstructions&project=local-2']}>
        <Routes>
          <Route path="/agent-hub" element={<AgentHubEntry />} />
          <Route path="/workbench" element={<LocationProbe />} />
        </Routes>
      </MemoryRouter>,
    );
    expect(screen.getByTestId('location').textContent).toContain('projectId=local-2');
    expect(screen.getByTestId('location').textContent).toContain('view=projectAgent');
  });
});
