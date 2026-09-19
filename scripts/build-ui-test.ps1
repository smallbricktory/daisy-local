# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Builds the e2e binary on Windows. Output:
# $env:CARGO_TARGET_DIR\release\daisy-app.exe, target\release when the var is
# unset. The frontend goes to apps\frontend\dist-ui-test; the shipping
# apps\frontend\dist is not written. VITE_UI_TEST is cleared on exit so a
# later shipping build in the same session does not pick it up.
$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..')
$env:VITE_UI_TEST = '1'
try {
    # vite writes its chunk-size warning to stderr; under Stop, Windows
    # PowerShell 5.1 turns that into a terminating NativeCommandError.
    # $LASTEXITCODE gates each native step instead.
    $ErrorActionPreference = 'Continue'
    npx pnpm@10 --dir apps/frontend run build
    if ($LASTEXITCODE -ne 0) { throw "frontend: pnpm build failed ($LASTEXITCODE)" }
    Push-Location crates/tauri-app
    try {
        cargo tauri build --no-bundle --features ui-test --config tauri.ui-test.conf.json
        if ($LASTEXITCODE -ne 0) { throw "cargo tauri build failed ($LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
    $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
    Get-Item (Join-Path $targetDir 'release/daisy-app.exe')
} finally {
    $env:VITE_UI_TEST = $null
    $ErrorActionPreference = 'Stop'
    Pop-Location
}
