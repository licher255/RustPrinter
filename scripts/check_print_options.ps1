# Default checks do not access printers; -Simulate uses Microsoft Print to PDF only.
param([switch] $Simulate)
$ErrorActionPreference = 'Stop'
$source = Get-Content -LiteralPath (Join-Path $PSScriptRoot '../src-tauri/src/services/windows_print.ps1') -Raw
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
$selection = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Select-Page' }, $true)
. ([scriptblock]::Create($selection.Extent.Text))
$job = '{"page_ranges":[[2,3],[5,5]]}' | ConvertFrom-Json
foreach ($page in 1..7) {
    if ((Select-Page $page) -ne ($page -in @(2,3,5))) { throw "Incorrect selection for page $page" }
}
$job = '{"page_ranges":[[2,3]]}' | ConvertFrom-Json
if ((Select-Page 1) -or -not (Select-Page 2) -or -not (Select-Page 3) -or (Select-Page 4)) { throw 'Single range selection failed' }
$job = '{"page_ranges":[]}' | ConvertFrom-Json
if (-not (Select-Page 1)) { throw 'Default must print all pages' }

Add-Type -AssemblyName System.Drawing
$native = [regex]::Match($source, "(?s)Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'\r?\n(.*?)\r?\n'@")
if (-not $native.Success) { throw 'Native renderer source not found' }
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition $native.Groups[1].Value
function Check-Bounds($imageWidth, $imageHeight, $mode, $fidelity, $expectedWidth, $expectedHeight) {
    $bounds = [AirPrinterNative]::PageBounds(
        [Drawing.SizeF]::new($imageWidth, $imageHeight),
        [Drawing.SizeF]::new(200, 300), [Drawing.SizeF]::new(220, 320), $mode, $fidelity)
    if ([Math]::Abs($bounds.Width - $expectedWidth) -gt 0.01 -or
        [Math]::Abs($bounds.Height - $expectedHeight) -gt 0.01 -or
        [Math]::Abs($bounds.X - (200 - $expectedWidth) / 2) -gt 0.01 -or
        [Math]::Abs($bounds.Y - (300 - $expectedHeight) / 2) -gt 0.01) { throw "Incorrect $mode bounds: $bounds" }
}
Check-Bounds 400 200 'fit' $false 200 100
Check-Bounds 400 200 'fill' $false 600 300
Check-Bounds 400 200 'none' $false 400 200
foreach ($mode in @('auto', 'auto-fit')) {
    Check-Bounds 400 200 $mode $false 200 100
    Check-Bounds 100 50 $mode $false 100 50
    Check-Bounds 100 50 $mode $true 200 100
}
'PASS: page selection and all advertised scaling modes; no printer used'

if ($Simulate) {
    # Explicit output paths permit Microsoft Print to PDF without a save dialog.
    # This never changes the default printer or uses a physical queue.
    $directory = Join-Path $PSScriptRoot '../src-tauri/target/print-options-simulation'
    $directory = [IO.Directory]::CreateDirectory($directory).FullName
    $pages = @()
    foreach ($color in @([Drawing.Color]::Red, [Drawing.Color]::Green, [Drawing.Color]::Blue)) {
        $image = [Drawing.Bitmap]::new(100, 100)
        $graphics = [Drawing.Graphics]::FromImage($image)
        try {
            $graphics.Clear($color)
            $path = Join-Path $directory ($color.Name + '.jpg')
            $image.Save($path, [Drawing.Imaging.ImageFormat]::Jpeg)
            $pages += $path
        } finally { $graphics.Dispose(); $image.Dispose() }
    }
    $allPdf = Join-Path $directory 'all-pages.pdf'
    $selectedPdf = Join-Path $directory 'selected-page.pdf'
    $job = @{
        printer = 'Microsoft Print to PDF'; format = 'image/jpeg'; pages = $pages
        name = 'AirPrinter automatic page selection test'; page_ranges = @(); scaling = 'fit'; fidelity = $false
        copies = 1; quality = 4; sides = 'one-sided'; color_mode = 'auto'; orientation = 3
        paper_kind = 9; paper_width = 827; paper_height = 1169
        directory = $directory; cancel = (Join-Path $directory 'cancel')
        spool_id = (Join-Path $directory 'spool-id'); output = $allPdf
    }
    $manifest = Join-Path $directory 'job.json'
    $savedManifest = $env:AIRPRINTER_MANIFEST
    try {
        $env:AIRPRINTER_MANIFEST = $manifest
        foreach ($selected in @($false, $true)) {
            if ($selected) {
                $job.format = 'application/pdf'; $job.input = $allPdf
                $job.page_ranges = ,@(2,2); $job.output = $selectedPdf; $job.scaling = 'none'
            }
            $json = ConvertTo-Json -InputObject $job -Depth 8
            [IO.File]::WriteAllText($manifest, $json, [Text.UTF8Encoding]::new($false))
            & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot '../src-tauri/src/services/windows_print.ps1')
            if ($LASTEXITCODE -ne 0) { throw 'Windows virtual print simulation failed' }
        }
    } finally { $env:AIRPRINTER_MANIFEST = $savedManifest }
    Add-Type -AssemblyName System.Runtime.WindowsRuntime
    $null = [Windows.Storage.StorageFile, Windows.Storage, ContentType=WindowsRuntime]
    $null = [Windows.Data.Pdf.PdfDocument, Windows.Data.Pdf, ContentType=WindowsRuntime]
    $await = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Await-Result' }, $true)
    . ([scriptblock]::Create($await.Extent.Text))
    foreach ($expected in @(@($allPdf,3), @($selectedPdf,1))) {
        $file = Await-Result ([Windows.Storage.StorageFile]::GetFileFromPathAsync($expected[0])) ([Windows.Storage.StorageFile])
        $pdf = Await-Result ([Windows.Data.Pdf.PdfDocument]::LoadFromFileAsync($file)) ([Windows.Data.Pdf.PdfDocument])
        if ($pdf.PageCount -ne $expected[1]) { throw "Incorrect output page count: $($pdf.PageCount)" }
    }
    'PASS: automatic Windows PDF output, then PDF page 2 selected and reprinted without dialogs'
}
