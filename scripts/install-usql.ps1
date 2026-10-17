# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.
param(
    [string]$BinDir = (Join-Path $env:LOCALAPPDATA 'UQA\bin'),
    [string]$ArchiveDirectory = ''
)
$ErrorActionPreference = 'Stop'
$Version = '@UQA_VERSION@'
if ($Version.StartsWith('@')) { throw 'Use the installer attached to a release.' }
if ([string]::IsNullOrWhiteSpace($BinDir)) { throw 'Installation directory is empty.' }
if (Test-Path -LiteralPath (Join-Path $BinDir 'usql.exe') -PathType Container) { throw 'The usql destination is a directory.' }
if ($env:OS -ne 'Windows_NT') { throw 'Use install-usql.sh on Linux or macOS.' }
$Architecture = $env:PROCESSOR_ARCHITEW6432
if (!$Architecture) { $Architecture = $env:PROCESSOR_ARCHITECTURE }
$Target = switch ($Architecture) {
    'AMD64' { 'x86_64-pc-windows-msvc' }
    'ARM64' { 'aarch64-pc-windows-msvc' }
    default { throw "Unsupported architecture: $Architecture" }
}
$Asset = "uqa-cli-$Version-$Target.zip"
$TempDir = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
$Staged = $null
New-Item -ItemType Directory -Path $TempDir | Out-Null
try {
    foreach ($Name in @($Asset, "$Asset.sha256")) {
        $Destination = Join-Path $TempDir $Name
        if ($ArchiveDirectory) {
            Copy-Item -LiteralPath (Join-Path $ArchiveDirectory $Name) -Destination $Destination
        } else {
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
            Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/cognica-io/uqa-engine/releases/download/v$Version/$Name" -OutFile $Destination
        }
    }
    $Archive = Join-Path $TempDir $Asset
    $Checksum = (Get-Content -LiteralPath "$Archive.sha256" -Raw).Trim()
    if ($Checksum -cnotmatch ('^([0-9a-f]{64})  ' + [regex]::Escape($Asset) + '$')) {
        throw 'Invalid SHA-256 checksum file.'
    }
    if ((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant() -cne $Matches[1]) {
        throw 'SHA-256 checksum mismatch.'
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $Zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        $Entry = $Zip.GetEntry('usql.exe')
        if (!$Entry) { throw 'Archive has no usql.exe executable.' }
        New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
        $Staged = Join-Path $BinDir ([IO.Path]::GetRandomFileName())
        [IO.Compression.ZipFileExtensions]::ExtractToFile($Entry, $Staged)
    } finally { $Zip.Dispose() }
    Move-Item -LiteralPath $Staged -Destination (Join-Path $BinDir 'usql.exe') -Force
    $Staged = $null
    Write-Output "Installed usql $Version to $BinDir\usql.exe"
    if (($env:PATH -split ';') -notcontains $BinDir) { Write-Output "Add $BinDir to PATH to run usql." }
} finally {
    if ($Staged -and (Test-Path -LiteralPath $Staged)) { Remove-Item -LiteralPath $Staged -Force }
    Remove-Item -LiteralPath $TempDir -Recurse -Force
}
