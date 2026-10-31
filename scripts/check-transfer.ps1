<#
.SYNOPSIS
    전송 회귀 점검. 루프백 테스트(무선 없이 프로토콜만)와 실제 전송 기록(engine.log)을 기준선과 견줍니다.

.DESCRIPTION
    1. 루프백: wirdrop/tests/loopback.rs. 한글 이름 파일과 폴더가 그대로 도착하는지, 거절하면 아무것도
       남지 않는지 보고, release 빌드로 256 MB를 보내 속도를 잽니다.
    2. 실제 기록: 엔진이 전송마다 남기는 한 줄을 모읍니다.
         send 3: accepted after 0.48 s, then 376.9 MB delivered in 26.55 s (14.2 MB/s)
         receive 7: 120.4 MB received in 6.10 s (19.7 MB/s)
       MinMB보다 큰 최근 전송들의 중앙값이 기준선의 Tolerance배보다 느리면 실패합니다.

.EXAMPLE
    ./scripts/check-transfer.ps1                 # 루프백과 실제 기록 모두
    ./scripts/check-transfer.ps1 -LogsOnly       # 실제 기록만
    ./scripts/check-transfer.ps1 -SaveBaseline   # 지금 값을 기준선으로 저장
#>
param(
    [switch]$LogsOnly,
    [switch]$SaveBaseline,
    # 이보다 작은 전송은 연결 준비 시간이 더 커서 속도를 보지 않습니다.
    [double]$MinMB = 20,
    # 방향마다 최근 몇 번을 볼지.
    [int]$Last = 10,
    # 기준선의 이 비율보다 느리면 실패.
    [double]$Tolerance = 0.75,
    [string]$LogDir = (Join-Path $env:LOCALAPPDATA 'WirDrop\logs')
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$baselinePath = Join-Path $PSScriptRoot 'transfer-baseline.json'
$baseline = @{}
if (Test-Path $baselinePath) {
    (Get-Content $baselinePath -Raw | ConvertFrom-Json).PSObject.Properties | ForEach-Object { $baseline[$_.Name] = [double]$_.Value }
}
$measured = [ordered]@{}
$failed = $false

function Get-Median([double[]]$values) {
    $sorted = $values | Sort-Object
    $n = $sorted.Count
    if ($n % 2) { return $sorted[($n - 1) / 2] }
    return ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2
}

function Compare-Baseline([string]$key, [double]$value) {
    $measured[$key] = [math]::Round($value, 1)
    if (-not $baseline.ContainsKey($key)) {
        Write-Host ('  {0}: {1:N1} MB/s (기준선 없음)' -f $key, $value)
        return
    }
    $floor = $baseline[$key] * $Tolerance
    $ok = $value -ge $floor
    $mark = if ($ok) { '통과' } else { '느려짐' }
    Write-Host ('  {0}: {1:N1} MB/s, 기준선 {2:N1} MB/s (하한 {3:N1}) {4}' -f $key, $value, $baseline[$key], $floor, $mark)
    if (-not $ok) { $script:failed = $true }
}

if (-not $LogsOnly) {
    Write-Host '루프백 (무선 없이 프로토콜만)'
    Push-Location $root
    try {
        cargo test -q -p wirdrop --test loopback 2>&1 | Out-Host
        if ($LASTEXITCODE) { Write-Host '  기능 테스트 실패'; $failed = $true }
        $out = cargo test -q -p wirdrop --release --test loopback -- --ignored --nocapture 2>&1 | Out-String
        if ($LASTEXITCODE) { Write-Host $out; Write-Host '  속도 테스트 실패'; $failed = $true }
        elseif ($out -match 'LOOPBACK .*\(([\d.]+) MB/s\)') { Compare-Baseline 'loopback' ([double]$Matches[1]) }
    } finally {
        Pop-Location
    }
}

Write-Host ('실제 전송 기록 ({0}, {1} MB 이상, 방향마다 최근 {2}번)' -f $LogDir, $MinMB, $Last)
$logs = @('engine.old.log', 'engine.log') | ForEach-Object { Join-Path $LogDir $_ } | Where-Object { Test-Path $_ }
$runs = @{ send = @(); receive = @() }
foreach ($line in (Get-Content $logs -ErrorAction SilentlyContinue)) {
    if ($line -match 'send \d+: accepted after [\d.]+ s, then ([\d.]+) MB delivered in [\d.]+ s \(([\d.]+) MB/s\)') {
        if ([double]$Matches[1] -ge $MinMB) { $runs.send += [pscustomobject]@{ When = $line.Substring(0, 19); MB = [double]$Matches[1]; Rate = [double]$Matches[2] } }
    } elseif ($line -match 'receive \d+: ([\d.]+) MB received in [\d.]+ s \(([\d.]+) MB/s\)') {
        if ([double]$Matches[1] -ge $MinMB) { $runs.receive += [pscustomobject]@{ When = $line.Substring(0, 19); MB = [double]$Matches[1]; Rate = [double]$Matches[2] } }
    }
}
foreach ($way in 'send', 'receive') {
    $recent = @($runs[$way] | Select-Object -Last $Last)
    if (-not $recent.Count) { Write-Host "  $($way): 기록 없음"; continue }
    $recent | Format-Table When, MB, Rate -AutoSize | Out-Host
    Compare-Baseline $way (Get-Median ($recent | ForEach-Object { $_.Rate }))
}

if ($SaveBaseline) {
    foreach ($k in $measured.Keys) { $baseline[$k] = $measured[$k] }
    $sorted = [ordered]@{}
    $baseline.Keys | Sort-Object | ForEach-Object { $sorted[$_] = $baseline[$_] }
    $sorted | ConvertTo-Json | Set-Content -Encoding utf8 $baselinePath
    Write-Host "기준선을 저장했습니다: $baselinePath"
}
if ($failed) { exit 1 }
