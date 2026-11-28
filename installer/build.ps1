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
   With the runtime (the default), andruna-app.exe is one of them, and the
   guest ART root of each architecture is checked and staged as art\ next
   to it (debug sections stripped, no boot image: the installer writes the
   boot image on the user's PC, ADR 0043). The ART root comes from
   -ArtRootX64 / -ArtRootArm64, ANDRUNA_ART_ROOT_X64 / ANDRUNA_ART_ROOT_ARM64
   or out\guest\<arch>\art-root. ANGLE's libEGL.dll and libGLESv2.dll (ADR
   0029) are staged in angle\ from -AngleDirX64 / -AngleDirArm64,
   ANDRUNA_ANGLE_DIR_X64 / ANDRUNA_ANGLE_DIR_ARM64 or out\angle\<arch>\bin
   and signed with the rest. -BuildRuntime builds a missing ART root
   (aosp/tools/build_guest.py) or ANGLE build (cargo xtask angle-build).
   A missing or incomplete one fails the build; -NoRuntime builds an
   installer without the runtime (manager, sysd and launcher only).
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
    [switch] $Unsigned,
    [switch] $NoRuntime,
    [switch] $BuildRuntime,
    [string] $ArtRootX64,
    [string] $ArtRootArm64,
    [string] $AngleDirX64,
    [string] $AngleDirArm64
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
if (-not $NoRuntime) { $RustBinaries += 'andruna-app.exe' }
# ELF e_machine of each architecture's guest code.
$ElfMachine = @{ x64 = 62; arm64 = 183 }
# What andruna-app needs in the ART root (crates/andruna-app/src/layout.rs:
# BOOT_JARS, libart, bionic, dalvikvm, dex2oat, Conscrypt's JNI library and
# the time zone data).
$ArtRequired = @('lib64\libart.so', 'lib64\libc.so', 'bin\dalvikvm', 'bin\dex2oat',
    'javalib\core-oj.jar', 'javalib\core-libart.jar', 'javalib\core-icu4j.jar', 'javalib\bouncycastle.jar',
    'javalib\conscrypt.jar', 'lib64\libjavacrypto.so', 'tzdata\etc\tz\tzdata')

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

function Get-ArtRootProblem([string] $Dir, [string] $A) {
    if (-not (Test-Path $Dir)) { return "$Dir does not exist" }
    $missing = @($ArtRequired | Where-Object { -not (Test-Path (Join-Path $Dir $_)) })
    if (-not (Get-ChildItem (Join-Path $Dir 'icu') -Filter 'icudt*.dat' -ErrorAction SilentlyContinue)) { $missing += 'icu\icudt*.dat' }
    if ($missing.Count -gt 0) { return "$Dir is incomplete; missing: $($missing -join ', ')" }
    # The guest code must be for this architecture.
    $head = [byte[]]::new(20)
    $fs = [IO.File]::OpenRead((Join-Path $Dir 'lib64\libart.so'))
    try { [void]$fs.Read($head, 0, 20) } finally { $fs.Dispose() }
    $machine = [BitConverter]::ToUInt16($head, 18)
    if ($head[0] -ne 0x7f -or $machine -ne $ElfMachine[$A]) {
        return "$Dir\lib64\libart.so is not an ELF file for $A (e_machine $machine)"
    }
    $null
}

