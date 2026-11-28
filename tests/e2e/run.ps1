<#
.SYNOPSIS
End-to-end test of the product path on this PC: the signed installer, sysd,
the package manager, the runtime, the manager, uninstall.

.DESCRIPTION
Runs with Windows PowerShell 5.1 on an x64 or arm64 PC, as the user (no
administrator rights). Steps, each timed and recorded:

 1. Backs up what the product touches in the user's profile: the
    "Installed apps" entry of Andruna, the Start menu shortcut, the package
    file types, %LOCALAPPDATA%\Andruna; and snapshots the Uninstall keys,
    the Start menu and the notification registrations.
 2. Installs silently into a separate folder (-InstallDir, default
    %LOCALAPPDATA%\AndrunaE2E\Programs\Andruna), never the user's install.
 3. Checks the install: files, signatures (signer thumbprint and
    timestamp), the boot image written at install, the andrunaRuntime ACE.
 4. Starts the installed andruna-sysd and talks to it over its pipe
    (SysdPipe.psm1): installs the test's own package (fixture\e2e.apk), the
    package manager's real fixture APK and Binary Eye from F-Droid (pinned
    SHA-256); checks Start menu shortcuts, "Installed apps" entries and the
    dex2oat output.
 5. Runs org.andruna.e2e.Main from the installed package with the
    installed andruna-app (JIT and AOT), and starts the package through
    andruna-launch (StartPackage).
 6. Screenshots of the manager (library and app page, English and Hebrew)
    into -ScreenshotDir. The installed manager's window is captured with
    PrintWindow; on a locked session (where that gives a blank image) pass
    -ManagerSnapshotExe, a Debug build of the manager (ui/README.md,
    "Screenshots"), which renders its own window against the installed
    sysd and real data.
 7. Uninstalls the packages through sysd and checks that shortcuts,
    entries and files are gone; installs one again and leaves it for the
    Andruna uninstaller.
 8. Uninstalls Andruna silently and checks the cleanup, including the
    Android app's shortcut and entry.
 9. Restores the backup and compares the profile with the snapshot of
    step 1. This runs even when a step failed.

No Andruna process may be running when it starts (-StopRunning ends the
ones of this user). Results: <OutDir>\results.json and results.md.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File tests\e2e\run.ps1
powershell -ExecutionPolicy Bypass -File tests\e2e\run.ps1 -Installer out\installer\Andruna-0.1.0-setup-arm64-x64.exe
#>
[CmdletBinding()]
param(
    [string] $Installer,
    [string] $InstallDir = (Join-Path $env:LOCALAPPDATA 'AndrunaE2E\Programs\Andruna'),
    [string] $OutDir,
    [string] $ScreenshotDir,
    [string] $Thumbprint = $env:ANDRUNA_SIGN_THUMBPRINT,
    [string] $ManagerSnapshotExe,
    [switch] $StopRunning,
    [switch] $SkipNetwork
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $OutDir) { $OutDir = Join-Path $Repo 'out\e2e' }
if (-not $ScreenshotDir) { $ScreenshotDir = Join-Path $OutDir 'screenshots' }
if (-not $Installer) {
    $Installer = Get-ChildItem (Join-Path $Repo 'out\installer') -Filter 'Andruna-*-setup-*.exe' |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
    if (-not $Installer) { throw 'no installer in out\installer; build it with installer\build.ps1' }
}
$Installer = [IO.Path]::GetFullPath($Installer)
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
Import-Module (Join-Path $PSScriptRoot 'SysdPipe.psm1') -Force

$DataRoot = Join-Path $env:LOCALAPPDATA 'Andruna'
$Programs = [Environment]::GetFolderPath('Programs')
$UninstallRoot = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
$ClassKeys = @('.apk', '.apks', '.xapk', '.apkm', 'Andruna.Package', 'Applications\Andruna.Manager.exe')
$Isa = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x86_64' }
$ProcessNames = @('Andruna.Manager', 'andruna-sysd', 'andruna-launch', 'andruna-app')
$BinaryEye = @{
    File = 'de.markusfisch.android.binaryeye_184.apk'
    Sha256 = '6995b9a4242ea54141ac0a31a8b933a764c84d64e45d6082a33d7d808fdabedb'
    Urls = @('https://f-droid.org/repo/', 'https://ftp.lysator.liu.se/pub/fdroid/repo/',
        'https://f-droid.org/archive/', 'https://ftp.lysator.liu.se/pub/fdroid/archive/')
}

