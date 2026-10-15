# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Builds the e2e binary on Windows. Output:
# $env:CARGO_TARGET_DIR\release\daisy-app.exe, target\release when the var is
# unset. The frontend goes to apps\frontend\dist-ui-test; the shipping
# apps\frontend\dist is not written. The release profile runs with LTO off and
# 16 codegen units; opt-level stays 3. VITE_UI_TEST and the profile overrides
# are cleared on exit so a later shipping build in the same session does not
# pick them up.
$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..')
$env:VITE_UI_TEST = '1'
try {
    # $LASTEXITCODE gates each native step.
    $ErrorActionPreference = 'Continue'
    # Stages the bundle resources; the download scripts keep files whose sha256 already matches.
    foreach ($m in 'voiceprints', 'nemotron', 'nemotron3-diar', 'embeddings') {
        bash "scripts/download-$m.sh"
        if ($LASTEXITCODE -ne 0) { throw "download-$m failed ($LASTEXITCODE)" }
    }
    npx pnpm@10 --dir apps/frontend run build
    if ($LASTEXITCODE -ne 0) { throw "frontend: pnpm build failed ($LASTEXITCODE)" }
    Push-Location crates/tauri-app
    try {
        $env:CARGO_PROFILE_RELEASE_LTO = 'false'
        $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = '16'
        cargo tauri build --no-bundle --features ui-test --config tauri.ui-test.conf.json
        if ($LASTEXITCODE -ne 0) { throw "cargo tauri build failed ($LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
    $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
    Get-Item (Join-Path $targetDir 'release/daisy-app.exe')
} finally {
    $env:VITE_UI_TEST = $null
    $env:CARGO_PROFILE_RELEASE_LTO = $null
    $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = $null
    $ErrorActionPreference = 'Stop'
    Pop-Location
}
