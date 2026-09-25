param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$projectDir = Split-Path -Parent $PSScriptRoot
Push-Location $projectDir
try {
    if (-not $SkipBuild) {
        cargo build --locked --release -p easy-switch-link --target x86_64-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
    }
    $outputDir = Join-Path $projectDir 'dist'
    New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
    $exePath = Join-Path $projectDir 'target/x86_64-pc-windows-msvc/release/easy-switch-link.exe'
    Copy-Item -LiteralPath $exePath -Destination (Join-Path $outputDir 'easy-switch-link-0.2.1-windows-x64.exe')
    wix build (Join-Path $projectDir 'packaging/windows/main.wxs') -arch x64 -d "ProjectDir=$projectDir" -d "ExePath=$exePath" -o (Join-Path $outputDir 'easy-switch-link-0.2.1-windows-x64.msi')
    if ($LASTEXITCODE -ne 0) { throw 'WiX build failed' }
    Get-ChildItem -LiteralPath $outputDir -File | Where-Object { $_.Extension -in '.exe','.msi' } | ForEach-Object {
        $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $($_.Name)"
    } | Set-Content -LiteralPath (Join-Path $outputDir 'SHA256SUMS-windows.txt') -Encoding ascii
} finally { Pop-Location }
