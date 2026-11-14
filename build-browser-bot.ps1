# BROWSER-01: Prepare the browser SDK separately from the native BOT executable.
# This script is opt-in; build-bot.ps1 retains its original behavior.
[CmdletBinding(DefaultParameterSetName = 'Source')]
param(
    [Parameter(ParameterSetName = 'Archive', Mandatory = $true)]
    [string]$SdkArchive,
    [Parameter(ParameterSetName = 'Directory', Mandatory = $true)]
    [string]$SdkDirectory,
    [Parameter(ParameterSetName = 'Source')]
    [switch]$FromSource
)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$package = Join-Path $root 'crates/sekai-profile-wasm'
$destination = Join-Path $root 'runtime/browser-sdk'
$stage = Join-Path $root ('runtime/browser-sdk-stage-' + [guid]::NewGuid().ToString('N'))

function Find-SdkDist([string]$Base) {
    foreach ($candidate in @((Join-Path $Base 'dist'), (Join-Path $Base 'package/dist'), $Base)) {
        if (Test-Path -LiteralPath (Join-Path $candidate 'index.js')) {
            return $candidate
        }
    }
    throw "No browser dist/index.js found in $Base"
}

function Test-SdkDist([string]$Dist) {
    foreach ($name in @('index.js', 'renderer.js', 'worker.js', 'worker-client.js',
                        'sekai_profile_wasm.js', 'sekai_profile_wasm.wasm',
                        'third-party/freetype/FTL.txt')) {
        if (-not (Test-Path -LiteralPath (Join-Path $Dist $name) -PathType Leaf)) {
            throw "Incomplete browser SDK: missing $name in $Dist"
        }
    }
}

if ($PSCmdlet.ParameterSetName -eq 'Archive') {
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Expand-Archive -LiteralPath (Resolve-Path -LiteralPath $SdkArchive).Path -DestinationPath $stage
    $dist = Find-SdkDist $stage
} elseif ($PSCmdlet.ParameterSetName -eq 'Directory') {
    $dist = Find-SdkDist (Resolve-Path -LiteralPath $SdkDirectory).Path
} else {
    # The existing Dockerfile builds the matching Rust/WASM core and FreeType.
    # TypeScript compilation runs on the Windows host with Node/npm.
    Get-Command docker -ErrorAction Stop | Out-Null
    Get-Command npm.cmd -ErrorAction Stop | Out-Null
    & docker build -f (Join-Path $package 'Dockerfile') -t allium-bot-browser:local $root
    if ($LASTEXITCODE -ne 0) { throw 'Browser WASM Docker build failed' }
    $container = & docker create allium-bot-browser:local
    if ($LASTEXITCODE -ne 0) { throw 'Could not create browser build container' }
    $container = $container.Trim()
    try {
        $generated = Join-Path $package 'dist'
        New-Item -ItemType Directory -Force -Path $generated | Out-Null
        & docker cp ($container + ':/artifacts/.') $generated
        if ($LASTEXITCODE -ne 0) { throw 'Could not copy browser WASM artifacts' }
    } finally {
        & docker rm $container | Out-Null
    }
    Push-Location $package
    try {
        & npm.cmd ci
        if ($LASTEXITCODE -ne 0) { throw 'Browser TypeScript dependency installation failed' }
        & npm.cmd run build:ts
        if ($LASTEXITCODE -ne 0) { throw 'Browser TypeScript build failed' }
    } finally {
        Pop-Location
    }
    $dist = Join-Path $package 'dist'
}
Test-SdkDist $dist
$target = Join-Path $destination 'dist'
New-Item -ItemType Directory -Force -Path $target | Out-Null
if ([IO.Path]::GetFullPath($dist) -ne [IO.Path]::GetFullPath($target)) {
    Get-ChildItem -LiteralPath $dist -Force | Copy-Item -Destination $target -Recurse -Force
}
Test-SdkDist $target
if (Test-Path -LiteralPath $stage) {
    $resolvedStage = (Resolve-Path -LiteralPath $stage).Path
    $runtimeRoot = [IO.Path]::GetFullPath((Join-Path $root 'runtime'))
    if ((Split-Path -Parent $resolvedStage) -ne $runtimeRoot) {
        throw "Refusing to remove a staging directory outside $runtimeRoot"
    }
    Remove-Item -LiteralPath $resolvedStage -Recurse -Force
}
Write-Host "Browser SDK ready: $target"
Write-Host 'Deploy bot_service.py, bot_browser.py, browser_bot/ and runtime/browser-sdk/.'
Write-Host 'On the deployment machine: python -m pip install aiohttp playwright'
Write-Host 'Then: python -m playwright install chromium --no-shell'
Write-Host 'Set backend to browser in bot_service.json and restart the service.'
