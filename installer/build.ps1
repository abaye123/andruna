<#
.SYNOPSIS
Builds the Andruna installer (ADR 0017).

.DESCRIPTION
1. Builds the Rust release executables (andruna-sysd.exe, andruna-launch.exe)
   for each architecture with `cargo xtask build --arch <arch> --release` and
   signs them with `cargo xtask sign`. -SkipRustBuild uses the existing
   target\<triple>\release\ files, -RustBinDir another folder.
2. Checks that the Rust executables import no llvm-mingw runtime DLL
   (libc++, libunwind, libwinpthread, ...), which the installer does not carry.
3. Publishes Andruna.Manager self-contained for each architecture and stages
   the Rust executables next to it.
4. Signs every .exe and .dll in the staged files that has no signature yet
   (our own binaries; files already signed by their vendor keep that signature).
5. Builds one installer with NSIS that installs the files matching the PC's
   processor, and signs the installer and its uninstaller.
6. Checks the signatures: signer thumbprint and timestamp.

Signing uses the certificate whose thumbprint is in ANDRUNA_SIGN_THUMBPRINT,
from the current user's or the machine's certificate store. -Unsigned builds
an unsigned installer for local development and CI.

NSIS is taken from ANDRUNA_NSIS (a folder containing makensis.exe) or
downloaded once into .cache\nsis-<version> and checked against its pinned
SHA-256.

.EXAMPLE
pwsh installer\build.ps1
pwsh installer\build.ps1 -Arch x64 -Unsigned
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string[]] $Arch = @('x64', 'arm64'),
    [string] $Configuration = 'Release',
    [string] $OutDir,
    [string] $RustBinDir,
    [switch] $SkipRustBuild,
    [switch] $Unsigned
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $Repo 'out\installer' }

# Pinned NSIS release (zlib/libpng license; see ADR 0017).
$NsisVersion = '3.11'
$NsisUrl = "https://downloads.sourceforge.net/project/nsis/NSIS%203/$NsisVersion/nsis-$NsisVersion.zip"
$NsisSha256 = 'c7d27f780ddb6cffb4730138cd1591e841f4b7edb155856901cdf5f214394fa1'

$TimestampUrl = 'http://timestamp.digicert.com'
$RustTriples = @{ x64 = 'x86_64-pc-windows-gnullvm'; arm64 = 'aarch64-pc-windows-gnullvm' }
# Rust executables the installer carries; the build fails when one is missing.
$RustBinaries = @('andruna-sysd.exe', 'andruna-launch.exe')

function Invoke-Cargo([string[]] $CargoArgs) {
    Push-Location $Repo
    try {
        & cargo @CargoArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArgs -join ' ') failed" }
    }
    finally { Pop-Location }
}

function Get-LlvmMingw {
    $dir = if ($env:ANDRUNA_LLVM_MINGW) { $env:ANDRUNA_LLVM_MINGW } else { 'C:\tools\llvm-mingw' }
    if (-not (Test-Path (Join-Path $dir 'bin\llvm-objdump.exe'))) {
        throw "llvm-mingw not found at $dir (run cargo xtask toolchain or set ANDRUNA_LLVM_MINGW)"
    }
    $dir
}

# The installer carries no llvm-mingw runtime DLL, so the Rust executables must
# not import one: every DLL that llvm-mingw ships for any target is refused.
function Test-NoToolchainDlls([string] $LlvmMingw, [string[]] $Files) {
    $objdump = Join-Path $LlvmMingw 'bin\llvm-objdump.exe'
    $forbidden = Get-ChildItem $LlvmMingw -Recurse -Filter *.dll | ForEach-Object { $_.Name.ToLowerInvariant() } | Sort-Object -Unique
    foreach ($f in $Files) {
        $imports = & $objdump -p $f | Select-String 'DLL Name:\s*(\S+)' | ForEach-Object { $_.Matches[0].Groups[1].Value }
        if ($LASTEXITCODE -ne 0) { throw "llvm-objdump failed on $f" }
        $bad = @($imports | Where-Object { $forbidden -contains $_.ToLowerInvariant() })
        if ($bad.Count -gt 0) { throw "$f imports llvm-mingw runtime DLLs: $($bad -join ', ')" }
        Write-Host "  imports of $(Split-Path -Leaf $f): $((@($imports) | Sort-Object -Unique) -join ', ')"
    }
}

