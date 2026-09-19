# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Builds the daisy-llm worker in release and stages it as a Tauri sidecar:
# crates/tauri-app/binaries/daisy-llm-<host triple>.exe.
$ErrorActionPreference = 'Stop'
$repo_root = Split-Path -Parent $PSScriptRoot
Set-Location $repo_root
$triple = (rustc -vV | Select-String '^host: (.+)$').Matches.Groups[1].Value
if ($LASTEXITCODE -ne 0) { throw "rustc -vV failed ($LASTEXITCODE)" }
if (-not $triple) { throw "rustc -vV printed no host triple" }
# llama.cpp's nested vulkan-shaders-gen build overflows MAX_PATH under the
# Visual Studio generator even inside a one-letter target dir (its MSBuild
# .tlog directory fails at 137 characters). Ninja writes no .tlog, so the
# worker builds with Ninja; ninja.exe comes from PATH or from the Visual
# Studio install's "C++ CMake tools for Windows" component.
# The generator belongs to this build alone: the app's own cmake crates read
# CMAKE_GENERATOR too, and cmake-rs hands MSVC a CMAKE_GENERATOR_INSTANCE that
# Ninja rejects. Both it and PATH go back the way they were.
$prev_generator = $env:CMAKE_GENERATOR
$prev_generator_set = Test-Path Env:CMAKE_GENERATOR
$prev_path = $env:PATH
try {
    if (-not $env:CMAKE_GENERATOR) { $env:CMAKE_GENERATOR = 'Ninja' }
    if ($env:CMAKE_GENERATOR -eq 'Ninja' -and -not (Get-Command ninja -ErrorAction SilentlyContinue)) {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        $ninja = $null
        if (Test-Path $vswhere) {
            $ninja = & $vswhere -latest -products * -find 'Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\ninja.exe' |
                     Select-Object -First 1
        }
        if (-not $ninja) { throw "ninja.exe not found on PATH or in a Visual Studio install (C++ CMake tools for Windows component)" }
        $env:PATH = "$(Split-Path -Parent $ninja);$env:PATH"
    }
    cargo build -p daisy-llm --release --locked
    if ($LASTEXITCODE -ne 0) { throw "daisy-llm build failed ($LASTEXITCODE)" }
} finally {
    if ($prev_generator_set) { $env:CMAKE_GENERATOR = $prev_generator }
    else { Remove-Item Env:CMAKE_GENERATOR -ErrorAction SilentlyContinue }
    $env:PATH = $prev_path
}
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
$out = Join-Path $repo_root 'crates\tauri-app\binaries'
New-Item -ItemType Directory -Force -Path $out | Out-Null
Copy-Item -Force (Join-Path $target 'release\daisy-llm.exe') (Join-Path $out "daisy-llm-$triple.exe")
Write-Host "staged $out\daisy-llm-$triple.exe"