New-Item -ItemType Directory -Force $OutDir, $ScreenshotDir | Out-Null
$Backup = Join-Path $OutDir 'backup'
$Work = Join-Path $OutDir 'work'
foreach ($d in $Backup, $Work) {
    if (Test-Path $d) { Remove-Item -Recurse -Force $d }
    New-Item -ItemType Directory -Force $d | Out-Null
}

$results = [ordered]@{
    started = (Get-Date).ToString('s')
    machine = "$env:COMPUTERNAME, $env:PROCESSOR_ARCHITECTURE, $((Get-CimInstance Win32_OperatingSystem).Caption) $([Environment]::OSVersion.Version)"
    cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
    installer = $Installer
    installer_mib = [math]::Round((Get-Item $Installer).Length / 1MB, 1)
    install_dir = $InstallDir
    steps = [Collections.Generic.List[object]]::new()
}
$failures = [Collections.Generic.List[string]]::new()

function Add-Step([string] $Name, [bool] $Ok, [string] $Detail, [double] $Ms = -1) {
    $results.steps.Add([ordered]@{ step = $Name; ok = $Ok; ms = [math]::Round($Ms, 1); detail = $Detail })
    $mark = if ($Ok) { 'ok  ' } else { 'FAIL' }
    $t = if ($Ms -ge 0) { " ($([math]::Round($Ms)) ms)" } else { '' }
    Write-Host "[$mark] $Name$t - $Detail"
    if (-not $Ok) { $failures.Add($Name) }
}

function Test-Step([string] $Name, [scriptblock] $Body) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    try {
        $d = & $Body
        Add-Step $Name $true ([string]($d -join '; ')) $sw.Elapsed.TotalMilliseconds
    }
    catch {
        Add-Step $Name $false $_.Exception.Message $sw.Elapsed.TotalMilliseconds
    }
}

# Runs a native program without turning its stderr into PowerShell errors;
# returns its exit code and output lines.
function Invoke-Native([string] $Exe, [string[]] $ArgList) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $out = & $Exe @ArgList 2>&1 | ForEach-Object { "$_" }
        [pscustomobject]@{ Code = $LASTEXITCODE; Output = @($out) }
    }
    finally { $ErrorActionPreference = $old }
}

function Get-AndrunaProcesses {
    Get-Process -Name $ProcessNames -ErrorAction SilentlyContinue
}

function Stop-TestProcesses {
    Get-AndrunaProcesses | Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

function Get-ShortcutInfo([string] $Path) {
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut($Path)
    [pscustomobject]@{ Target = $s.TargetPath; Arguments = $s.Arguments; Icon = $s.IconLocation }
}

# What the comparison after the restore looks at.
function Get-ProfileSnapshot {
    [ordered]@{
        uninstall = @(Get-ChildItem $UninstallRoot | Where-Object { $_.PSChildName -like 'Andruna*' } | ForEach-Object {
                $k = $_; "$($k.PSChildName)=" + (($k.GetValueNames() | Sort-Object | ForEach-Object { "${_}:$($k.GetValue($_))" }) -join '|')
            })
        classes = @($ClassKeys | ForEach-Object { "$_=$(Test-Path "HKCU:\Software\Classes\$_")" })
        aumids = @(Get-ChildItem 'HKCU:\Software\Classes\AppUserModelId' -ErrorAction SilentlyContinue | ForEach-Object PSChildName | Sort-Object)
        start_menu = @(Get-ChildItem $Programs -Filter *.lnk | ForEach-Object { "$($_.Name):$($_.Length):$($_.LastWriteTimeUtc.Ticks)" } | Sort-Object)
        data_root = if (Test-Path $DataRoot) {
            @(Get-ChildItem $DataRoot -Recurse -File | Where-Object { $_.FullName -notlike "$DataRoot\logs\*" } |
                    ForEach-Object { "$($_.FullName.Substring($DataRoot.Length)):$((Get-FileHash $_.FullName -Algorithm SHA256).Hash)" } | Sort-Object)
        } else { @('(none)') }
    }
}

function Backup-Profile {
    $state = [ordered]@{ uninstall = $false; lnk = $false; data = $false; classes = @() }
    if (Test-Path "$UninstallRoot\Andruna") {
        [void](Invoke-Native reg.exe @('export', 'HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Andruna', (Join-Path $Backup 'uninstall.reg'), '/y'))
        $state.uninstall = $true
    }
    $i = 0
    foreach ($k in $ClassKeys) {
        if (Test-Path "HKCU:\Software\Classes\$k") {
            $file = Join-Path $Backup "classes-$i.reg"
            [void](Invoke-Native reg.exe @('export', "HKCU\Software\Classes\$k", $file, '/y'))
            $state.classes += $k
        }
        $i++
    }
    if (Test-Path (Join-Path $Programs 'Andruna.lnk')) {
        Copy-Item (Join-Path $Programs 'Andruna.lnk') (Join-Path $Backup 'Andruna.lnk')
        $state.lnk = $true
    }
    if (Test-Path $DataRoot) {
        & robocopy.exe $DataRoot (Join-Path $Backup 'data') /E /COPY:DAT /DCOPY:DAT /R:1 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "backing up $DataRoot failed" }
        $state.data = $true
    }
    $state
}

