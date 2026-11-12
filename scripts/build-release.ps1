# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Windows release build for Daisy. Runs on the Windows self-hosted runner
# (or any Windows box with Rust + Node + pnpm + WebView2 SDK installed).
#
# Produces two SEPARATE dist artifacts:
#   * Daisy_<version>_x64-setup.exe   (NSIS installer; path selection + upgrade)
#   * daisy-<version>.zip             (portable/ — drag-drop runnable bundle)
# The installer is not packed inside the zip.
#
# Code signing: when .creds-artifact-signing (gitignored) is present with valid
# Azure Trusted/Artifact Signing creds, the app exe + NSIS installer are signed
# via Microsoft's Sign CLI (`sign code trusted-signing`), authenticating from the
# AZURE_* env vars (service principal; no Azure CLI, no interactive login).
# Absent → unsigned build, or a thrown error under -RequireSigned (local
# tester builds). See the signing block below.
# Build-box prereqs: .NET SDK + `dotnet tool install --global sign --prerelease`.

[CmdletBinding()]
param(
    [string]$Version,
    # Refuses to finish unless every shipped exe carries a valid Authenticode signature.
    [switch]$RequireSigned
)

$ErrorActionPreference = 'Stop'

# Sets CI so pnpm runs non-interactively.
if (-not $env:CI) { $env:CI = 'true' }

# Sets the CMake policy floor unless already provided.
if (-not $env:CMAKE_POLICY_VERSION_MINIMUM) { $env:CMAKE_POLICY_VERSION_MINIMUM = '3.5' }

$repo_root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $repo_root

# -RequireSigned fails before the long build when signing is impossible.
if ($RequireSigned -and -not (Test-Path (Join-Path $repo_root '.creds-artifact-signing'))) {
    throw ("signing: .creds-artifact-signing is missing from $repo_root, so this build would be UNSIGNED and blocked on tester machines. " +
           "Next: place the gitignored .creds-artifact-signing (TENANT_ID, CLIENT_ID, SECRET_VALUE, ACCOUNT_URI, ACCOUNT_NAME, CERT_PROFILE_NAME) " +
           "in the repo root of the source checkout on the build host (scripts/lib/stage-windows.sh rsyncs it to C:\tmp\daisy-build), then rerun the build.")
}

# Vulkan GPU whisper backend (NVIDIA/AMD/Intel; CPU fallback when no device).
# The Vulkan SDK (glslc) is resolved explicitly from the machine VULKAN_SDK env.
if (-not $env:VULKAN_SDK) {
    $vk = Get-ChildItem 'C:\VulkanSDK' -Directory -ErrorAction SilentlyContinue |
          Sort-Object Name -Descending | Select-Object -First 1
    if ($vk) { $env:VULKAN_SDK = $vk.FullName }
}
if ($env:VULKAN_SDK) { $env:PATH = "$($env:VULKAN_SDK)\Bin;$($env:PATH)" }
# The build goes into a short target dir. All target references below honor $target_root.
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'G:\b' }
New-Item -ItemType Directory -Force $env:CARGO_TARGET_DIR | Out-Null
$target_root = $env:CARGO_TARGET_DIR

if (-not $Version) {
    # The version comes from crates/tauri-app/Cargo.toml.
    $cargoToml = Get-Content "$repo_root/crates/tauri-app/Cargo.toml" -Raw
    if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
        $Version = $matches[1]
    } else {
        throw "Couldn't read tauri-app version from Cargo.toml"
    }
}

Write-Host "==> Building Daisy $Version (Windows)" -ForegroundColor Cyan

# Frontend ------------------------------------------------------------------
# The native frontend commands run under Continue and $LASTEXITCODE gates each step.
$ErrorActionPreference = 'Continue'
Write-Host "--- frontend deps ---"
pnpm --dir apps/frontend install --frozen-lockfile
if ($LASTEXITCODE -ne 0) { throw "frontend: pnpm install failed ($LASTEXITCODE)" }
Write-Host "--- frontend build ---"
# Builds the shipping dist.
Remove-Item Env:VITE_UI_TEST -ErrorAction SilentlyContinue
pnpm --dir apps/frontend exec tsc -b
if ($LASTEXITCODE -ne 0) { throw "frontend: tsc -b failed ($LASTEXITCODE)" }
pnpm --dir apps/frontend exec vite build
if ($LASTEXITCODE -ne 0) { throw "frontend: vite build failed ($LASTEXITCODE)" }
$ErrorActionPreference = 'Stop'

