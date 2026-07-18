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
# Absent → unsigned build. See the signing block below.
# Build-box prereqs: .NET SDK + `dotnet tool install --global sign --prerelease`.

[CmdletBinding()]
param(
    [string]$Version
)

$ErrorActionPreference = 'Stop'

# pnpm refuses to purge/recreate node_modules non-interactively without a TTY
# unless CI is set (GitHub Actions sets it automatically; a manual run
# doesn't). Mirrors build-inside.sh.
if (-not $env:CI) { $env:CI = 'true' }

# Newer CMake (>=4.0) removed compatibility with CMakeLists requiring <3.5, which
# breaks vendored C deps with old policy mins (audiopus_sys/opus, aec/onnxruntime).
# CMake honors this env var as the floor; set it unless already provided.
if (-not $env:CMAKE_POLICY_VERSION_MINIMUM) { $env:CMAKE_POLICY_VERSION_MINIMUM = '3.5' }

$repo_root = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $repo_root

# Vulkan GPU whisper backend (NVIDIA/AMD/Intel; CPU fallback when no device).
# The runner has the Vulkan SDK (glslc) installed; a service-launched shell
# may not inherit the machine VULKAN_SDK env, and it is resolved explicitly.
if (-not $env:VULKAN_SDK) {
    $vk = Get-ChildItem 'C:\VulkanSDK' -Directory -ErrorAction SilentlyContinue |
          Sort-Object Name -Descending | Select-Object -First 1
    if ($vk) { $env:VULKAN_SDK = $vk.FullName }
}
if ($env:VULKAN_SDK) { $env:PATH = "$($env:VULKAN_SDK)\Bin;$($env:PATH)" }
# whisper.cpp's vulkan-shaders-gen ExternalProject overflows Windows
# MAX_PATH (260) under the normal deep target/ path; the build goes into a
# short dir. All target references below honor $target_root.
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'D:\b' }
New-Item -ItemType Directory -Force $env:CARGO_TARGET_DIR | Out-Null
$target_root = $env:CARGO_TARGET_DIR

if (-not $Version) {
    # Match Linux pipeline: version comes from crates/tauri-app/Cargo.toml.
    $cargoToml = Get-Content "$repo_root/crates/tauri-app/Cargo.toml" -Raw
    if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
        $Version = $matches[1]
    } else {
        throw "Couldn't read tauri-app version from Cargo.toml"
    }
}

Write-Host "==> Building Daisy $Version (Windows)" -ForegroundColor Cyan

# Frontend ------------------------------------------------------------------
# vite writes its chunk-size WARNING to stderr; under
# $ErrorActionPreference=Stop, Windows PowerShell 5.1 turns that into a
# terminating NativeCommandError. The native frontend commands run under
# Continue and $LASTEXITCODE gates each step.
$ErrorActionPreference = 'Continue'
Write-Host "--- frontend deps ---"
pnpm --dir apps/frontend install --frozen-lockfile
if ($LASTEXITCODE -ne 0) { throw "frontend: pnpm install failed ($LASTEXITCODE)" }
Write-Host "--- frontend build ---"
pnpm --dir apps/frontend exec tsc -b
if ($LASTEXITCODE -ne 0) { throw "frontend: tsc -b failed ($LASTEXITCODE)" }
pnpm --dir apps/frontend exec vite build
if ($LASTEXITCODE -ne 0) { throw "frontend: vite build failed ($LASTEXITCODE)" }
$ErrorActionPreference = 'Stop'

# Model downloads use curl/bash which write progress to stderr; run under
# Continue (a real failure surfaces later as a missing bundle.resource).
$ErrorActionPreference = 'Continue'
# Voiceprints model ---------------------------------------------------------
Write-Host "--- staging voiceprints model ---"
& "$repo_root\models\voiceprints\download.ps1"

