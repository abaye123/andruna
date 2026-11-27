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
/// One installed package. Protocol version 1 reports only the package name;
/// label and version are not known to the manager yet.
/// </summary>
public sealed record PackageSummary(string Name);

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
}

public static class SysdErrorCodes
{
    public static SysdErrorCode FromWire(ushort code) => (SysdErrorCode)code;

    /// <summary>True for the codes protocol version 1 defines.</summary>
    public static bool IsKnown(this SysdErrorCode code) => code is >= SysdErrorCode.UnsupportedVersion and <= SysdErrorCode.Internal;
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

    public static SysdResult<T> Ok(T value) => new(SysdOutcome.Ok, value, null);

    public static SysdResult<T> Fail(SysdOutcome outcome, string? detail, SysdErrorCode? code = null) =>
        new(outcome, default, detail, code);

    public SysdResult<TOther> Cast<TOther>() => new(Outcome, default, Detail, ErrorCode);
}
