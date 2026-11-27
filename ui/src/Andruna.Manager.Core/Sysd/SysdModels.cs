namespace Andruna.Manager.Core.Sysd;

/// <summary>Body of the Status response (kind 0x8002).</summary>
public sealed record SysdStatus(
    uint Pid,
    ulong UptimeMs,
    uint Connections,
    uint Hosts,
    ulong IdleTimeoutMs,
    string Version);

/// <summary>
/// One installed package as the library shows it. <see cref="Info"/> holds
/// what ListPackageInfo (0x0009) reported, or is null when sysd reports only
/// names (ListPackages, 0x0005).
/// </summary>
public sealed record PackageSummary(string Name, PackageInfo? Info = null);

/// <summary>One installed package, as ListPackageInfo (0x8009) reports it.</summary>
public sealed record PackageInfo(
    string Package,
    string Label,
    ulong VersionCode,
    string VersionName,
    string IconIcoPath,
    string IconPngPath,
    IReadOnlyList<string> LauncherActivities,
    ulong InstalledMs,
    ulong SizeBytes,
    string AppUserModelId)
{
    public bool Equals(PackageInfo? other) => other is not null &&
        (Package, Label, VersionCode, VersionName, IconIcoPath, IconPngPath, InstalledMs, SizeBytes, AppUserModelId) ==
        (other.Package, other.Label, other.VersionCode, other.VersionName, other.IconIcoPath, other.IconPngPath, other.InstalledMs, other.SizeBytes, other.AppUserModelId) &&
        LauncherActivities.SequenceEqual(other.LauncherActivities);

    public override int GetHashCode() => Package.GetHashCode(StringComparison.Ordinal);
}

/// <summary>What InspectPackage (0x8006) reports about package files before they are installed (P-12).</summary>
public sealed record PackageDetails(
    string Package,
    ulong VersionCode,
    string VersionName,
    string Label,
    uint MinSdk,
    uint TargetSdk,
    string SignerSha256,
    byte SignatureScheme,
    IReadOnlyList<string> Permissions,
    IReadOnlyList<string> NativeAbis,
    string SelectedAbi,
    ulong InstalledVersionCode,
    string IconPngPath)
{
    public bool Equals(PackageDetails? other) => other is not null &&
        (Package, VersionCode, VersionName, Label, MinSdk, TargetSdk, SignerSha256, SignatureScheme, SelectedAbi, InstalledVersionCode, IconPngPath) ==
        (other.Package, other.VersionCode, other.VersionName, other.Label, other.MinSdk, other.TargetSdk, other.SignerSha256, other.SignatureScheme, other.SelectedAbi, other.InstalledVersionCode, other.IconPngPath) &&
        Permissions.SequenceEqual(other.Permissions) && NativeAbis.SequenceEqual(other.NativeAbis);

    public override int GetHashCode() => Package.GetHashCode(StringComparison.Ordinal);
}

/// <summary>How the compile step of an install ended; the package is installed in every case.</summary>
public enum CompileStatus : byte
{
    Compiled = 0,
    NoCode = 1,
    Unavailable = 2,
    Failed = 3,
}

/// <summary>The result of InstallPackage (0x8007).</summary>
public sealed record InstallOutcome(
    string Package,
    ulong VersionCode,
    string VersionName,
    bool Updated,
    CompileStatus Compile,
    string CompileMessage,
    uint Shortcuts);

/// <summary>One entry of the missing-function counters (0x8011): <c>&lt;area&gt;:&lt;entry point&gt;</c> and how often it was called.</summary>
public sealed record MissingFunction(string Name, ulong Calls);

/// <summary>The counters of one package, most-called first; <see cref="UpdatedMs"/> is 0 when none were recorded.</summary>
public sealed record MissingFunctionReport(ulong UpdatedMs, IReadOnlyList<MissingFunction> Entries)
{
    public bool Equals(MissingFunctionReport? other) => other is not null && UpdatedMs == other.UpdatedMs && Entries.SequenceEqual(other.Entries);

    public override int GetHashCode() => UpdatedMs.GetHashCode();
}

