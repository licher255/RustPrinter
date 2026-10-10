param([Parameter(Mandatory = $true)][string]$ManifestPath)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Text.UTF8Encoding]::new($false)
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$repoPrefix = $repo.TrimEnd('\') + '\'
if (-not [IO.Path]::IsPathRooted($ManifestPath)) { throw 'ManifestPath must be absolute.' }
$path = [IO.Path]::GetFullPath($ManifestPath)
if (-not $path.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'ManifestPath must be inside this workspace.' }
if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Manifest not found: $path" }
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { throw 'Run this script from an elevated PowerShell window (Administrator).' }

$manifest = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
$donePath = Join-Path (Split-Path -Parent $path) 'done.json'
if (-not (Test-Path -LiteralPath $donePath -PathType Leaf) -or -not ((Get-Content -LiteralPath $donePath -Raw | ConvertFrom-Json).done)) { throw 'Capture is not marked done; refusing to touch pktmon.' }
if ($manifest.session -notmatch '^AirPrinter_(\d+)$') { throw 'Manifest session name is not from capture_print_chain.ps1.' }
$pidPart = $Matches[1]
$expected = @("ap_ipp631_$pidPart", "ap_ipp8631_$pidPart", "ap_mdns_$pidPart")
$recorded = @($manifest.steps | Where-Object { $_.name -like 'add filter ap_*' } | ForEach-Object { $_.name -replace '^add filter ', '' })
if ($recorded.Count -ne 3 -or @($expected | Where-Object { $_ -notin $recorded }).Count -ne 0) { throw 'Manifest does not prove that all three expected filters were created.' }

function Invoke-Pktmon([string[]]$Arguments) {
    $output = @(& pktmon.exe @Arguments 2>&1 | ForEach-Object { "$_" })
    $code = $LASTEXITCODE
    if ($code -ne 0) { throw "pktmon $($Arguments -join ' ') failed (exit $code): $($output -join ' ')" }
    return ,$output
}
$status = (Invoke-Pktmon @('status')) -join "`n"
if ($status -match '(?i)running|active|正在运行|已启动' -and $status -notmatch '(?i)not running|not active|没有运行|未运行|未启动') { throw 'pktmon is active; refusing to remove filters.' }
if ($status -notmatch '(?i)not running|not active|没有运行|未运行|未启动') { throw "Could not verify pktmon is stopped: $status" }

$listing = (Invoke-Pktmon @('filter','list')) -join "`n"
$names = @([regex]::Matches($listing, '(?m)^\s*\d+\s+(\S+)\s+') | ForEach-Object { $_.Groups[1].Value })
if ($names.Count -ne 3 -or @($expected | Where-Object { $_ -notin $names }).Count -ne 0) { throw "Current pktmon filters do not exactly match this capture's three filters; left untouched.`n$listing" }

$null = Invoke-Pktmon @('filter','remove')
$after = (Invoke-Pktmon @('filter','list')) -join "`n"
$remaining = @([regex]::Matches($after, '(?m)^\s*\d+\s+(\S+)\s+'))
if ($remaining.Count -ne 0 -or $after -notmatch '(?i)no filters|数据包筛选器\s*[:：]\s*无') { throw "Post-cleanup filter state is ambiguous; inspect manually.`n$after" }
Write-Output "Removed the three pktmon filters recorded by $path"