# Builds the guest ART root of one architecture (crates/andruna-app/README.md):
# boot class path jars, then the guest userland and ART (arm64 is cross-built).
function Invoke-ArtRootBuild([string] $A) {
    Push-Location $Repo
    try {
        if (-not (Test-Path 'third_party\aosp\art\.git')) {
            & cargo xtask fetch; if ($LASTEXITCODE -ne 0) { throw 'cargo xtask fetch failed' }
        }
        & cargo xtask patch; if ($LASTEXITCODE -ne 0) { throw 'cargo xtask patch failed' }
        $jars = if ($env:ANDRUNA_BOOT_JARS) { $env:ANDRUNA_BOOT_JARS } else { 'out\s1\java' }
        if (-not (Test-Path (Join-Path $jars 'core-oj.jar'))) {
            & python spikes/s1/java/build_boot.py --skip-verify
            if ($LASTEXITCODE -ne 0) { throw 'spikes/s1/java/build_boot.py failed' }
        }
        $steps = @('sysroot', 'builtins', 'bionic', 'libcxx', 'aosp', 'art')
        & python aosp/tools/build_guest.py --arch $A @steps
        if ($LASTEXITCODE -ne 0) { throw "aosp/tools/build_guest.py --arch $A failed" }
    }
    finally { Pop-Location }
}

function Resolve-ArtRoot([string] $A) {
    $given = if ($A -eq 'x64') { $ArtRootX64 } else { $ArtRootArm64 }
    $envName = "ANDRUNA_ART_ROOT_$($A.ToUpperInvariant())"
    $fromEnv = [Environment]::GetEnvironmentVariable($envName)
    $dir = if ($given) { $given } elseif ($fromEnv) { $fromEnv } else { Join-Path $Repo "out\guest\$A\art-root" }
    $dir = [IO.Path]::GetFullPath($dir)
    $problem = Get-ArtRootProblem $dir $A
    if ($problem -and $BuildRuntime -and -not $given -and -not $fromEnv) {
        Write-Host "Building the ART root for $A ($problem)"
        Invoke-ArtRootBuild $A
        $problem = Get-ArtRootProblem $dir $A
    }
    if ($problem) {
        throw "No usable ART root for ${A}: $problem. Build it (python aosp/tools/build_guest.py --arch $A sysroot builtins bionic libcxx aosp art, see crates/andruna-app/README.md), pass -BuildRuntime, point -ArtRoot$(if ($A -eq 'x64') { 'X64' } else { 'Arm64' }) or $envName at one, or pass -NoRuntime for an installer without the runtime."
    }
    $dir
}

# Stages the ART root as <stage>\art: everything except boot images (the
# installer writes the boot image on the user's PC, ADR 0043), with the debug
# sections of the ELF files stripped (.symtab stays for crash reports).
function Copy-ArtRoot([string] $Src, [string] $Dst, [string] $LlvmMingw) {
    New-Item -ItemType Directory -Force $Dst | Out-Null
    foreach ($d in Get-ChildItem $Src -Directory) {
        if ($d.Name -eq 'javalib') {
            New-Item -ItemType Directory -Force (Join-Path $Dst 'javalib') | Out-Null
            Get-ChildItem $d.FullName -File | Copy-Item -Destination (Join-Path $Dst 'javalib')
        }
        else {
            Copy-Item -Recurse $d.FullName (Join-Path $Dst $d.Name)
        }
    }
    $strip = Join-Path $LlvmMingw 'bin\llvm-strip.exe'
    $elf = foreach ($f in Get-ChildItem (Join-Path $Dst 'lib64'), (Join-Path $Dst 'bin') -File) {
        $b = [byte[]]::new(4)
        $fs = [IO.File]::OpenRead($f.FullName)
        try { [void]$fs.Read($b, 0, 4) } finally { $fs.Dispose() }
        if ($b[0] -eq 0x7f -and $b[1] -eq 0x45 -and $b[2] -eq 0x4c -and $b[3] -eq 0x46) { $f.FullName }
    }
    & $strip --strip-debug @elf
    if ($LASTEXITCODE -ne 0) { throw "llvm-strip failed in $Dst" }
}

$AngleDlls = @('libEGL.dll', 'libGLESv2.dll')
$PeMachine = @{ x64 = 0x8664; arm64 = 0xAA64 }

