param(
    [string]$PhoneIp = '192.168.3.197',
    [string]$PrinterName = 'FF Q501h for DocuPrint P115 b',
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [ValidateRange(30, 180)][int]$DurationSeconds = 90
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Text.UTF8Encoding]::new($false)
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not [IO.Path]::IsPathRooted($OutputDirectory)) { throw 'OutputDirectory must be an absolute workspace path.' }
$workspace = [IO.Path]::GetFullPath($OutputDirectory)
$repoPrefix = $repo.TrimEnd('\') + '\'
if (-not ($workspace.Equals($repo, [StringComparison]::OrdinalIgnoreCase) -or $workspace.StartsWith($repoPrefix, [StringComparison]::OrdinalIgnoreCase))) { throw 'OutputDirectory must be inside this workspace.' }
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { throw 'Run this script from an elevated PowerShell window (Administrator).' }

$mutex = [Threading.Mutex]::new($false, 'Global\AirPrinterPrintCapture')
if (-not $mutex.WaitOne(0)) { throw 'Another AirPrinter capture script is already running.' }
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$session = "AirPrinter_$PID"
$etwSession = "${session}_$([DateTime]::UtcNow.Ticks)"
$dir = Join-Path $workspace "target\print-capture\$stamp"
$null = New-Item -ItemType Directory -Path $dir -Force
$manifestPath = Join-Path $dir 'manifest.json'
$manifest = [ordered]@{ session = $session; etwSession = $etwSession; started = (Get-Date).ToString('o'); durationSeconds = $DurationSeconds; phoneIp = $PhoneIp; printerName = $PrinterName; output = $dir; steps = @(); warnings = @(); failures = @(); usbEtwNote = 'PrintService-USBMon ETW is driver/event tracing; it is not a raw USB payload capture.' }
$pktStarted = $false; $ippStarted = $false; $usbStarted = $false; $filtersAdded = @()
$captureStart = $null; $captureEnd = $null

function Save-Manifest { $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding UTF8 }
function Step([string]$Name, [string]$Detail = '', $ExitCode = $null) { $manifest.steps += [ordered]@{ time = (Get-Date).ToString('o'); name = $Name; exitCode = $ExitCode; detail = $Detail }; Save-Manifest }
function Run-Native([string]$Exe, [string[]]$NativeArguments, [string]$Name) {
    $out = @(& $Exe @NativeArguments 2>&1 | ForEach-Object { "$_" })
    $code = $LASTEXITCODE
    if ($code -ne 0) { Step $Name ($out -join ' ') $code; throw "$Name failed (exit $code): $($out -join ' ')" }
    Step $Name ($out -join ' ') $code
    return ,$out
}
function Save-Snapshot([string]$Label) {
    $snapshot = [ordered]@{ time = (Get-Date).ToString('o'); processes = @(); ports = @(); printer = $null; ips = @() }
    $snapshot.processes = @(Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^(AirPrinter|spoolsv)\.exe$' } | Select-Object ProcessId, Name, ExecutablePath)
    $snapshot.ports = @(Get-NetTCPConnection -ErrorAction SilentlyContinue | Where-Object { $_.LocalPort -in 631,8631 -or $_.RemotePort -in 631,8631 } | Select-Object LocalAddress,LocalPort,RemoteAddress,RemotePort,State,OwningProcess)
    $snapshot.printer = Get-Printer -Name $PrinterName -ErrorAction SilentlyContinue | Select-Object Name,DriverName,PortName,PrinterStatus,WorkOffline
    $snapshot.ips = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.IPAddress -notlike '127.*' } | Select-Object InterfaceAlias,IPAddress,PrefixLength)
    $snapshot | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $dir "$Label-snapshot.json") -Encoding UTF8
    Step "$Label snapshot" 'process executable paths, IPP ports, printer driver/port, local IPv4 addresses'
}
function Save-QueueSample {
    $jobs = @(Get-PrintJob -PrinterName $PrinterName -ErrorAction SilentlyContinue | Select-Object ID,JobStatus,Size,TotalPages,PagesPrinted)
    [ordered]@{ time = (Get-Date).ToString('o'); jobs = $jobs } | ConvertTo-Json -Depth 4 -Compress | Add-Content -LiteralPath (Join-Path $dir 'queue.jsonl') -Encoding UTF8
}
function Copy-AppDiagnostics {
    $path = Join-Path $env:APPDATA 'com.airprinter.app\print-diagnostics.jsonl'
    foreach ($file in @($path, "$path.previous")) { if (Test-Path -LiteralPath $file) { Copy-Item -LiteralPath $file -Destination (Join-Path $dir ([IO.Path]::GetFileName($file))) -Force } }
    Step 'copy app diagnostics' $path
}