# Model downloads run under Continue.
$ErrorActionPreference = 'Continue'
# Voiceprints model ---------------------------------------------------------
Write-Host "--- staging voiceprints model ---"
& "$repo_root\models\voiceprints\download.ps1"

# Other bundled models -------------------------------------------------------
# tauri.conf.json (+ tauri.windows.conf.json) bundle.resources declares the
# model paths. The download scripts run under Git Bash.
Write-Host "--- staging nemotron model ---"
bash scripts/download-nemotron.sh
# Live speaker-name diarizer, declared in tauri.conf.json bundle.resources.
Write-Host "--- staging nemotron3 diarization model ---"
bash scripts/download-nemotron3-diar.sh
if ((Get-ChildItem "$repo_root\models\nemotron3-diar" -File -ErrorAction SilentlyContinue | Measure-Object).Count -lt 5) {
    throw "nemotron3 diarization model missing from staging"
}
if (-not (Test-Path "$repo_root\models\nemotron\nemotron-speech-streaming-en-0.6b-560ms-int8\encoder.int8.onnx")) {
    throw "nemotron model missing from staging"
}
# Embedding model for Q&A/RAG.
Write-Host "--- staging embeddings model ---"
bash scripts/download-embeddings.sh
# Segmentation model (segmentation-3.0.onnx), declared in tauri.conf.json
# bundle.resources beside the voiceprint model. download-voiceprints.sh fetches
# both (idempotent).
Write-Host "--- staging segmentation model ---"
bash scripts/download-voiceprints.sh

# Tauri bundle (NSIS + the raw .exe under target\release\) ------------------
# The frontend is built above; tauri.conf.json leaves beforeBuildCommand empty.
# --- code signing (Azure Trusted Signing) -----------------------------------
# Reads .creds-artifact-signing (gitignored) for the service-principal creds +
# the non-secret account/profile, and signs the app exe + NSIS installer via
# trusted-signing-cli during the tauri build. Absent → unsigned build (dev /
# no-creds fallback). The client secret only ever lives in the AZURE_CLIENT_SECRET
# env var — never logged, never written into the config overlay.
$signConfigArg = @()
$credsFile = Join-Path $repo_root '.creds-artifact-signing'
if (Test-Path $credsFile) {
    $creds = @{}
    foreach ($line in Get-Content $credsFile) {
        if ($line -match '^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$') { $creds[$matches[1]] = $matches[2] }
    }
    foreach ($k in 'TENANT_ID','CLIENT_ID','SECRET_VALUE','ACCOUNT_URI','ACCOUNT_NAME','CERT_PROFILE_NAME') {
        if ([string]::IsNullOrWhiteSpace($creds[$k])) { throw "signing: .creds-artifact-signing missing $k" }
    }
    # Microsoft's Sign CLI (dotnet global tool) authenticates from the AZURE_* env
    # vars (DefaultAzureCredential → EnvironmentCredential). The per-user tools dir
    # goes on PATH so the bare `sign` resolves in tauri's signCommand.
    $toolsDir = Join-Path $env:USERPROFILE '.dotnet\tools'
    if (-not (Test-Path (Join-Path $toolsDir 'sign.exe'))) {
        throw "signing creds present but Microsoft 'sign' tool not found in $toolsDir. Install: dotnet tool install --global sign --prerelease"
    }
    $env:PATH = "$toolsDir;$($env:PATH)"
    # Unattended service-principal auth (no interactive sign-in).
    $env:AZURE_TENANT_ID     = $creds['TENANT_ID']
    $env:AZURE_CLIENT_ID     = $creds['CLIENT_ID']
    $env:AZURE_CLIENT_SECRET = $creds['SECRET_VALUE']
    # signCommand carries only the non-secret account/profile/endpoint; %1 is the
    # file tauri hands the signer.
    $certProfile = $creds['CERT_PROFILE_NAME']
    $signCmd = "sign code trusted-signing %1 --trusted-signing-account $($creds['ACCOUNT_NAME']) --trusted-signing-certificate-profile $certProfile --trusted-signing-endpoint $($creds['ACCOUNT_URI'])"
    $overlay = Join-Path $env:TEMP 'daisy-sign.conf.json'
    @{ bundle = @{ windows = @{ signCommand = $signCmd } } } | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $overlay
    $signConfigArg = @('--config', $overlay)
    Write-Host "==> Code signing ENABLED (account $($creds['ACCOUNT_NAME']) / profile $certProfile)" -ForegroundColor Green
} else {
    Write-Host "==> Code signing DISABLED (.creds-artifact-signing absent) - unsigned build" -ForegroundColor Yellow
}

