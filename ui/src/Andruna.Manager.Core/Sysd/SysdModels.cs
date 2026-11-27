namespace Andruna.Manager.Core.Sysd;

/// <summary>Result of the <c>status</c> method.</summary>
public sealed record SysdStatus(string Version, int Protocol, long UptimeMs);

/// <summary>One entry of the <c>list_packages</c> result.</summary>
public sealed record PackageSummary(string Name, string? Label, string? VersionName, long VersionCode);

/// <summary>Body of the <c>list_packages</c> result.</summary>
public sealed record PackageList(IReadOnlyList<PackageSummary> Packages);

/// <summary>How a call to the service ended.</summary>
public enum SysdOutcome
{
    /// <summary>The service answered with a result.</summary>
    Ok,

    /// <summary>No service is listening on the pipe.</summary>
    NotRunning,

    /// <summary>The service answered, but not in the expected protocol.</summary>
    ProtocolError,

    /// <summary>The service answered with an error.</summary>
    ServiceError,
}

/// <summary>The value of a service call, or why there is none.</summary>
public sealed record SysdResult<T>(SysdOutcome Outcome, T? Value, string? Detail)
{
    public bool IsOk => Outcome == SysdOutcome.Ok;

    public static SysdResult<T> Ok(T value) => new(SysdOutcome.Ok, value, null);

    public static SysdResult<T> Fail(SysdOutcome outcome, string? detail) => new(outcome, default, detail);
}