try {
    Save-Manifest
    $status = (Run-Native 'pktmon.exe' @('status') 'pktmon status check') -join "`n"
    if ($status -match '(?i)not running|not active|未运行|没有运行|未启动|未在运行') { }
    elseif ($status -match '(?i)running|active|正在运行|已启动') { throw 'pktmon is already active; leaving it untouched.' }
    else { throw "Could not safely determine pktmon state from localized output: $status" }
    $beforeFilters = (Run-Native 'pktmon.exe' @('filter','list') 'pktmon filter preflight') -join "`n"
    if ($beforeFilters -match '(?i)no filters|没有数据包筛选器|没有筛选器|无筛选器|没有过滤器|无过滤器' -or $beforeFilters -match '(?s)^\s*数据包筛选器\s*[:：]\s*无\s*$') { }
    elseif ($beforeFilters -match '(?i)filter\s*(id|#)|筛选器|过滤器') { throw 'pktmon already has filters; leaving them untouched.' }
    else { throw "Could not safely determine pktmon filter state from localized output: $beforeFilters" }
    try { $null = Run-Native 'logman.exe' @('query','-ets') 'ETW session inventory (optional)' }
    catch {
        $manifest.warnings += "Optional logman session inventory failed: $($_.Exception.Message)"
        Save-Manifest
    }

    $null = [Net.IPAddress]::Parse($PhoneIp)
    $route = Find-NetRoute -RemoteIPAddress $PhoneIp -ErrorAction Stop | Select-Object -First 1
    $local = Get-NetIPAddress -InterfaceIndex $route.InterfaceIndex -AddressFamily IPv4 -ErrorAction Stop | Where-Object { $_.IPAddress -eq $route.IPAddress } | Select-Object -First 1
    if (-not $local) { throw "Could not determine the local subnet for $PhoneIp." }
    $cidr = "$($local.IPAddress)/$($local.PrefixLength)"
    $specs = @(
        @{ Suffix='ipp631'; Args=@('-t','TCP','-i',$PhoneIp,'-p','631') },
        @{ Suffix='ipp8631'; Args=@('-t','TCP','-i',$PhoneIp,'-p','8631') },
        @{ Suffix='mdns'; Args=@('-t','UDP','-i',$cidr,'-p','5353') }
    )
    foreach ($spec in $specs) {
        $name = "ap_$($spec.Suffix)_$PID"
        $null = Run-Native 'pktmon.exe' (@('filter','add',$name) + $spec.Args) "add filter $name"
        $filtersAdded += $name
    }
    $null = Run-Native 'pktmon.exe' @('start','--capture','--comp','all','--pkt-size','0','--file-size','64','--log-mode','circular','--file-name',(Join-Path $dir 'network.etl')) 'start pktmon'
    $pktStarted = $true

    $null = Run-Native 'logman.exe' @('create','trace',($etwSession + '_PrintService'),'-ow','-o',(Join-Path $dir 'printservice.etl'),'-p','Microsoft-Windows-PrintService','0xFFFFFFFF','5','-ets') 'create PrintService ETW'
    $ippStarted = $true
    $null = Run-Native 'logman.exe' @('create','trace',($etwSession + '_USBMon'),'-ow','-o',(Join-Path $dir 'usbmon.etl'),'-p','Microsoft-Windows-PrintService-USBMon','0xFFFFFFFF','5','-ets') 'create USBMon ETW'
    $usbStarted = $true

    $captureStart = Get-Date
    Save-Snapshot 'start'; Copy-AppDiagnostics
    [ordered]@{ ready = $true; time = $captureStart.ToString('o'); directory = $dir; durationSeconds = $DurationSeconds } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $dir 'ready.json') -Encoding UTF8
    Step 'ready' 'Capture sessions running; start the phone print attempt now.'
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $DurationSeconds) { Save-QueueSample; Start-Sleep -Milliseconds 500 }
    $captureEnd = Get-Date
    Save-Snapshot 'end'; Copy-AppDiagnostics
} catch {
    $manifest.failures += [ordered]@{ time = (Get-Date).ToString('o'); message = $_.Exception.Message }
    Save-Manifest
} finally {
    if ($pktStarted) { try { $null = Run-Native 'pktmon.exe' @('stop') 'stop pktmon' } catch { $manifest.failures += $_.Exception.Message } }
    if ($ippStarted) { try { $null = Run-Native 'logman.exe' @('stop',($etwSession + '_PrintService'),'-ets') 'stop PrintService ETW' } catch { $manifest.failures += $_.Exception.Message } }
    if ($usbStarted) { try { $null = Run-Native 'logman.exe' @('stop',($etwSession + '_USBMon'),'-ets') 'stop USBMon ETW' } catch { $manifest.failures += $_.Exception.Message } }
    foreach ($etl in @('network','printservice','usbmon')) {
        $source = Join-Path $dir "$etl.etl"
        if (Test-Path -LiteralPath $source) {
            if ($etl -eq 'network') { try { $null = Run-Native 'pktmon.exe' @('etl2pcap',$source,'--out',(Join-Path $dir 'network.pcapng')) 'export pcapng'; $null = Run-Native 'pktmon.exe' @('etl2txt',$source,'--out',(Join-Path $dir 'network.txt')) 'export packet text' } catch { $manifest.failures += $_.Exception.Message } }
        }
    }
    foreach ($channel in @('Microsoft-Windows-PrintService/Admin','Microsoft-Windows-PrintService/Operational')) {
        try {
            $eventStart = if ($captureStart) { $captureStart } else { (Get-Date).AddMinutes(-1) }
            $eventEnd = if ($captureEnd) { $captureEnd } else { Get-Date }
            $events = @(Get-WinEvent -FilterHashtable @{ LogName=$channel; StartTime=$eventStart; EndTime=$eventEnd } -ErrorAction Stop)
            $events | ForEach-Object { $_.ToXml() } | Set-Content -LiteralPath (Join-Path $dir (($channel -replace '.*/','') + '.xml')) -Encoding UTF8
            Step "export events $channel" "$($events.Count) events"
        } catch { Step "events unavailable $channel" $_.Exception.Message }
    }
    if ($filtersAdded.Count -gt 0) {
        try {
            $current = (Run-Native 'pktmon.exe' @('filter','list') 'pktmon filter cleanup check') -join "`n"
            $allPresent = @($filtersAdded | Where-Object { $current -notmatch [regex]::Escape($_) }).Count -eq 0
            $names = @($current -split "`r?`n" | ForEach-Object {
                if ($_ -match '^\s*\d+\s+(\S+)\s+') { $Matches[1] }
                elseif ($_ -match '^\s*(Name|名称)\s*:\s*(.+?)\s*$') { $Matches[2] }
            })
            $exactNames = ($names.Count -eq $filtersAdded.Count) -and (@($filtersAdded | Where-Object { $_ -notin $names }).Count -eq 0)
            if ($allPresent -and $exactNames) { $null = Run-Native 'pktmon.exe' @('filter','remove') 'remove own pktmon filters' }
            else { $manifest.failures += 'pktmon filters changed during capture; left all filters untouched for safety.' }
        } catch { $manifest.failures += $_.Exception.Message }
    }
    if (-not $captureEnd) { $captureEnd = Get-Date }
    $manifest.ended = $captureEnd.ToString('o')
    $manifest.exitCode = if ($manifest.failures.Count) { 1 } else { 0 }
    Save-Manifest
    [ordered]@{ done = $true; time = (Get-Date).ToString('o'); exitCode = $manifest.exitCode; failures = $manifest.failures } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $dir 'done.json') -Encoding UTF8
    $mutex.ReleaseMutex(); $mutex.Dispose()
    exit $manifest.exitCode
}
