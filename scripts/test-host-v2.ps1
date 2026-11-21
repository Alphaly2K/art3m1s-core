$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $installation) { throw 'MSVC x64 tools are required' }
    & (Join-Path $installation 'Common7\Tools\Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
}

$buildDir = $env:ART3M1S_V2_TARGET_DIR
if (-not $buildDir) { $buildDir = 'target/host-v2' }
& cargo build --locked --lib --manifest-path crates/artemis-core/Cargo.toml --target-dir $buildDir @args
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$libDir = (Resolve-Path (Join-Path $buildDir 'debug')).Path
$library = Join-Path $libDir 'art3m1s_core.dll'
$exportTable = & dumpbin.exe /NOLOGO /EXPORTS $library
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$exports = @($exportTable | ForEach-Object {
    if ($_ -match '^\s*\d+\s+[0-9A-Fa-f]+\s+[0-9A-Fa-f]+\s+(\S+)') { $Matches[1] }
})
if ($exports.Count -ne 1 -or $exports[0] -cne 'art3m1s_get_api_v2') {
    throw "Unexpected v2 exports: $($exports -join ', ')"
}

$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("artemis-v2-" + [Guid]::NewGuid())
$originalPath = $env:PATH
New-Item -Type Directory $scratch | Out-Null
try {
    $env:PATH = "$libDir;$originalPath"
    $headerCheck = Join-Path $scratch 'header.cpp'
    Set-Content -LiteralPath $headerCheck '#include "art3m1s_core_v2.draft.h"' -Encoding ascii
    & cl.exe /nologo /std:c++17 /W4 /WX /Idoc/abi /Zs $headerCheck
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $import = Join-Path $libDir 'art3m1s_core.dll.lib'
    $probe = Join-Path $scratch 'artemis-v2.exe'
    $object = Join-Path $scratch 'artemis-v2.obj'
    & cl.exe /nologo /std:c11 /W4 /WX /D_CRT_SECURE_NO_WARNINGS /DARTEMIS_THREAD_SMOKE /Idoc/abi "/Fo$object" "/Fe$probe" tools/host-smoke/artemis_v2.c $import
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & $probe
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    $env:PATH = $originalPath
    Remove-Item -LiteralPath $scratch -Recurse -Force
}