function Get-Version {
    $props = [xml](Get-Content (Join-Path $Repo 'ui\Directory.Build.props') -Raw)
    $v = $props.Project.PropertyGroup.Version | Select-Object -First 1
    if ($v -notmatch '^\d+\.\d+\.\d+$') { throw "ui\Directory.Build.props has no a.b.c Version (found '$v')" }
    $v
}

function Get-Makensis {
    if ($env:ANDRUNA_NSIS) {
        $exe = Join-Path $env:ANDRUNA_NSIS 'makensis.exe'
        if (-not (Test-Path $exe)) { throw "ANDRUNA_NSIS is set but $exe does not exist" }
        return $exe
    }

    $dir = Join-Path $Repo ".cache\nsis-$NsisVersion"
    $exe = Join-Path $dir "nsis-$NsisVersion\makensis.exe"
    if (Test-Path $exe) { return $exe }

    New-Item -ItemType Directory -Force $dir | Out-Null
    $zip = Join-Path $dir "nsis-$NsisVersion.zip"
    Write-Host "Downloading NSIS $NsisVersion"
    & curl.exe -sSfL -o $zip $NsisUrl
    if ($LASTEXITCODE -ne 0) { throw "downloading $NsisUrl failed" }
    $hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
    if ($hash -ne $NsisSha256) {
        Remove-Item $zip
        throw "NSIS archive SHA-256 is $hash, expected $NsisSha256"
    }
    & tar.exe -xf $zip -C $dir
    if ($LASTEXITCODE -ne 0) { throw "extracting $zip failed" }
    $exe
}

function Get-SignTool {
    if ($env:ANDRUNA_SIGNTOOL) { return $env:ANDRUNA_SIGNTOOL }
    $kits = 'C:\Program Files (x86)\Windows Kits\10\bin'
    $hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    $found = Get-ChildItem $kits -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^10\.' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "$hostArch\signtool.exe" } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1
    if (-not $found) { throw 'signtool.exe not found; install the Windows SDK or set ANDRUNA_SIGNTOOL' }
    $found
}

function Invoke-Sign([string] $SignTool, [string] $Thumbprint, [string[]] $Files) {
    if (-not $Files) { return }
    # signtool accepts many files per call; batches keep the command line short.
    for ($i = 0; $i -lt $Files.Count; $i += 50) {
        $batch = $Files[$i..([Math]::Min($i + 49, $Files.Count - 1))]
        & $SignTool sign /sha1 $Thumbprint /fd sha256 /tr $TimestampUrl /td sha256 /q @batch
        if ($LASTEXITCODE -ne 0) { throw "signtool failed on: $($batch -join ', ')" }
    }
}

function Test-Signature([string] $File, [string] $Thumbprint) {
    $sig = Get-AuthenticodeSignature $File
    if (-not $sig.SignerCertificate) { throw "$File is not signed" }
    if ($sig.SignerCertificate.Thumbprint -ne $Thumbprint) {
        throw "$File is signed by $($sig.SignerCertificate.Thumbprint), expected $Thumbprint"
    }
    if (-not $sig.TimeStamperCertificate) { throw "$File has no timestamp" }
    [pscustomobject]@{
        File = Split-Path -Leaf $File
        Status = $sig.Status
        Signer = $sig.SignerCertificate.Thumbprint
        Timestamp = $sig.TimeStamperCertificate.Subject
    }
}

$version = Get-Version
$thumbprint = $null
$signTool = $null
if (-not $Unsigned) {
    $thumbprint = $env:ANDRUNA_SIGN_THUMBPRINT
    if (-not $thumbprint) { throw 'Set ANDRUNA_SIGN_THUMBPRINT to the code signing certificate thumbprint, or pass -Unsigned for a local development build.' }
    $thumbprint = $thumbprint.ToUpperInvariant()
    $signTool = Get-SignTool
}