/// <summary>An event from the sysd event stream (0x8010).</summary>
public abstract record SysdEvent(ulong Seq, ulong TimeMs)
{
    /// <summary>An app posted or updated a notification (type 1).</summary>
    public sealed record NotificationPosted(ulong Seq, ulong TimeMs, string Package, int Id, string Tag, string Channel, string Title, string Text)
        : SysdEvent(Seq, TimeMs);

    /// <summary>An app cancelled a notification (type 2).</summary>
    public sealed record NotificationRemoved(ulong Seq, ulong TimeMs, string Package, int Id, string Tag) : SysdEvent(Seq, TimeMs);

    /// <summary>An app's process ended abnormally (type 3).</summary>
    public sealed record AppCrashed(ulong Seq, ulong TimeMs, string Package, uint Pid, uint ExitCode, string Reason) : SysdEvent(Seq, TimeMs);

    /// <summary>An event type this version does not know.</summary>
    public sealed record Unknown(ulong Seq, ulong TimeMs, ushort Type) : SysdEvent(Seq, TimeMs);
}

/// <summary>One answer to WaitEvents (0x8010).</summary>
public sealed record SysdEventBatch(ulong Epoch, ulong Next, bool Missed, IReadOnlyList<SysdEvent> Events)
{
    public bool Equals(SysdEventBatch? other) => other is not null &&
        (Epoch, Next, Missed) == (other.Epoch, other.Next, other.Missed) && Events.SequenceEqual(other.Events);

    public override int GetHashCode() => Next.GetHashCode();
}

/// <summary>
/// Reason codes of the Error response (kind 0x80FF). A code from a newer
/// server keeps its number and is not one of the named values; it is
/// treated as a failure.
/// </summary>
public enum SysdErrorCode : ushort
{
    UnsupportedVersion = 1,
    Malformed = 2,
    UnknownRequest = 3,
    InvalidPackageName = 4,
    NotInstalled = 5,
    NotRunning = 6,
    NoWindow = 7,
    HostStartFailed = 8,
    Busy = 9,
    Internal = 10,
    InvalidPackage = 11,
    InvalidSignature = 12,
    SignerMismatch = 13,
    IncompatibleAbi = 14,
    SdkTooNew = 15,
    Downgrade = 16,
    InvalidSplits = 17,
    InstallFailed = 18,
    NotPermitted = 32,
}

public static class SysdErrorCodes
{
    public static SysdErrorCode FromWire(ushort code) => (SysdErrorCode)code;

    /// <summary>True for the codes this client knows.</summary>
    public static bool IsKnown(this SysdErrorCode code) =>
        code is >= SysdErrorCode.UnsupportedVersion and <= SysdErrorCode.InstallFailed or SysdErrorCode.NotPermitted;
}

/// <summary>How a call to the service ended.</summary>
public enum SysdOutcome
{
    /// <summary>The service answered with a result.</summary>
    Ok,

    /// <summary>No service is listening on the pipe.</summary>
    NotRunning,

    /// <summary>The pipe is served by a process of another user; nothing was sent.</summary>
    WrongServerUser,

    /// <summary>The service answered, but not in the expected protocol.</summary>
    ProtocolError,

    /// <summary>The service answered with an Error response (<see cref="SysdResult{T}.ErrorCode"/>).</summary>
    ServiceError,
}

/// <summary>The value of a service call, or why there is none.</summary>
public sealed record SysdResult<T>(SysdOutcome Outcome, T? Value, string? Detail, SysdErrorCode? ErrorCode = null)
{
    public bool IsOk => Outcome == SysdOutcome.Ok;

    /// <summary>True when the service is too old to know the request: the feature is not available yet.</summary>
    public bool IsUnsupported => ErrorCode == SysdErrorCode.UnknownRequest;

    public static SysdResult<T> Ok(T value) => new(SysdOutcome.Ok, value, null);

    public static SysdResult<T> Fail(SysdOutcome outcome, string? detail, SysdErrorCode? code = null) =>
        new(outcome, default, detail, code);

    public SysdResult<TOther> Cast<TOther>() => new(Outcome, default, Detail, ErrorCode);
}
