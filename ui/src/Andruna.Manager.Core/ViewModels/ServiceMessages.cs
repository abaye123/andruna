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
        _ => new UserMessage(
            strings.Get("ServiceFaultedTitle"), strings.Format("ServiceFaultedMessage", snapshot.Detail ?? string.Empty)),
    };

    /// <summary>The same as <see cref="For"/> for a failed call result.</summary>
    public static UserMessage ForOutcome(SysdOutcome outcome, string? detail, bool installed, IStrings strings) =>
        For(new ServiceSnapshot(
            outcome switch
            {
                SysdOutcome.NotRunning => installed ? ServiceState.NotRunning : ServiceState.NotInstalled,
                _ => ServiceState.Faulted,
            },
            null,
            detail), strings)!;
}
