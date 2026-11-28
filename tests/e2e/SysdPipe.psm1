# A minimal client of sysd's pipe protocol (crates/andruna-ipc/README.md),
# version 1, for the end-to-end test. Windows PowerShell 5.1 (.NET Framework).
#
# Only what the test needs: Ping, Status, InspectPackage, InstallPackage,
# UninstallPackage, ListPackageInfo, StartPackage. The client checks that the
# pipe's server process runs from the expected install directory before it
# sends anything.

Set-StrictMode -Version Latest

Add-Type -Namespace AndrunaE2E -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool GetNamedPipeServerProcessId(Microsoft.Win32.SafeHandles.SafePipeHandle pipe, out uint pid);
'@

$script:Utf8 = [Text.UTF8Encoding]::new($false)

function Get-SysdPipeName([string] $Instance = 'default') {
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $session = (Get-Process -Id $PID).SessionId
    "Andruna.sysd.$sid.$session.$Instance"
}

class SysdConnection {
    [IO.Pipes.NamedPipeClientStream] $Pipe
    [uint32] $NextId = 1
    [uint32] $ServerPid

    SysdConnection([string] $Name, [string] $ExpectedDir, [int] $TimeoutMs) {
        $this.Pipe = [IO.Pipes.NamedPipeClientStream]::new('.', $Name, [IO.Pipes.PipeDirection]::InOut,
            [IO.Pipes.PipeOptions]::None, [Security.Principal.TokenImpersonationLevel]::Identification)
        $this.Pipe.Connect($TimeoutMs)
        [uint32] $p = 0
        $native = 'AndrunaE2E.Native' -as [type]
        if (-not $native::GetNamedPipeServerProcessId($this.Pipe.SafePipeHandle, [ref] $p)) {
            throw 'GetNamedPipeServerProcessId failed'
        }
        $this.ServerPid = $p
        $path = (Get-Process -Id $p).Path
        if ($ExpectedDir -and -not $path.StartsWith($ExpectedDir, [StringComparison]::OrdinalIgnoreCase)) {
            $this.Pipe.Dispose()
            throw "the sysd pipe is served by $path, not by the install under test ($ExpectedDir)"
        }
    }

    [void] Close() { $this.Pipe.Dispose() }

    hidden [byte[]] ReadExact([int] $n) {
        $b = [byte[]]::new($n)
        $o = 0
        while ($o -lt $n) {
            $r = $this.Pipe.Read($b, $o, $n - $o)
            if ($r -le 0) { throw 'sysd closed the connection' }
            $o += $r
        }
        return $b
    }

    # Sends one request; returns @{ Kind; Body (SysdReader) }.
    [hashtable] Call([uint16] $Kind, [byte[]] $Body) {
        $id = $this.NextId++
        $ms = [IO.MemoryStream]::new()
        $w = [IO.BinaryWriter]::new($ms)
        $w.Write([uint32](8 + $Body.Length))
        $w.Write([uint16]1)
        $w.Write($Kind)
        $w.Write($id)
        $w.Write($Body)
        $w.Flush()
        $frame = $ms.ToArray()
        $this.Pipe.Write($frame, 0, $frame.Length)
        $this.Pipe.Flush()
        $len = [BitConverter]::ToUInt32($this.ReadExact(4), 0)
        $payload = $this.ReadExact([int]$len)
        $rkind = [BitConverter]::ToUInt16($payload, 2)
        $rid = [BitConverter]::ToUInt32($payload, 4)
        if ($rid -ne $id) { throw "response id $rid for request $id" }
        $r = [SysdReader]::new($payload, 8)
        if ($rkind -eq 0x80FF) {
            $code = $r.U16()
            $msg = $r.Str()
            throw "sysd error ${code}: $msg"
        }
        return @{ Kind = $rkind; Body = $r }
    }
}

class SysdReader {
    [byte[]] $B
    [int] $O
    SysdReader([byte[]] $b, [int] $o) { $this.B = $b; $this.O = $o }
    [byte] U8() { $v = $this.B[$this.O]; $this.O += 1; return $v }
    [bool] Bool() { return $this.U8() -ne 0 }
    [uint16] U16() { $v = [BitConverter]::ToUInt16($this.B, $this.O); $this.O += 2; return $v }
    [uint32] U32() { $v = [BitConverter]::ToUInt32($this.B, $this.O); $this.O += 4; return $v }
    [uint64] U64() { $v = [BitConverter]::ToUInt64($this.B, $this.O); $this.O += 8; return $v }
    [string] Str() {
        $n = $this.U16()
        $v = [Text.Encoding]::UTF8.GetString($this.B, $this.O, $n)
        $this.O += $n
        return $v
    }
    [string[]] Strs() {
        $n = $this.U32()
        $a = [Collections.Generic.List[string]]::new()
        for ($i = 0; $i -lt $n; $i++) { $a.Add($this.Str()) }
        return $a.ToArray()
    }
}

