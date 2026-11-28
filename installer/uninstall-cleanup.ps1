<#
.SYNOPSIS
Run by the Andruna uninstaller (andruna.nsi) before it removes the install
folder: removes what points into that folder from the user's profile.

.DESCRIPTION
- "Installed apps" entries of Android apps (ADR 0027) whose uninstall
  command starts this folder's andruna-launch.exe.
- Start menu shortcuts of Android apps that target that andruna-launch.exe.
  The apps' files and data stay unless the user removes them in the
  uninstaller.
- The manager's notification registration, which Windows App SDK's
  UnregisterAll (Andruna.Manager.exe --unregister) leaves behind: the
  AppUserModelId key named after the manager's path, the AppUserModelId key
  whose icon is in this folder, and the COM activator class it names.
#>
param([Parameter(Mandatory = $true)] [string] $InstallDir)

$ErrorActionPreference = 'SilentlyContinue'
$InstallDir = $InstallDir.TrimEnd('\')
$launch = Join-Path $InstallDir 'andruna-launch.exe'
$quoted = [char]34 + $launch + [char]34

Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall' |
    Where-Object { $_.PSChildName -like 'Andruna.App.*' -and ([string]$_.GetValue('UninstallString')).StartsWith($quoted, 'OrdinalIgnoreCase') } |
    Remove-Item -Recurse

$shell = New-Object -ComObject WScript.Shell
Get-ChildItem ([Environment]::GetFolderPath('Programs')) -Filter *.lnk |
    Where-Object { $shell.CreateShortcut($_.FullName).TargetPath -ieq $launch } |
    Remove-Item

$aumids = 'HKCU:\Software\Classes\AppUserModelId'
Get-ChildItem $aumids |
    Where-Object { ([string]$_.GetValue('IconUri')).StartsWith($InstallDir + '\', 'OrdinalIgnoreCase') } |
    ForEach-Object {
        $activator = $_.GetValue('CustomActivator')
        if ($activator) { Remove-Item -Recurse "HKCU:\Software\Classes\CLSID\$activator" }
        Remove-Item -Recurse $_.PSPath
    }
$byPath = (Join-Path $InstallDir 'Andruna.Manager.exe').ToLowerInvariant().Replace('\', '.')
Remove-Item -Recurse (Join-Path $aumids $byPath)
exit 0
