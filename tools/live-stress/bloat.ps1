# bloat.ps1 — interactive CPU + RAM load generator for controller testing.
# Type a number = level. Each level adds 1 busy CPU core + ~300MB resident RAM.
# 0 = idle (clears load). q = quit. Change the number any time to adjust live.

$jobs = @()
$ram  = New-Object System.Collections.ArrayList

function Clear-Load {
    foreach ($j in $script:jobs) {
        Stop-Job   $j -ErrorAction SilentlyContinue
        Remove-Job $j -Force -ErrorAction SilentlyContinue
    }
    $script:jobs = @()
    $script:ram.Clear()
    [GC]::Collect()
}

$cores = [Environment]::ProcessorCount
Write-Host "CPU+RAM bloat. cores=$cores. Enter level (0=off, q=quit). 1 level = 1 busy core + ~300MB."

while ($true) {
    $in = Read-Host "level"
    if ($in -eq 'q') { Clear-Load; Write-Host "cleared. bye."; break }
    $n = 0
    if (-not [int]::TryParse($in, [ref]$n) -or $n -lt 0) { Write-Host "enter a non-negative number or q"; continue }

    Clear-Load

    # CPU: n background jobs each pegging one core with a tight float loop.
    for ($i = 0; $i -lt $n; $i++) {
        $script:jobs += Start-Job {
            $x = 1.0
            while ($true) { for ($k = 0; $k -lt 2000000; $k++) { $x = [math]::Sqrt($x + 1.0) } }
        }
    }

    # RAM: n * ~300MB, page-touched so it's actually resident (not lazy).
    for ($i = 0; $i -lt $n; $i++) {
        $b = New-Object byte[] (300MB)
        for ($p = 0; $p -lt $b.Length; $p += 4096) { $b[$p] = 1 }
        [void]$script:ram.Add($b)
    }

    Write-Host ("ACTIVE: {0} busy cores (of {1}), ~{2}MB RAM held" -f $n, $cores, ($n * 300))
}