function New-SysdBody {
    $ms = [IO.MemoryStream]::new()
    [pscustomobject]@{ Stream = $ms; Writer = [IO.BinaryWriter]::new($ms) }
}

function Add-Str($Body, [string] $S) {
    $b = $script:Utf8.GetBytes($S)
    $Body.Writer.Write([uint16]$b.Length)
    $Body.Writer.Write($b)
}

function Add-Strs($Body, [string[]] $S) {
    $Body.Writer.Write([uint32]$S.Count)
    foreach ($x in $S) { Add-Str $Body $x }
}

function Get-Bytes($Body) { $Body.Writer.Flush(); , $Body.Stream.ToArray() }

function Connect-Sysd([string] $ExpectedDir, [int] $TimeoutMs = 10000, [string] $Instance = 'default') {
    [SysdConnection]::new((Get-SysdPipeName $Instance), $ExpectedDir, $TimeoutMs)
}

function Invoke-SysdPing($C) {
    $r = $C.Call(0x0001, [byte[]]@())
    $r.Body.U16()
}

function Invoke-SysdStatus($C) {
    $r = $C.Call(0x0002, [byte[]]@()).Body
    [pscustomobject]@{ Pid = $r.U32(); UptimeMs = $r.U64(); Connections = $r.U32(); Hosts = $r.U32(); IdleTimeoutMs = $r.U64(); Version = $r.Str() }
}

function Invoke-SysdInspect($C, [string[]] $Apks, [string] $Locale = 'en') {
    $b = New-SysdBody; Add-Strs $b $Apks; Add-Str $b $Locale
    $r = $C.Call(0x0006, (Get-Bytes $b)).Body
    [pscustomobject]@{
        Package = $r.Str(); VersionCode = $r.U64(); VersionName = $r.Str(); Label = $r.Str()
        MinSdk = $r.U32(); TargetSdk = $r.U32(); SignerSha256 = $r.Str(); Scheme = $r.U8()
        Permissions = $r.Strs(); NativeAbis = $r.Strs(); SelectedAbi = $r.Str()
        InstalledVersionCode = $r.U64(); IconPng = $r.Str()
    }
}

$script:CompileStatus = @('compiled', 'no_code', 'unavailable', 'failed')

function Invoke-SysdInstall($C, [string[]] $Apks) {
    $b = New-SysdBody; Add-Strs $b $Apks
    $r = $C.Call(0x0007, (Get-Bytes $b)).Body
    [pscustomobject]@{
        Package = $r.Str(); VersionCode = $r.U64(); VersionName = $r.Str(); Updated = $r.Bool()
        Compile = $script:CompileStatus[$r.U8()]; CompileMessage = $r.Str(); Shortcuts = $r.U32()
    }
}

function Invoke-SysdUninstall($C, [string] $Package, [bool] $KeepData = $false) {
    $b = New-SysdBody; Add-Str $b $Package; $b.Writer.Write([byte]($(if ($KeepData) { 1 } else { 0 })))
    [void]$C.Call(0x0008, (Get-Bytes $b))
}

function Invoke-SysdListInfo($C, [string] $Locale = 'en') {
    $b = New-SysdBody; Add-Str $b $Locale
    $r = $C.Call(0x0009, (Get-Bytes $b)).Body
    $n = $r.U32()
    for ($i = 0; $i -lt $n; $i++) {
        [pscustomobject]@{
            Package = $r.Str(); Label = $r.Str(); VersionCode = $r.U64(); VersionName = $r.Str()
            IconIco = $r.Str(); IconPng = $r.Str(); Launchers = $r.Strs(); InstalledMs = $r.U64()
            SizeBytes = $r.U64(); AppUserModelId = $r.Str()
        }
    }
}

function Invoke-SysdStartPackage($C, [string] $Package) {
    $b = New-SysdBody; Add-Str $b $Package
    $r = $C.Call(0x0003, (Get-Bytes $b)).Body
    [pscustomobject]@{ HostPid = $r.U32(); AlreadyRunning = $r.Bool() }
}

Export-ModuleMember -Function Get-SysdPipeName, Connect-Sysd, Invoke-Sysd*
