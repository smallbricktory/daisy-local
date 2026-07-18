# stress.ps1 - interactive whisper-transcription load for live-controller testing.
#
# Runs N concurrent whisper (Vulkan) transcriptions in a loop; they compete for
# the SAME GPU/CPU the live decoder uses, so the live decode_ms / rho climbs hard
# and you can watch the catch-up controller shed (raise hop) in the perf log.
#
# At the "workers" prompt: type a target number (0=off, q=quit, +/- to step).
#
# Needs a release-built whisper_bench.exe (the `vulkan` feature), a 16kHz mono
# WAV, and a ggml whisper model. Pass paths or set the defaults below.
#
#   powershell -ExecutionPolicy Bypass -File stress.ps1
#   powershell -ExecutionPolicy Bypass -File stress.ps1 -Bench D:\b\release\whisper_bench.exe -Model C:\tmp\ggml-base.en.bin -Wav C:\tmp\meeting-10min.wav

param(
    [string]$Bench = 'D:\b\release\whisper_bench.exe',
    [string]$Model = 'C:\tmp\stress-model.bin',
    [string]$Wav   = 'C:\tmp\stress.wav',
    [int]$Threads  = 4   # threads per worker (one steady Vulkan decode loop)
)

$jobs = @()

function Set-Workers([int]$n) {
    while ($script:jobs.Count -lt $n) {
        $script:jobs += Start-Job -ArgumentList $Bench, $Model, $Wav, $Threads -ScriptBlock {
            param($b, $m, $w, $th)
            $env:BENCH_THREADS = "$th"
            while ($true) { & $b $m $w 2>&1 | Out-Null }
        }
    }
    while ($script:jobs.Count -gt $n) {
        $j = $script:jobs[-1]
        $script:jobs = @($script:jobs[0..($script:jobs.Count - 2)])
        Stop-Job $j -EA SilentlyContinue
        Remove-Job $j -Force -EA SilentlyContinue
    }
    # Reap any orphaned decode children beyond the target.
    $procs = @(Get-Process whisper_bench -EA SilentlyContinue)
    if ($procs.Count -gt $n) {
        $procs | Select-Object -Last ($procs.Count - $n) | Stop-Process -Force -EA SilentlyContinue
    }
}

if (-not (Test-Path $Bench)) { Write-Host "missing bench: $Bench - build whisper_bench (vulkan) first"; exit 1 }
if (-not (Test-Path $Model)) { Write-Host "missing model: $Model"; exit 1 }
if (-not (Test-Path $Wav))   { Write-Host "missing wav: $Wav"; exit 1 }

$target = 0
Write-Host "whisper stress. Type target number of concurrent transcriptions (0=off, q=quit, +/- step)."
while ($true) {
    $in = Read-Host "workers"
    if ($in -eq 'q') {
        Set-Workers 0
        Get-Process whisper_bench -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
        Write-Host "stopped. bye."
        break
    }
    if ($in -eq '+') { $target++ }
    elseif ($in -eq '-') { $target = [Math]::Max(0, $target - 1) }
    elseif ([int]::TryParse($in, [ref]$target)) { $target = [Math]::Max(0, $target) }
    else { Write-Host "number, +, -, or q"; continue }
    Set-Workers $target
    Write-Host ("ACTIVE: {0} concurrent transcriptions" -f $target)
}