$stageRoot = Join-Path $OutDir 'stage'
if (Test-Path $stageRoot) { Remove-Item -Recurse -Force $stageRoot }
New-Item -ItemType Directory -Force $stageRoot | Out-Null
$project = Join-Path $Repo 'ui\src\Andruna.Manager\Andruna.Manager.csproj'
$defines = @("!define VERSION `"$version`"")
$signedOwn = @()
$llvmMingw = Get-LlvmMingw

foreach ($a in $Arch) {
    $stage = Join-Path $stageRoot $a

    if (-not $RustBinDir -and -not $SkipRustBuild) {
        Write-Host "Building the Rust executables for $a"
        Invoke-Cargo @('xtask', 'build', '--arch', $a, '--release')
        if (-not $Unsigned) {
            # xtask reads the thumbprint from ANDRUNA_SIGN_THUMBPRINT, as this script does.
            Invoke-Cargo @('xtask', 'sign', '--arch', $a, '--release')
        }
    }

    $binDir = if ($RustBinDir) { Join-Path $RustBinDir $a } else { Join-Path $Repo "target\$($RustTriples[$a])\release" }
    $rustFiles = foreach ($name in $RustBinaries) {
        $src = Join-Path $binDir $name
        if (-not (Test-Path $src)) { throw "$src does not exist; build it with cargo xtask build --arch $a --release" }
        $src
    }
    Write-Host "Checking the imports of the Rust executables ($a)"
    Test-NoToolchainDlls $llvmMingw @($rustFiles)

    Write-Host "Publishing Andruna.Manager for $a"
    & dotnet publish $project -c $Configuration -r "win-$a" --self-contained -o $stage -nologo -v q
    if ($LASTEXITCODE -ne 0) { throw "dotnet publish for $a failed" }
    Get-ChildItem $stage -Filter *.pdb -Recurse | Remove-Item

    foreach ($src in $rustFiles) {
        Copy-Item $src $stage
        Write-Host "  $(Split-Path -Leaf $src) ($a): $src"
        if (-not $Unsigned -and (Get-AuthenticodeSignature $src).Status -ne 'NotSigned') {
            # Signed by cargo xtask sign; checked with the rest below.
            $signedOwn += Join-Path $stage (Split-Path -Leaf $src)
        }
    }

    if (-not $Unsigned) {
        $toSign = Get-ChildItem $stage -Recurse -Include *.exe, *.dll |
            Where-Object { (Get-AuthenticodeSignature $_.FullName).Status -eq 'NotSigned' } |
            ForEach-Object FullName
        Write-Host "  signing $(@($toSign).Count) unsigned binaries ($a)"
        Invoke-Sign $signTool $thumbprint @($toSign)
        $signedOwn += @($toSign)
    }

    $defines += "!define STAGE_$($a.ToUpperInvariant()) `"$stage`""
}

New-Item -ItemType Directory -Force $OutDir | Out-Null
$archTag = ($Arch | Sort-Object) -join '-'
$installer = Join-Path $OutDir "Andruna-$version-setup-$archTag.exe"
$defines += "!define OUTFILE `"$installer`""
$defines += "!define ICON `"$(Join-Path $Repo 'ui\src\Andruna.Manager\Assets\Andruna.ico')`""
if (-not $Unsigned) {
    $defines += "!define SIGN_CMD '`"$signTool`" sign /sha1 $thumbprint /fd sha256 /tr $TimestampUrl /td sha256 /q `"%1`"'"
}
$definesFile = Join-Path $OutDir 'defines.nsh'
[IO.File]::WriteAllLines($definesFile, $defines, [Text.UTF8Encoding]::new($true))

$makensis = Get-Makensis
Write-Host "Building $installer"
& $makensis /V2 /INPUTCHARSET UTF8 "/DDEFINES=$definesFile" (Join-Path $PSScriptRoot 'andruna.nsi')
if ($LASTEXITCODE -ne 0) { throw 'makensis failed' }

if (-not $Unsigned) {
    Write-Host 'Checking signatures (a self-signed development certificate reports UnknownError; signer and timestamp are what is checked)'
    $checked = @($installer) + $signedOwn | ForEach-Object { Test-Signature $_ $thumbprint }
    $checked | Sort-Object File -Unique | Format-Table -AutoSize | Out-String | Write-Host
}

Write-Host "Installer: $installer"