# Other bundled models -------------------------------------------------------
# tauri.conf.json's bundle.resources declares the whisper + dtln-aec + voiceprint
# paths. Without these files on disk the tauri-app build bails with "resource
# path ... doesn't exist". The Linux container build runs the same download
# scripts from build-inside.sh; Git Bash is on PATH (Add Git Bash to PATH
# step in release.yml) and they are invoked here directly.
Write-Host "--- staging whisper + dtln-aec models ---"
bash scripts/download-whisper.sh
# BGE embedding model for Q&A/RAG (embeddings crate hard-fails without it).
Write-Host "--- staging embeddings model ---"
bash scripts/download-embeddings.sh
# Speakrs diarization model set (ONNX + PLDA .npy matrices), declared in
# tauri.conf.json bundle.resources. download-voiceprints.sh fetches both the
# voiceprint model (idempotent — already staged above) AND the speakrs set;
# without this the tauri-app build bails: "resource path ...\models\speakrs\
# plda_lda.npy doesn't exist". Linux/macOS get it via the same script.
Write-Host "--- staging speakrs models ---"
bash scripts/download-voiceprints.sh

# Tauri bundle (NSIS + the raw .exe under target\release\) ------------------
# Frontend is already built above. tauri.conf.json leaves beforeBuildCommand
# empty; tauri-cli's cwd for beforeBuildCommand differs between Linux, the CI
# Windows runner, and a plain user checkout. The build scripts own the
# frontend build.
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
    # Microsoft's Sign CLI (dotnet global tool) — authenticates straight from
    # the AZURE_* env vars (DefaultAzureCredential → EnvironmentCredential):
    # no Azure CLI, no interactive login. Installed per-user; the build's
    # fresh shell may not have ~/.dotnet/tools on PATH. The per-user tools
    # dir goes on PATH and the bare `sign` resolves in tauri's signCommand.
    # Tauri splits signCommand on whitespace and treats the first token as
    # the program; a quoted full path does not parse there.
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
    # file tauri hands the signer. ($certProfile, not $profile, which is a
    # PowerShell automatic variable.)
    $certProfile = $creds['CERT_PROFILE_NAME']
    $signCmd = "sign code trusted-signing %1 --trusted-signing-account $($creds['ACCOUNT_NAME']) --trusted-signing-certificate-profile $certProfile --trusted-signing-endpoint $($creds['ACCOUNT_URI'])"
    $overlay = Join-Path $env:TEMP 'daisy-sign.conf.json'
    @{ bundle = @{ windows = @{ signCommand = $signCmd } } } | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $overlay
    $signConfigArg = @('--config', $overlay)
    Write-Host "==> Code signing ENABLED (account $($creds['ACCOUNT_NAME']) / profile $certProfile)" -ForegroundColor Green
} else {
    Write-Host "==> Code signing DISABLED (.creds-artifact-signing absent) - unsigned build" -ForegroundColor Yellow
}

Write-Host "--- tauri bundle (NSIS) ---"
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

# NSIS setup.exe, pinned to the version just built —
# `target\release\bundle\nsis\` accumulates installers across versions on
# incremental builds. Ships as its own dist artifact, not inside the
# portable zip.
$nsis_name = "Daisy_${Version}_x64-setup.exe"
$nsis = "$target_root\release\bundle\nsis\$nsis_name"
if (-not (Test-Path $nsis)) {
    throw "Expected NSIS installer at $nsis. Did ``cargo tauri build --bundles nsis`` succeed for version $Version?"
}
Copy-Item -Force $nsis "$dist\$nsis_name"

# Portable payload at the zip root — not inside a nested folder. "Extract
# All" yields a clean daisy-<version>\daisy-app.exe with its siblings
# (daisy.exe sidecar, onnxruntime DLLs, resources\models\) beside it.
# Collects the executable, the WebView2 loader (if present), and the
# resources/ directory (which contains the bundled voiceprints model).
$portable = $stage
$app_exe = "$target_root\release\daisy-app.exe"
if (-not (Test-Path $app_exe)) {
    throw "daisy-app.exe not found at $app_exe. Did the tauri build complete?"
}
Copy-Item -Force $app_exe "$portable\daisy-app.exe"

# Runtime DLLs (onnxruntime, from the `ort` download-binaries crate).
# daisy-app.exe loads these from the same directory; Tauri's NSIS run stages
# them into target/release/ but does not copy them into a per-portable
# layout. They are copied explicitly here.
$dlls = @('onnxruntime.dll', 'onnxruntime_providers_shared.dll')
foreach ($dll in $dlls) {
    $src = "$target_root\release\$dll"
    if (Test-Path $src) { Copy-Item -Force $src "$portable\$dll" }
    else { Write-Host "WARN: missing $src - portable launch may fail" -ForegroundColor Yellow }
}