Write-Host "--- staging daisy-llm sidecar ---"
& "$repo_root\scripts\stage-daisy-llm.ps1"

Write-Host "--- tauri bundle (NSIS) ---"
# Earlier runs' installers go first; bundle\nsis\ holds only this version's.
Remove-Item "$target_root\release\bundle\nsis\*.exe" -Force -ErrorAction SilentlyContinue
Push-Location crates/tauri-app
# Vulkan GPU whisper (NVIDIA/AMD/Intel, CPU fallback) is a Windows target
# dep of providers-local. The runtime loader vulkan-1.dll ships in System32
# with every Windows GPU driver.
$ErrorActionPreference = 'Continue'  # cargo/tauri write progress to stderr
cargo tauri build --bundles nsis @signConfigArg
$tauri_exit = $LASTEXITCODE
$ErrorActionPreference = 'Stop'
Pop-Location
if ($tauri_exit -ne 0) { throw "tauri build failed ($tauri_exit)" }

# Assemble dist/daisy-<version>.zip -----------------------------------------
$dist = "$repo_root\dist"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force $dist | Out-Null

$stage = "$dist\stage"
New-Item -ItemType Directory -Force $stage | Out-Null

# NSIS setup.exe for the version just built. Ships as its own dist artifact,
# not inside the portable zip.
$nsis_name = "Daisy_${Version}_x64-setup.exe"
$nsis = "$target_root\release\bundle\nsis\$nsis_name"
if (-not (Test-Path $nsis)) {
    throw "Expected NSIS installer at $nsis. Did ``cargo tauri build --bundles nsis`` succeed for version $Version?"
}
Copy-Item -Force $nsis "$dist\$nsis_name"

# Portable payload at the zip root: daisy-app.exe with its siblings
# (onnxruntime DLLs, models\) beside it.
# Collects the executable, the sidecar, the onnxruntime DLLs and the models\
# directory (bundled models), all at the zip root.
$portable = $stage
$app_exe = "$target_root\release\daisy-app.exe"
if (-not (Test-Path $app_exe)) {
    throw "daisy-app.exe not found at $app_exe. Did the tauri build complete?"
}
# Release-binary license guard (the rules are in license-test-markers.txt;
# scripts/check-no-license-test.sh applies them on Linux and macOS).
$exe_text = [System.Text.Encoding]::GetEncoding(28591).GetString([System.IO.File]::ReadAllBytes($app_exe))
foreach ($line in Get-Content "$PSScriptRoot\license-test-markers.txt") {
    if ($line.StartsWith('+') -and $exe_text.IndexOf($line.Substring(1), [StringComparison]::Ordinal) -lt 0) {
        throw "$app_exe lacks the production license key - wrong file, or a license-test build"
    }
    if ($line.StartsWith('-') -and $exe_text.IndexOf($line.Substring(1), [StringComparison]::Ordinal) -ge 0) {
        throw "$app_exe contains license test-stack marker $($line.Substring(1))"
    }
}
Write-Host "license guard: $app_exe clean"
Copy-Item -Force $app_exe "$portable\daisy-app.exe"

# daisy-llm sidecar, staged by stage-daisy-llm.ps1 and copied next to
# daisy-app.exe.
$llm_exe = "$target_root\release\daisy-llm.exe"
if (-not (Test-Path $llm_exe)) {
    throw "daisy-llm.exe not found at $llm_exe. Did the tauri build complete?"
}
Copy-Item -Force $llm_exe "$portable\daisy-llm.exe"

# Runtime DLLs (onnxruntime, from the `ort` download-binaries crate).
# daisy-app.exe loads these from the same directory.
$dlls = @('onnxruntime.dll', 'onnxruntime_providers_shared.dll')
foreach ($dll in $dlls) {
    $src = "$target_root\release\$dll"
    if (Test-Path $src) { Copy-Item -Force $src "$portable\$dll" }
    else { Write-Host "WARN: missing $src - portable launch may fail" -ForegroundColor Yellow }
}

