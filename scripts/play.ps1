# ztrx launcher: runs the game for normal play (no scripted presses, no capture).
# The faster settings below are on unless -Plain is given; a VICTORY_* value
# already set in the environment wins over them.
param(
    [Parameter(Mandatory = $true)][string]$Game,
    [string]$Cache = "$env:LOCALAPPDATA\ztrx\cache",
    [string]$Data = "$env:LOCALAPPDATA\ztrx\data",
    [string]$Exe = "$PSScriptRoot\..\target\release\victory.exe",
    [switch]$Plain
)
foreach ($v in "VICTORY_KEYS", "VICTORY_CAPTURE", "VICTORY_CAPTURE_FRAMES", "VICTORY_DUMP_TARGETS") {
    Remove-Item "Env:$v" -ErrorAction SilentlyContinue
}
# Measured on an RTX 5070 in the prologue: 2.1 fps without them, 5.9 fps with
# them (and the indirect budget in the card's memory). Submissions go to the
# card without a wait for each and what shaders wrote is copied back on a
# thread of its own (RELAY=2), on several threads (WIDE_BACK); indirect
# arguments are clamped a window at a time (INDIRECT_WINDOW); title buffers
# of 1 MB or more get their copy in the card's memory, up to 1 GB of them
# (CARD_BUFFERS_*), hashed once per run of pieces (CARD_REHASH_RUNS); a
# texture refresh does not wait while title memory is being remapped
# (TEXTURE_NO_WAIT).
$fast = [ordered]@{
    VICTORY_RELAY               = '2'
    VICTORY_WIDE_BACK           = '1'
    VICTORY_CARD_REHASH_RUNS    = '1'
    VICTORY_INDIRECT_WINDOW     = '1'
    VICTORY_TEXTURE_NO_WAIT     = '1'
    VICTORY_CARD_BUFFERS_MIN_MB = '1'
    VICTORY_CARD_BUFFERS_MB     = '1024'
}
if ($Plain) {
    Write-Host "Faster settings off (-Plain)."
} else {
    $set = foreach ($k in $fast.Keys) {
        if (-not (Test-Path "Env:$k")) {
            Set-Item "Env:$k" $fast[$k]
            "$k=$($fast[$k])"
        }
    }
    Write-Host "Faster settings on: $($set -join ' ') (-Plain turns them off)."
}
New-Item -ItemType Directory -Force $Cache, $Data | Out-Null
$env:VICTORY_CACHE_DIR = $Cache
$env:VICTORY_DATA_DIR = $Data
& $Exe run $Game --execute 2>&1 | Out-File "$env:TEMP\ztrx.log" -Encoding utf8
Write-Host "Exited with $LASTEXITCODE; log: $env:TEMP\ztrx.log"