# Models — Tauri's bundle.resources only flow into the NSIS installer's
# unpacked tree, not into the portable layout. Mirror the resource-dir
# layout that runtime env vars (DAISY_*_DIR) expect:
#   <portable>\resources\models\<model>\...
$resources = "$portable\resources"
New-Item -ItemType Directory -Force "$resources\models\voiceprints" | Out-Null
New-Item -ItemType Directory -Force "$resources\models\whisper" | Out-Null
New-Item -ItemType Directory -Force "$resources\models\dtln-aec" | Out-Null
New-Item -ItemType Directory -Force "$resources\models\embeddings" | Out-Null
Copy-Item -Force "$repo_root\models\voiceprints\model.onnx" "$resources\models\voiceprints\"
Copy-Item -Force "$repo_root\models\whisper\ggml-base.en.bin" "$resources\models\whisper\"
Copy-Item -Force "$repo_root\models\dtln-aec\model_256_1.onnx" "$resources\models\dtln-aec\"
Copy-Item -Force "$repo_root\models\dtln-aec\model_256_2.onnx" "$resources\models\dtln-aec\"
Copy-Item -Force "$repo_root\models\embeddings\model.onnx" "$resources\models\embeddings\"
Copy-Item -Force "$repo_root\models\embeddings\tokenizer.json" "$resources\models\embeddings\"
Copy-Item -Force "$repo_root\models\embeddings\config.json" "$resources\models\embeddings\"
# speakrs diarization (CPU path on Windows — ONNX + PLDA, no CoreML mlmodelc).
New-Item -ItemType Directory -Force "$resources\models\speakrs" | Out-Null
Copy-Item -Force "$repo_root\models\speakrs\segmentation-3.0.onnx" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\wespeaker-voxceleb-resnet34.onnx" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\wespeaker-voxceleb-resnet34.onnx.data" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\wespeaker-voxceleb-resnet34.min_num_samples.txt" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\plda_lda.npy" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\plda_mean1.npy" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\plda_mean2.npy" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\plda_mu.npy" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\plda_psi.npy" "$resources\models\speakrs\"
Copy-Item -Force "$repo_root\models\speakrs\plda_tr.npy" "$resources\models\speakrs\"

# Third-party open-source notices. The NSIS installer gets this via Tauri's
# bundle.resources; the portable layout is hand-assembled here and it is
# copied in explicitly (MIT/Apache/BSD require the text to accompany the
# binary).
$licenses = "$repo_root\THIRD-PARTY-LICENSES.txt"
if (Test-Path $licenses) { Copy-Item -Force $licenses "$portable\THIRD-PARTY-LICENSES.txt" }
else { Write-Host "WARN: THIRD-PARTY-LICENSES.txt missing - run scripts/gen-licenses.sh" -ForegroundColor Yellow }
Copy-Item -Force "$repo_root\LICENSE" "$portable\LICENSE"

# Signs the portable payload's executable. Tauri signs the binary it packs
# into the NSIS installer; this zip is hand-assembled from the raw (unsigned)
# build output and the staged copy is signed here. Reuses the env (AZURE_*) +
# PATH + $creds set in the signing block above; runs only when signing is
# enabled.
if ($signConfigArg.Count -gt 0) {
    foreach ($exe in @("$portable\daisy-app.exe")) {
        if (Test-Path $exe) {
            sign code trusted-signing $exe --trusted-signing-account $creds['ACCOUNT_NAME'] --trusted-signing-certificate-profile $certProfile --trusted-signing-endpoint $creds['ACCOUNT_URI']
            if ($LASTEXITCODE -ne 0) { throw "portable sign failed for $exe ($LASTEXITCODE)" }
        }
    }
    Write-Host "==> Portable payload exes signed" -ForegroundColor Green
}

# Zip -----------------------------------------------------------------------
$zip = "$dist\daisy-${Version}.zip"
Compress-Archive -Path "$stage\*" -DestinationPath $zip -Force
Remove-Item -Recurse -Force $stage

Write-Host ""
Write-Host "==> Built: $zip" -ForegroundColor Green
Get-ChildItem $dist
