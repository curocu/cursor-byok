param(
    [string]$BuildRoot = 'D:\devtools\cursor-byok',
    [string]$CMakeBin = 'D:\devtools\cmake-3.31.12-windows-x86_64\bin'
)
$ErrorActionPreference = 'Stop'
$repository = Split-Path $PSScriptRoot -Parent
$env:CARGO_HOME = Join-Path $BuildRoot 'cache\cargo'
$env:CARGO_TARGET_DIR = Join-Path $BuildRoot 'target'
$env:npm_config_cache = Join-Path $BuildRoot 'cache\npm'
$env:TEMP = Join-Path $BuildRoot 'tmp'
$env:TMP = $env:TEMP
$env:PATH = "$CMakeBin;$env:PATH"
@($env:CARGO_HOME, $env:CARGO_TARGET_DIR, $env:npm_config_cache, $env:TEMP) | ForEach-Object {
    New-Item -ItemType Directory -Force -Path $_ | Out-Null
}
function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed: exit $LASTEXITCODE" }
}
Push-Location $repository
try {
    Invoke-Checked cargo @('test', '-p', 'cursor-server', '--lib', 'local_app::proxy::tests', '--locked')
    Invoke-Checked cargo @('test', '-p', 'cursor-server', '--lib', 'cursor::inference', '--locked')
    Invoke-Checked cargo @('test', '-p', 'cursor-server', '--test', 'inference', '--test', 'inference_providers', '--test', 'connect_wire', '--test', 'cursor_transport_lifecycle', '--locked')
    Push-Location (Join-Path $repository 'apps\desktop')
    try {
        Invoke-Checked npm @('ci')
        Invoke-Checked npm @('run', 'check')
        Invoke-Checked npm @('run', 'tauri:build', '--', '--config', 'src-tauri/tauri.local.conf.json', '--bundles', 'nsis')
    } finally { Pop-Location }
    Write-Host "EXE: $env:CARGO_TARGET_DIR\release\cursor-byok-desktop.exe"
    Write-Host "Installer: $env:CARGO_TARGET_DIR\release\bundle\nsis"
} finally { Pop-Location }
