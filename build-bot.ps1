param(
    [Parameter(Mandatory = $true)][string]$VcpkgRoot,
    [switch]$InstallDependencies,
    [switch]$Test
)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$vcpkg = Join-Path (Resolve-Path -LiteralPath $VcpkgRoot).Path 'vcpkg.exe'
if (-not (Test-Path -LiteralPath $vcpkg)) { throw "vcpkg.exe not found: $vcpkg" }
if ($InstallDependencies) {
    & $vcpkg install x264:x64-windows freetype:x64-windows pkgconf:x64-windows
    if ($LASTEXITCODE -ne 0) { throw 'vcpkg native dependency installation failed' }
}
$installed = Join-Path (Split-Path -Parent $vcpkg) 'installed/x64-windows'
if (-not (Test-Path -LiteralPath (Join-Path $installed 'include/x264.h'))) {
    throw 'Install x264:x64-windows first, or pass -InstallDependencies'
}
$env:X264_INCLUDE_DIR = Join-Path $installed 'include'
$env:X264_LIB_DIR = Join-Path $installed 'lib'
$env:X264_LIB_NAME = 'libx264'
$pkgconf = Join-Path $installed 'tools/pkgconf/pkgconf.exe'
if (-not (Test-Path -LiteralPath $pkgconf)) {
    throw 'Install freetype:x64-windows and pkgconf:x64-windows, or pass -InstallDependencies'
}
# freetype-sys 0.23 bundled build cannot locate its libpng/zlib headers.
# Use the vcpkg FreeType package and pkgconf rather than patching registry files.
$env:PKG_CONFIG = $pkgconf
$env:PKG_CONFIG_PATH = (Join-Path $installed 'lib/pkgconfig') + [IO.Path]::PathSeparator + (Join-Path $installed 'share/pkgconfig')
$env:VCPKG_ROOT = Split-Path -Parent $vcpkg
$env:VCPKGRS_TRIPLET = 'x64-windows'
$env:VCPKGRS_DYNAMIC = '1'
$env:PATH = (Join-Path $installed 'bin') + [IO.Path]::PathSeparator + $env:PATH
Push-Location $root
try {
    & cargo build --locked --release -p sekai-profile-cli --features "animation-export,sekai-profile/parallel"
    if ($LASTEXITCODE -ne 0) { throw 'Renderer build failed' }
    if ($Test) {
        & cargo test --locked -p sekai-profile-cli --features animation-export
        if ($LASTEXITCODE -ne 0) { throw 'Renderer tests failed' }
    }
    $dest = Join-Path $root 'target/release'
    Get-ChildItem -LiteralPath (Join-Path $installed 'bin') -Filter '*.dll' |
        Copy-Item -Destination $dest -Force
    Write-Host "Built $dest/sekai-profile.exe (keep the DLLs beside it)"
} finally {
    Pop-Location
}
