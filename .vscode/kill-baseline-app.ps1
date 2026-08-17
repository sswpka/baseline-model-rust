param(
    [string]$WorkspaceFolder = (Split-Path -Parent $PSScriptRoot)
)

$targetDir = Join-Path $WorkspaceFolder 'target'

# Kill any process actually running out of this project's target dir (debug/release/deps/etc.),
# not just an exact "baseline-app.exe" name match.
$procs = Get-Process -ErrorAction SilentlyContinue | Where-Object {
    $_.Path -and $_.Path.StartsWith($targetDir, [System.StringComparison]::OrdinalIgnoreCase)
}

if ($procs) {
    foreach ($p in $procs) {
        Write-Host "Killing stale process: $($p.ProcessName) (PID $($p.Id))"
    }
    $procs | Stop-Process -Force -ErrorAction SilentlyContinue
    $procs | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
}

# taskkill returning doesn't guarantee Windows/AV has released the file handle yet.
# Poll until the known binaries can be opened exclusively (or give up after 10s).
$binaries = @(
    Join-Path $targetDir 'debug\baseline-app.exe'
    Join-Path $targetDir 'release\baseline-app.exe'
)

$deadline = (Get-Date).AddSeconds(10)
while ((Get-Date) -lt $deadline) {
    $locked = $false
    foreach ($bin in $binaries) {
        if (Test-Path $bin) {
            try {
                $fs = [System.IO.File]::Open($bin, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
                $fs.Close()
            } catch {
                $locked = $true
            }
        }
    }
    if (-not $locked) { break }
    Start-Sleep -Milliseconds 200
}

exit 0