function Restore-Profile($State) {
    # Registry: the product's keys as they were.
    if (Test-Path "$UninstallRoot\Andruna") { Remove-Item -Recurse -Force "$UninstallRoot\Andruna" }
    if ($State.uninstall) { [void](Invoke-Native reg.exe @('import', (Join-Path $Backup 'uninstall.reg'))) }
    $i = 0
    foreach ($k in $ClassKeys) {
        $p = "HKCU:\Software\Classes\$k"
        if ($State.classes -contains $k) {
            [void](Invoke-Native reg.exe @('import', (Join-Path $Backup "classes-$i.reg")))
        }
        elseif (Test-Path $p) { Remove-Item -Recurse -Force $p }
        $i++
    }
    $lnk = Join-Path $Programs 'Andruna.lnk'
    if ($State.lnk) {
        Copy-Item -Force (Join-Path $Backup 'Andruna.lnk') $lnk
        (Get-Item $lnk).LastWriteTimeUtc = (Get-Item (Join-Path $Backup 'Andruna.lnk')).LastWriteTimeUtc
    }
    elseif (Test-Path $lnk) { Remove-Item -Force $lnk }
    # The data root exactly as it was.
    if (Test-Path $DataRoot) { Remove-Item -Recurse -Force $DataRoot }
    if ($State.data) {
        & robocopy.exe (Join-Path $Backup 'data') $DataRoot /E /COPY:DAT /DCOPY:DAT /R:1 /W:1 /NFL /NDL /NJH /NJS /NP | Out-Null
        if ($LASTEXITCODE -ge 8) { throw "restoring $DataRoot failed" }
    }
}

function Get-AppDir([string] $Package) { Join-Path $DataRoot "app\$Package" }