# Models sit beside daisy-app.exe, as in the NSIS install; the DAISY_*_DIR env
# vars and the crates' current_exe()/models/... fallbacks resolve there:
#   <portable>\models\<model>\...
$resources = $portable
New-Item -ItemType Directory -Force "$resources\models\voiceprints" | Out-Null
New-Item -ItemType Directory -Force "$resources\models\embeddings" | Out-Null
New-Item -ItemType Directory -Force "$resources\models\nemotron" | Out-Null
New-Item -ItemType Directory -Force "$resources\models\nemotron3-diar" | Out-Null
Copy-Item -Force "$repo_root\models\voiceprints\model.onnx" "$resources\models\voiceprints\"
Copy-Item -Recurse -Force "$repo_root\models\nemotron\nemotron-speech-streaming-en-0.6b-560ms-int8" "$resources\models\nemotron\"
Copy-Item -Force "$repo_root\models\nemotron3-diar\nemotron3_diar_fast32.onnx" "$resources\models\nemotron3-diar\"
Copy-Item -Force "$repo_root\models\nemotron3-diar\learnable_sil_emb.bin" "$resources\models\nemotron3-diar\"
Copy-Item -Force "$repo_root\models\nemotron3-diar\nemotron3_diar.json" "$resources\models\nemotron3-diar\"
Copy-Item -Force "$repo_root\models\nemotron3-diar\NOTICE" "$resources\models\nemotron3-diar\"
Copy-Item -Force "$repo_root\models\nemotron3-diar\LICENSE-OpenMDW-1.1.txt" "$resources\models\nemotron3-diar\"
Copy-Item -Force "$repo_root\models\embeddings\model.onnx" "$resources\models\embeddings\"
Copy-Item -Force "$repo_root\models\embeddings\tokenizer.json" "$resources\models\embeddings\"
Copy-Item -Force "$repo_root\models\embeddings\config.json" "$resources\models\embeddings\"
Copy-Item -Force "$repo_root\models\voiceprints\segmentation-3.0.onnx" "$resources\models\voiceprints\"

# Third-party open-source notices are copied into the portable layout.
$licenses = "$repo_root\THIRD-PARTY-LICENSES.txt"
if (Test-Path $licenses) { Copy-Item -Force $licenses "$portable\THIRD-PARTY-LICENSES.txt" }
else { Write-Host "WARN: THIRD-PARTY-LICENSES.txt missing - run scripts/gen-licenses.sh" -ForegroundColor Yellow }
Copy-Item -Force "$repo_root\LICENSE" "$portable\LICENSE"

# Signs the portable payload's executable. Reuses the env (AZURE_*) + PATH +
# $creds set in the signing block above; runs only when signing is enabled.
if ($signConfigArg.Count -gt 0) {
    foreach ($exe in @("$portable\daisy-app.exe", "$portable\daisy-llm.exe")) {
        if (Test-Path $exe) {
            sign code trusted-signing $exe --trusted-signing-account $creds['ACCOUNT_NAME'] --trusted-signing-certificate-profile $certProfile --trusted-signing-endpoint $creds['ACCOUNT_URI']
            if ($LASTEXITCODE -ne 0) { throw "portable sign failed for $exe ($LASTEXITCODE)" }
        }
    }
    Write-Host "==> Portable payload exes signed" -ForegroundColor Green
}

# Signature gate (-RequireSigned): checks every exe the build delivers, before
# the zip is made.
if ($RequireSigned) {
    $unsigned = @()
    foreach ($f in @($nsis, "$dist\$nsis_name", "$portable\daisy-app.exe", "$portable\daisy-llm.exe",
                     "$repo_root\crates\tauri-app\binaries\daisy-llm-x86_64-pc-windows-msvc.exe")) {
        if (-not (Test-Path $f)) { $unsigned += "$f (missing)"; continue }
        $sig = Get-AuthenticodeSignature $f
        Write-Host "signature: $($sig.Status) | $f | $($sig.SignerCertificate.Subject)"
        if ($sig.Status -ne 'Valid') { $unsigned += "$f ($($sig.Status))" }
    }
    if ($unsigned.Count -gt 0) {
        throw ("Windows build is NOT validly signed - refusing to deliver: " + ($unsigned -join '; ') +
               ". Next: check the signing log above (sign.exe errors), confirm .creds-artifact-signing is current " +
               "(the Trusted Signing profile/cert may need re-issuing), then rerun the build.")
    }
    Write-Host "==> Signature gate passed" -ForegroundColor Green
}

# Zip -----------------------------------------------------------------------
$zip = "$dist\daisy-${Version}.zip"
Compress-Archive -Path "$stage\*" -DestinationPath $zip -Force
Remove-Item -Recurse -Force $stage

Write-Host ""
Write-Host "==> Built: $zip" -ForegroundColor Green
Get-ChildItem $dist
