# Fetch the WeSpeaker speaker-embedding ONNX model used by the
# voiceprint pipeline (~26 MB). The model is public on Hugging Face
# (Wespeaker/wespeaker-voxceleb-resnet34-LM, CC-BY-4.0; attribution in
# scripts/lib/model-notices.txt).
# ASCII-only on purpose (Windows PowerShell 5.1 reads .ps1 as Windows-1252;
# UTF-8 multibyte characters trip the parser).
#
# Usage (from repo root):
#   powershell -ExecutionPolicy Bypass -File models\voiceprints\download.ps1

$ErrorActionPreference = 'Stop'
$outDir = Split-Path -Parent $PSCommandPath
$dst    = Join-Path $outDir 'model.onnx'
$url    = 'https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM/resolve/main/voxceleb_resnet34_LM.onnx'

if ((Test-Path $dst) -and ((Get-Item $dst).Length -gt 1000000)) {
  Write-Host ("exists: model.onnx (" + (Get-Item $dst).Length + " bytes)")
  exit 0
}

Write-Host ("Downloading voiceprint model into " + $outDir + "\")
$tmp = $dst + '.tmp'
# Faster than Invoke-WebRequest for large files on PS 5.1 (no progress-bar
# object churn).
$ProgressPreference = 'SilentlyContinue'
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
Move-Item -Force $tmp $dst

$size = (Get-Item $dst).Length
if ($size -lt 1000000) {
  Write-Warning ("model.onnx is only " + $size + " bytes -- download truncated?")
  exit 1
}
Write-Host ("  OK: model.onnx (" + $size + " bytes)")
Write-Host "Done. Voiceprints will pick this up via voiceprints::model_dir()."