function Get-AngleProblem([string] $Dir, [string] $A) {
    foreach ($d in $AngleDlls) {
        $p = Join-Path $Dir $d
        if (-not (Test-Path $p)) { return "$p does not exist" }
        $b = [IO.File]::ReadAllBytes($p)
        $pe = [BitConverter]::ToInt32($b, 0x3c)
        $m = [BitConverter]::ToUInt16($b, $pe + 4)
        if ($m -ne $PeMachine[$A]) { return "$p is not a $A DLL (machine 0x$('{0:X4}' -f $m))" }
    }
    $null
}

# ANGLE's DLLs (crates/andruna-gles/README.md: andruna-gles loads them from
# the angle folder next to the executable).
function Resolve-AngleDir([string] $A) {
    $given = if ($A -eq 'x64') { $AngleDirX64 } else { $AngleDirArm64 }
    $envName = "ANDRUNA_ANGLE_DIR_$($A.ToUpperInvariant())"
    $fromEnv = [Environment]::GetEnvironmentVariable($envName)
    $dir = if ($given) { $given } elseif ($fromEnv) { $fromEnv } else { Join-Path $Repo "out\angle\$A\bin" }
    $dir = [IO.Path]::GetFullPath($dir)
    $problem = Get-AngleProblem $dir $A
    if ($problem -and $BuildRuntime -and -not $given -and -not $fromEnv) {
        Write-Host "Building ANGLE for $A ($problem)"
        Invoke-Cargo @('xtask', 'angle-build', '--arch', $A)
        $problem = Get-AngleProblem $dir $A
    }
    if ($problem) {
        throw "No usable ANGLE build for ${A}: $problem. Build it with cargo xtask angle-build --arch $A, pass -BuildRuntime, point -AngleDir$(if ($A -eq 'x64') { 'X64' } else { 'Arm64' }) or $envName at one, or pass -NoRuntime."
    }
    $dir
}

function Get-DirSize([string] $Dir) {
    $s = (Get-ChildItem $Dir -Recurse -File | Measure-Object Length -Sum).Sum
    if ($s) { [int64]$s } else { [int64]0 }
}

function Format-MiB([int64] $Bytes) { '{0:N1} MiB' -f ($Bytes / 1MB) }

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

$artRoots = @{}
if (-not $NoRuntime) {
    # Checked first: a missing ART root fails before anything is built.
    foreach ($a in $Arch) { $artRoots[$a] = Resolve-ArtRoot $a }
    $angleDirs = @{}
    foreach ($a in $Arch) { $angleDirs[$a] = Resolve-AngleDir $a }
}
$sizes = @()

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

    if (-not $NoRuntime) {
        $src = $artRoots[$a]
        Write-Host "Staging the ART root ($a): $src"
        $before = Get-DirSize $src
        Copy-ArtRoot $src (Join-Path $stage 'art') $llvmMingw
        $after = Get-DirSize (Join-Path $stage 'art')
        Write-Host "  ART root ($a): $(Format-MiB $before) as built (with boot image if any), $(Format-MiB $after) staged"
        $angleStage = Join-Path $stage 'angle'
        New-Item -ItemType Directory -Force $angleStage | Out-Null
        foreach ($d in $AngleDlls) { Copy-Item (Join-Path $angleDirs[$a] $d) $angleStage }
        # Ours: signed below when they are not yet, and checked with the rest.
        if (-not $Unsigned) { foreach ($d in $AngleDlls) { $signedOwn += Join-Path $angleStage $d } }
        Write-Host "  ANGLE ($a): $($angleDirs[$a])"
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
    $sizes += [pscustomobject]@{
        Arch = $a
        Staged = Format-MiB (Get-DirSize $stage)
        Runtime = if ($NoRuntime) { '-' } else { Format-MiB (Get-DirSize (Join-Path $stage 'art')) }
    }
}
if (-not $NoRuntime) { $defines += '!define RUNTIME' }

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

$sizes | Format-Table -AutoSize | Out-String | Write-Host
Write-Host "Installer: $installer ($(Format-MiB (Get-Item $installer).Length))"