function Test-PackageDesktop([string] $Package, [bool] $Present) {
    $entry = Test-Path "$UninstallRoot\Andruna.App.$Package"
    $links = @(Get-ChildItem $Programs -Filter *.lnk | Where-Object {
            $i = Get-ShortcutInfo $_.FullName
            $i.Target -ieq (Join-Path $InstallDir 'andruna-launch.exe') -and ($i.Arguments -split ' ')[0] -match "^$([regex]::Escape($Package))(/|$)"
        })
    if ($Present) {
        if (-not $entry) { throw "no Installed apps entry Andruna.App.$Package" }
        if ($links.Count -lt 1) { throw "no Start menu shortcut for $Package" }
        $k = Get-Item "$UninstallRoot\Andruna.App.$Package"
        $u = [string]$k.GetValue('UninstallString')
        if (-not $u.StartsWith("`"$InstallDir\andruna-launch.exe`"", [StringComparison]::OrdinalIgnoreCase)) { throw "UninstallString is $u" }
        "entry '$($k.GetValue('DisplayName'))' $($k.GetValue('DisplayVersion')); shortcut '$($links[0].Name)' -> andruna-launch $((Get-ShortcutInfo $links[0].FullName).Arguments)"
    }
    else {
        if ($entry) { throw "Installed apps entry Andruna.App.$Package is still there" }
        if ($links.Count -gt 0) { throw "shortcut $($links[0].Name) is still there" }
        'no entry, no shortcut'
    }
}

# --- Screenshots ---------------------------------------------------------

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
public static class AndrunaE2EShot {
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int L, T, R, B; }
    // PW_RENDERFULLCONTENT: the window as DWM composes it, also when it is
    // covered or the session is locked.
    public static string Save(IntPtr h, string path) {
        RECT r; GetWindowRect(h, out r);
        int w = r.R - r.L, hh = r.B - r.T;
        using (var bmp = new Bitmap(w, hh, PixelFormat.Format32bppArgb))
        using (var g = Graphics.FromImage(bmp)) {
            IntPtr dc = g.GetHdc();
            bool ok = PrintWindow(h, dc, 2);
            g.ReleaseHdc(dc);
            if (!ok) return "PrintWindow failed";
            bmp.Save(path, ImageFormat.Png);
            return w + "x" + hh;
        }
    }
}
'@

function Wait-ManagerWindow([int] $Seconds = 30) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $p = Get-Process -Name 'Andruna.Manager' -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
        if ($p) { return $p }
        Start-Sleep -Milliseconds 500
    }
    throw 'the manager window did not appear'
}

function Save-ManagerShot([string] $Language, [string[]] $ManagerArgs, [string] $Name) {
    Get-Process -Name 'Andruna.Manager' -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
    $file = Join-Path $ScreenshotDir "$Name.png"
    if ($ManagerSnapshotExe) {
        $settings = Join-Path $Work "manager-$Language.json"
        [IO.File]::WriteAllText($settings, "{ `"language`": `"$Language`", `"theme`": `"light`" }")
        $env:ANDRUNA_MANAGER_SNAPSHOT = $file
        $env:ANDRUNA_MANAGER_SETTINGS = $settings
        try {
            Remove-Item $file -ErrorAction SilentlyContinue
            $p = if ($ManagerArgs.Count -gt 0) { Start-Process $ManagerSnapshotExe -ArgumentList $ManagerArgs -PassThru } else { Start-Process $ManagerSnapshotExe -PassThru }
            if (-not $p.WaitForExit(60000)) { Stop-Process -Id $p.Id -Force; throw 'the snapshot manager did not exit' }
        }
        finally {
            Remove-Item Env:\ANDRUNA_MANAGER_SNAPSHOT, Env:\ANDRUNA_MANAGER_SETTINGS -ErrorAction SilentlyContinue
        }
        if (-not (Test-Path $file)) { throw "no $file" }
        return "$file (rendered by the Debug manager against the installed sysd)"
    }
    $settings = Join-Path $DataRoot 'system\manager.json'
    New-Item -ItemType Directory -Force (Split-Path $settings) | Out-Null
    [IO.File]::WriteAllText($settings, "{ `"language`": `"$Language`", `"theme`": `"light`" }")
    $exe = Join-Path $InstallDir 'Andruna.Manager.exe'
    $p = if ($ManagerArgs.Count -gt 0) { Start-Process $exe -ArgumentList $ManagerArgs -PassThru } else { Start-Process $exe -PassThru }
    $w = Wait-ManagerWindow
    # Let the page load the library from sysd and render the icons.
    Start-Sleep -Seconds 6
    [void][AndrunaE2EShot]::ShowWindow($w.MainWindowHandle, 9)
    [void][AndrunaE2EShot]::SetForegroundWindow($w.MainWindowHandle)
    Start-Sleep -Milliseconds 800
    $size = [AndrunaE2EShot]::Save($w.MainWindowHandle, $file)
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    "$file ($size)"
}

# --- The test ------------------------------------------------------------

# A product sysd or manager of this user would take the test's pipe and
# window; processes of development trees (other instances) do not matter.
$running = @(Get-AndrunaProcesses | Where-Object {
        $_.Name -eq 'Andruna.Manager' -or ($_.Name -eq 'andruna-sysd' -and (Get-CimInstance Win32_Process -Filter "ProcessId=$($_.Id)").CommandLine -notmatch '--instance|--root')
    })
if ($running.Count -gt 0 -or (Test-Path "\\.\pipe\$(Get-SysdPipeName)")) {
    if (-not $StopRunning) {
        throw "Andruna processes are running ($(($running | ForEach-Object { "$($_.Name) $($_.Id)" }) -join ', ')); close them or pass -StopRunning"
    }
    $running | Stop-Process -Force
    Start-Sleep -Seconds 1
}
if (Test-Path $InstallDir) { throw "$InstallDir exists; remove it or choose another -InstallDir" }

$snapshot = Get-ProfileSnapshot
# The Debug manager registers for notifications under its own path; that
# registration is removed afterwards when the test created it.
$snapshotExeDir = if ($ManagerSnapshotExe) { Split-Path -Parent ([IO.Path]::GetFullPath($ManagerSnapshotExe)) } else { $null }
$snapshotExeRegistered = $snapshotExeDir -and (Test-Path ('HKCU:\Software\Classes\AppUserModelId\' + (Join-Path $snapshotExeDir 'Andruna.Manager.exe').ToLowerInvariant().Replace('\', '.')))
$state = Backup-Profile
Write-Host "Backup in $Backup (uninstall entry: $($state.uninstall), shortcut: $($state.lnk), data root: $($state.data), file types: $($state.classes -join ','))"
$conn = $null
$launchExe = Join-Path $InstallDir 'andruna-launch.exe'

try {
    # 2. Install.
    Test-Step 'install (silent)' {
        $p = Start-Process $Installer -ArgumentList '/S', "/D=$InstallDir" -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "installer exit code $($p.ExitCode)" }
        if (-not (Test-Path (Join-Path $InstallDir 'Andruna.Manager.exe'))) { throw 'Andruna.Manager.exe was not installed' }
        $size = (Get-ChildItem $InstallDir -Recurse -File | Measure-Object Length -Sum).Sum
        "installed $([math]::Round($size / 1MB, 1)) MiB into $InstallDir"
    }
    $installStep = $results.steps[$results.steps.Count - 1]
    $results.install_ms = $installStep.ms

    # 3. The install.
    Test-Step 'runtime files' {
        foreach ($f in 'andruna-sysd.exe', 'andruna-launch.exe', 'andruna-app.exe', 'angle\libEGL.dll', 'angle\libGLESv2.dll',
            'art\lib64\libart.so', 'art\bin\dex2oat', 'art\javalib\core-oj.jar', 'THIRD_PARTY_NOTICES.txt') {
            if (-not (Test-Path (Join-Path $InstallDir $f))) { throw "$f is missing" }
        }
        $art = (Get-ChildItem (Join-Path $InstallDir 'art') -Recurse -File | Measure-Object Length -Sum).Sum
        "art\ $([math]::Round($art / 1MB, 1)) MiB (with the boot image)"
    }
    Test-Step 'boot image written at install' {
        $dir = Join-Path $InstallDir "art\javalib\$Isa"
        if (-not (Test-Path (Join-Path $dir 'boot.art'))) { throw "no $dir\boot.art" }
        $m = Select-String -Path (Join-Path $DataRoot 'logs\sysd.log') -Pattern 'event=boot_image_written ms=(\d+)' | Select-Object -Last 1
        $ms = if ($m) { [int]$m.Matches[0].Groups[1].Value } else { -1 }
        $results.boot_image_ms = $ms
        $files = Get-ChildItem $dir -File
        "$($files.Count) files, $([math]::Round(($files | Measure-Object Length -Sum).Sum / 1MB, 1)) MiB, dex2oat $ms ms"
    }
    Test-Step 'andrunaRuntime ACE' {
        $acl = & icacls.exe $InstallDir
        $ace = $acl | Where-Object { $_ -match 'S-1-15-3-1024-[\d-]+:\(OI\)\(CI\)\(RX\)' }
        if (-not $ace) { throw "no inheritable RX ACE for a capability SID on $InstallDir" }
        $boot = & icacls.exe (Join-Path $InstallDir "art\javalib\$Isa\boot.art")
        if (-not ($boot | Where-Object { $_ -match 'S-1-15-3-1024-[\d-]+:\(I\)\(RX\)' })) { throw 'the boot image does not inherit the ACE' }
        ($ace -replace '^.*?(S-1-15-3-1024-)', '$1').Trim()
    }
    Test-Step 'signatures' {
        $files = Get-ChildItem $InstallDir -Recurse -Include *.exe, *.dll
        $ours = 0
        $vendor = 0
        foreach ($f in $files) {
            $s = Get-AuthenticodeSignature $f.FullName
            if (-not $s.SignerCertificate) { throw "$($f.FullName) is not signed" }
            if ($Thumbprint -and $s.SignerCertificate.Thumbprint -eq $Thumbprint.ToUpperInvariant()) {
                if (-not $s.TimeStamperCertificate) { throw "$($f.Name) has no timestamp" }
                $ours++
            }
            else { $vendor++ }
        }
        foreach ($f in 'andruna-sysd.exe', 'andruna-launch.exe', 'andruna-app.exe', 'Andruna.Manager.exe', 'angle\libEGL.dll', 'angle\libGLESv2.dll', 'Uninstall Andruna.exe') {
            $s = Get-AuthenticodeSignature (Join-Path $InstallDir $f)
            if ($Thumbprint -and $s.SignerCertificate.Thumbprint -ne $Thumbprint.ToUpperInvariant()) { throw "$f is signed by $($s.SignerCertificate.Subject)" }
        }
        "$($files.Count) binaries signed: $ours with $Thumbprint (timestamped), $vendor by their vendor"
    }
    Test-Step 'Andruna in Installed apps and Start menu' {
        $k = Get-Item "$UninstallRoot\Andruna"
        if ($k.GetValue('InstallLocation') -ne $InstallDir) { throw "InstallLocation is $($k.GetValue('InstallLocation'))" }
        $i = Get-ShortcutInfo (Join-Path $Programs 'Andruna.lnk')
        if ($i.Target -ine (Join-Path $InstallDir 'Andruna.Manager.exe')) { throw "Andruna.lnk targets $($i.Target)" }
        "EstimatedSize $($k.GetValue('EstimatedSize')) KiB; Andruna.lnk -> $($i.Target)"
    }

    # 4. sysd and packages.
    Test-Step 'sysd start and ping' {
        Start-Process (Join-Path $InstallDir 'andruna-sysd.exe') | Out-Null
        $deadline = (Get-Date).AddSeconds(15)
        while (-not $script:conn) {
            try { $script:conn = Connect-Sysd -ExpectedDir $InstallDir -TimeoutMs 1000 }
            catch { if ((Get-Date) -gt $deadline) { throw }; Start-Sleep -Milliseconds 200 }
        }
        $v = Invoke-SysdPing $script:conn
        $s = Invoke-SysdStatus $script:conn
        "protocol $v, sysd $($s.Version) pid $($s.Pid)"
    }
    if (-not $conn) { throw 'sysd did not start; the remaining steps need it' }

    $apks = [ordered]@{}
    $apks['org.andruna.e2e'] = Join-Path $PSScriptRoot 'fixture\e2e.apk'
    $apks['org.andruna.fixture'] = Join-Path $Repo 'crates\andruna-pkg\tests\fixtures\real\fixture.apk'
    if (-not $SkipNetwork) {
        Test-Step 'download Binary Eye (F-Droid, pinned SHA-256)' {
            $dst = Join-Path $Work $BinaryEye.File
            $cache = Join-Path $Repo ".cache\e2e\$($BinaryEye.File)"
            if ((Test-Path $cache) -and (Get-FileHash $cache).Hash -eq $BinaryEye.Sha256) { Copy-Item $cache $dst }
            else {
                foreach ($u in $BinaryEye.Urls) {
                    & curl.exe -fsSL --max-time 180 -o $dst "$u$($BinaryEye.File)"
                    if ($LASTEXITCODE -eq 0 -and (Get-FileHash $dst).Hash -eq $BinaryEye.Sha256) { break }
                    Remove-Item $dst -ErrorAction SilentlyContinue
                }
                if (-not (Test-Path $dst)) { throw 'download failed from every mirror' }
                New-Item -ItemType Directory -Force (Split-Path $cache) | Out-Null
                Copy-Item $dst $cache
            }
            "$([math]::Round((Get-Item $dst).Length / 1MB, 1)) MiB, SHA-256 ok"
        }
        $be = Join-Path $Work $BinaryEye.File
        if (Test-Path $be) { $apks['de.markusfisch.android.binaryeye'] = $be }
    }

    foreach ($pkg in $apks.Keys) {
        # sysd takes absolute paths; a copy keeps the repository's files untouched.
        $apk = Join-Path $Work "$pkg.apk"
        Copy-Item $apks[$pkg] $apk
        Test-Step "inspect $pkg" {
            $d = Invoke-SysdInspect $script:conn @($apk) 'en'
            if ($d.Package -ne $pkg) { throw "package is $($d.Package)" }
            "'$($d.Label)' $($d.VersionName), scheme v$($d.Scheme), ABI '$($d.SelectedAbi)', $($d.Permissions.Count) permissions"
        }
        Test-Step "install $pkg" {
            $r = Invoke-SysdInstall $script:conn @($apk)
            if ($r.Package -ne $pkg) { throw "installed $($r.Package)" }
            if ($r.Compile -ne 'compiled') { throw "compile step: $($r.Compile) $($r.CompileMessage)" }
            "compile $($r.Compile), $($r.Shortcuts) shortcut(s)"
        }
        Test-Step "dex2oat output $pkg" {
            $odex = Join-Path (Get-AppDir $pkg) "oat\$Isa\base.odex"
            $vdex = Join-Path (Get-AppDir $pkg) "oat\$Isa\base.vdex"
            if (-not (Test-Path $odex)) { throw "no $odex" }
            "base.odex $((Get-Item $odex).Length) bytes, base.vdex $(if (Test-Path $vdex) { (Get-Item $vdex).Length } else { 0 }) bytes"
        }
        Test-Step "Start menu and Installed apps $pkg" { Test-PackageDesktop $pkg $true }
    }
    Test-Step 'ListPackageInfo' {
        $l = @(Invoke-SysdListInfo $script:conn 'he')
        ($l | ForEach-Object { "$($_.Package) '$($_.Label)' $([math]::Round($_.SizeBytes / 1KB)) KiB" }) -join ', '
    }

    # 5. Java code from the installed package.
    $base = Join-Path (Get-AppDir 'org.andruna.e2e') 'base.apk'
    foreach ($mode in 'jit', 'aot') {
        Test-Step "run org.andruna.e2e.Main from the installed package ($mode)" {
            $data = Join-Path $Work "run-$mode"
            $r = Invoke-Native (Join-Path $InstallDir 'andruna-app.exe') @('--data', $data, '--mode', $mode, '--classpath', $base, 'org.andruna.e2e.Main', 'e2e', $mode)
            $out = $r.Output
            $code = $r.Code
            $line = @($out | Where-Object { "$_" -like 'andruna-e2e:*' }) | Select-Object -First 1
            if ($code -ne 0 -or -not $line) { throw "exit $code; output: $(($out | Select-Object -Last 5) -join ' | ')" }
            "$line"
        }
    }
    Test-Step 'StartPackage through andruna-launch' {
        $p = Start-Process $launchExe -ArgumentList '--quiet', 'org.andruna.e2e' -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "andruna-launch exit code $($p.ExitCode)" }
        Start-Sleep -Seconds 1
        $s = Invoke-SysdStatus $script:conn
        $hosts = @(Get-CimInstance Win32_Process -Filter "Name='andruna-sysd.exe' OR Name='andruna-app.exe'" |
                Where-Object { $_.CommandLine -match 'org\.andruna\.e2e' })
        $kind = if ($hosts | Where-Object { $_.CommandLine -match '--host' }) { 'the placeholder host (andruna-sysd --host)' }
            elseif ($hosts) { 'andruna-app' } else { 'no host process found' }
        "exit 0, sysd reports $($s.Hosts) host(s): $kind"
    }

    # 6. Screenshots of the manager with the installed apps.
    foreach ($lang in @(@('english', 'en'), @('hebrew', 'he'))) {
        Test-Step "screenshot library ($($lang[1]))" { Save-ManagerShot $lang[0] @() "manager-library-$($lang[1])" }
        Test-Step "screenshot app page ($($lang[1]))" { Save-ManagerShot $lang[0] @('--app', 'org.andruna.e2e') "manager-app-$($lang[1])" }
    }
    Get-Process -Name 'Andruna.Manager' -ErrorAction SilentlyContinue | Stop-Process -Force

    # 7. Uninstall the packages.
    Get-CimInstance Win32_Process -Filter "Name='andruna-sysd.exe'" | Where-Object { $_.CommandLine -match '--host' } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 1
    foreach ($pkg in $apks.Keys) {
        Test-Step "uninstall $pkg" {
            Invoke-SysdUninstall $script:conn $pkg $false
            if (Test-Path (Get-AppDir $pkg)) { throw "$(Get-AppDir $pkg) is still there" }
            if (Test-Path (Join-Path $DataRoot "data\$pkg")) { throw "data\$pkg is still there" }
            Test-PackageDesktop $pkg $false
        }
    }
    Test-Step 'install org.andruna.fixture again (left for the Andruna uninstaller)' {
        $r = Invoke-SysdInstall $script:conn @(Join-Path $Work 'org.andruna.fixture.apk')
        Test-PackageDesktop 'org.andruna.fixture' $true | Out-Null
        "compile $($r.Compile)"
    }
    $conn.Close()
    $conn = $null
    Stop-TestProcesses

    # 8. Uninstall Andruna.
    Test-Step 'uninstall Andruna (silent)' {
        $un = Join-Path $InstallDir 'Uninstall Andruna.exe'
        $p = Start-Process $un -ArgumentList '/S' -Wait -PassThru
        if ($p.ExitCode -ne 0) { throw "uninstaller exit code $($p.ExitCode)" }
        # The uninstaller runs from a copy in %TEMP%; wait for it to finish.
        $deadline = (Get-Date).AddSeconds(120)
        while ((Test-Path $InstallDir) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
        if (Test-Path $InstallDir) { throw "$InstallDir is still there" }
        'install folder removed'
    }
    Test-Step 'cleanup after uninstall' {
        $left = @()
        if (Test-Path "$UninstallRoot\Andruna") { $left += 'Installed apps entry' }
        if (Test-Path (Join-Path $Programs 'Andruna.lnk')) { $left += 'Andruna.lnk' }
        foreach ($k in 'Andruna.Package', 'Applications\Andruna.Manager.exe') { if (Test-Path "HKCU:\Software\Classes\$k") { $left += $k } }
        foreach ($e in '.apk', '.apks', '.xapk', '.apkm') {
            $p = "HKCU:\Software\Classes\$e\OpenWithProgids"
            if ((Test-Path $p) -and ((Get-Item $p).GetValueNames() -contains 'Andruna.Package')) { $left += "$e OpenWithProgids" }
        }
        try { Test-PackageDesktop 'org.andruna.fixture' $false | Out-Null } catch { $left += $_.Exception.Message }
        $aumid = @(Get-ChildItem 'HKCU:\Software\Classes\AppUserModelId' -ErrorAction SilentlyContinue | Where-Object {
                $_.PSChildName -like "*$(($InstallDir.ToLowerInvariant() -replace '\\', '.'))*" -or [string]$_.GetValue('IconUri') -like "$InstallDir\*"
            })
        if ($aumid.Count -gt 0) { $left += "notification registration $($aumid[0].PSChildName)" }
        if (@(Get-AndrunaProcesses | Where-Object { $_.Path -like "$InstallDir\*" }).Count -gt 0) { $left += 'running processes' }
        if ($left.Count -gt 0) { throw "left behind: $($left -join ', ')" }
        $kept = if (Test-Path (Get-AppDir 'org.andruna.fixture')) { 'kept (silent uninstall keeps app data, as designed)' } else { 'removed' }
        "nothing of the product left; the Android app's files in the data root: $kept"
    }
}
catch {
    Add-Step 'aborted' $false $_.Exception.Message
}
finally {
    if ($conn) { $conn.Close() }
    Stop-TestProcesses
    if (Test-Path (Join-Path $InstallDir 'Uninstall Andruna.exe')) {
        Start-Process (Join-Path $InstallDir 'Uninstall Andruna.exe') -ArgumentList '/S' -Wait | Out-Null
        $deadline = (Get-Date).AddSeconds(60)
        while ((Test-Path $InstallDir) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir -ErrorAction SilentlyContinue }
    $parent = Join-Path $env:LOCALAPPDATA 'AndrunaE2E'
    if ((Test-Path $parent) -and -not (Get-ChildItem $parent -Recurse -File)) { Remove-Item -Recurse -Force $parent }
    # Shortcuts and entries of Android apps that a failed step left behind.
    Get-ChildItem $UninstallRoot | Where-Object { $_.PSChildName -like 'Andruna.App.*' -and [string]$_.GetValue('UninstallString') -like "*$InstallDir*" } |
        Remove-Item -Recurse -Force
    Get-ChildItem $Programs -Filter *.lnk | Where-Object { (Get-ShortcutInfo $_.FullName).Target -like "$InstallDir\*" } | Remove-Item -Force
    if ($snapshotExeDir -and -not $snapshotExeRegistered) {
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $Repo 'installer\uninstall-cleanup.ps1') -InstallDir $snapshotExeDir
    }
    try { Restore-Profile $state }
    catch { Add-Step 'restore' $false "$($_.Exception.Message); the backup is in $Backup" }
    $after = Get-ProfileSnapshot
    $diff = @()
    foreach ($k in $snapshot.Keys) {
        $a = @($snapshot[$k]) -join "`n"
        $b = @($after[$k]) -join "`n"
        if ($a -ne $b) { $diff += $k }
    }
    Add-Step 'profile restored as before' ($diff.Count -eq 0) $(if ($diff.Count -eq 0) { 'uninstall entries, file types, notification registrations, Start menu and data root match the snapshot' } else { "differs: $($diff -join ', ')" })
    $left = @(Get-AndrunaProcesses | Where-Object { $_.Path -like "$InstallDir\*" })
    Add-Step 'no test processes left' ($left.Count -eq 0) "$($left.Count) left"

    $results.finished = (Get-Date).ToString('s')
    $results.failures = @($failures)
    $results | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 (Join-Path $OutDir 'results.json')
    $md = @('| Step | Result | Time | Detail |', '| --- | --- | ---: | --- |')
    foreach ($s in $results.steps) {
        $t = if ($s.ms -ge 0) { "$([math]::Round($s.ms)) ms" } else { '' }
        $md += "| $($s.step) | $(if ($s.ok) { 'ok' } else { '**failed**' }) | $t | $(($s.detail -replace '\|', '/') -replace "`r?`n", ' ') |"
    }
    [IO.File]::WriteAllLines((Join-Path $OutDir 'results.md'), $md, [Text.UTF8Encoding]::new($false))
    Write-Host "Results: $(Join-Path $OutDir 'results.md')"
}
if ($failures.Count -gt 0) {
    Write-Host "FAILED: $($failures -join ', ')"
    exit 1
}
Write-Host 'All steps passed.'

