using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>A user-facing title and explanation (what happened, why, what to do).</summary>
public sealed record UserMessage(string Title, string Message);

public static class ServiceMessages
{
    /// <summary>The problem to show for a snapshot, or null when the service is running.</summary>
    public static UserMessage? For(ServiceSnapshot snapshot, IStrings strings) => snapshot.State switch
    {
        ServiceState.Running => null,
        ServiceState.NotInstalled => new UserMessage(
            strings.Get("ServiceNotInstalledTitle"), strings.Get("ServiceNotInstalledMessage")),
        ServiceState.NotRunning => new UserMessage(
            strings.Get("ServiceNotRunningTitle"), strings.Get("ServiceNotRunningMessage")),
        ServiceState.Untrusted => new UserMessage(
            strings.Get("ServiceUntrustedTitle"), strings.Get("ServiceUntrustedMessage")),
        _ when snapshot.ErrorCode is { } code => new UserMessage(
            strings.Get("ServiceErrorTitle"), ForError(code, strings)),
        _ => new UserMessage(
            strings.Get("ServiceFaultedTitle"), strings.Format("ServiceFaultedMessage", snapshot.Detail ?? string.Empty)),
    };

    /// <summary>The same as <see cref="For"/> for a failed call result.</summary>
    public static UserMessage ForResult<T>(SysdResult<T> result, bool installed, IStrings strings) =>
        For(new ServiceSnapshot(
            result.Outcome switch
            {
                SysdOutcome.NotRunning => installed ? ServiceState.NotRunning : ServiceState.NotInstalled,
                SysdOutcome.WrongServerUser => ServiceState.Untrusted,
                _ => ServiceState.Faulted,
            },
            null,
            result.Detail,
            result.ErrorCode), strings)!;

    /// <summary>What an Error response code means, in the user's language.</summary>
    public static string ForError(SysdErrorCode code, IStrings strings) => code switch
    {
        SysdErrorCode.UnsupportedVersion => strings.Get("SysdErrorUnsupportedVersion"),
        SysdErrorCode.Malformed => strings.Get("SysdErrorMalformed"),
        SysdErrorCode.UnknownRequest => strings.Get("SysdErrorUnknownRequest"),
        SysdErrorCode.InvalidPackageName => strings.Get("SysdErrorInvalidPackageName"),
        SysdErrorCode.NotInstalled => strings.Get("SysdErrorNotInstalled"),
        SysdErrorCode.NotRunning => strings.Get("SysdErrorNotRunning"),
        SysdErrorCode.NoWindow => strings.Get("SysdErrorNoWindow"),
        SysdErrorCode.HostStartFailed => strings.Get("SysdErrorHostStartFailed"),
        SysdErrorCode.Busy => strings.Get("SysdErrorBusy"),
        SysdErrorCode.Internal => strings.Get("SysdErrorInternal"),
        SysdErrorCode.InvalidPackage => strings.Get("SysdErrorInvalidPackage"),
        SysdErrorCode.InvalidSignature => strings.Get("SysdErrorInvalidSignature"),
        SysdErrorCode.SignerMismatch => strings.Get("SysdErrorSignerMismatch"),
        SysdErrorCode.IncompatibleAbi => strings.Get("SysdErrorIncompatibleAbi"),
        SysdErrorCode.SdkTooNew => strings.Get("SysdErrorSdkTooNew"),
        SysdErrorCode.Downgrade => strings.Get("SysdErrorDowngrade"),
        SysdErrorCode.InvalidSplits => strings.Get("SysdErrorInvalidSplits"),
        SysdErrorCode.InstallFailed => strings.Get("SysdErrorInstallFailed"),
        SysdErrorCode.NotPermitted => strings.Get("SysdErrorNotPermitted"),
        _ => strings.Format("SysdErrorOther", (ushort)code),
    };
}
