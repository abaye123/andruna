using System.Diagnostics;

namespace Andruna.Manager.Core.Sysd;

/// <summary>State of the Andruna service as the management app sees it.</summary>
public enum ServiceState
{
    /// <summary>andruna-sysd.exe is not part of this installation.</summary>
    NotInstalled,

    /// <summary>andruna-sysd.exe is installed but does not answer.</summary>
    NotRunning,

    /// <summary>The service answers, but with an error or in an unexpected protocol.</summary>
    Faulted,

    /// <summary>The service answers.</summary>
    Running,
}

/// <summary>Where andruna-sysd.exe lives and how it is started.</summary>
public interface IServiceHost
{
    bool IsInstalled { get; }

    /// <summary>Starts the service process. Returns false if it could not be started.</summary>
    bool TryStart(out string? error);
}

/// <summary>
/// The real service host: andruna-sysd.exe next to Andruna.Manager.exe, as the
/// installer lays it out in %LOCALAPPDATA%\Programs\Andruna.
/// </summary>
public sealed class InstalledServiceHost(string directory) : IServiceHost
{
    public const string ExecutableName = "andruna-sysd.exe";

    public string ExecutablePath { get; } = Path.Combine(directory, ExecutableName);

    public bool IsInstalled => File.Exists(ExecutablePath);

    public bool TryStart(out string? error)
    {
        try
        {
            using var process = Process.Start(new ProcessStartInfo(ExecutablePath)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                WorkingDirectory = Path.GetDirectoryName(ExecutablePath),
            });
            error = process is null ? "the process did not start" : null;
            return process is not null;
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException or IOException)
        {
            error = e.Message;
            return false;
        }
    }
}

/// <summary>Snapshot of the service state with the reason for it.</summary>
public sealed record ServiceSnapshot(ServiceState State, SysdStatus? Status, string? Detail);

/// <summary>
/// Finds out whether sysd is running and starts it once if it is installed
/// but not running (sysd starts with the manager, see 02-architecture.md).
/// </summary>
public sealed class ServiceConnection(ISysdClient client, IServiceHost host, TimeSpan? startWait = null)
{
    private readonly TimeSpan _startWait = startWait ?? TimeSpan.FromSeconds(3);
    private bool _startAttempted;

    public ISysdClient Client => client;

    public IServiceHost Host => host;

    public async Task<ServiceSnapshot> CheckAsync(CancellationToken ct = default)
    {
        var status = await client.GetStatusAsync(ct).ConfigureAwait(false);
        if (status.Outcome == SysdOutcome.NotRunning && host.IsInstalled && !_startAttempted)
        {
            _startAttempted = true;
            if (!host.TryStart(out var error))
            {
                return new ServiceSnapshot(ServiceState.NotRunning, null, error);
            }

            var deadline = DateTime.UtcNow + _startWait;
            do
            {
                await Task.Delay(100, ct).ConfigureAwait(false);
                status = await client.GetStatusAsync(ct).ConfigureAwait(false);
            }
            while (status.Outcome == SysdOutcome.NotRunning && DateTime.UtcNow < deadline);
        }

        return status.Outcome switch
        {
            SysdOutcome.Ok => new ServiceSnapshot(ServiceState.Running, status.Value, null),
            SysdOutcome.NotRunning => new ServiceSnapshot(
                host.IsInstalled ? ServiceState.NotRunning : ServiceState.NotInstalled, null, status.Detail),
            _ => new ServiceSnapshot(ServiceState.Faulted, null, status.Detail),
        };
    }
}
