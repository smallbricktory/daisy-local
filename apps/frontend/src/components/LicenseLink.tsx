// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { tauri } from '../tauri';

export function LicenseLink({ url, license, testId }: {
  url: string | null | undefined;
  license: string;
  testId: string;
}) {
  if (!url) return <>{license}</>;
  return (
    <a href="#" data-testid={testId} onClick={(e) => { e.preventDefault(); void tauri.openExternal(url); }}>
      {license}
    </a>
  );
}
