using System.Diagnostics;

namespace Andruna.Manager.Core.Sysd;

/// <summary>State of the Andruna service as the management app sees it.</summary>
public enum ServiceState
{
    /// <summary>andruna-sysd.exe is not part of this installation.</summary>
    NotInstalled,

    /// <summary>andruna-sysd.exe is installed but does not answer.</summary>
    NotRunning,

    /// <summary>The pipe is served by a process of another user; it is not trusted.</summary>
    Untrusted,

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
            // sysd is single-instance per user and session; if another client
            // started it a moment ago this one exits with "already running"
            // and the connection retry finds the winner.
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
public sealed record ServiceSnapshot(ServiceState State, SysdStatus? Status, string? Detail, SysdErrorCode? ErrorCode = null);

/// <summary>
/// The manager's link to sysd (sysd starts with the manager, see
/// 02-architecture.md). Asks for the status over the kept connection; when
/// sysd is installed but not running, starts it and retries the connection
/// for a short time. <see cref="StartKeepAlive"/> checks periodically so that
/// a connection lost while the manager is open is opened again.
/// </summary>
public sealed class ServiceConnection(ISysdClient client, IServiceHost host, TimeSpan? startWait = null, TimeSpan? startCooldown = null)
{
    private readonly TimeSpan _startWait = startWait ?? TimeSpan.FromSeconds(3);
    private readonly TimeSpan _startCooldown = startCooldown ?? TimeSpan.FromSeconds(10);
    private readonly SemaphoreSlim _checkLock = new(1, 1);
    private DateTime? _lastStart;
    private CancellationTokenSource? _keepAlive;

    public ISysdClient Client => client;

    public IServiceHost Host => host;

    /// <summary>The result of the last check, or null before the first.</summary>
    public ServiceSnapshot? Last { get; private set; }

    public async Task<ServiceSnapshot> CheckAsync(CancellationToken ct = default)
    {
        await _checkLock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            Last = await CheckLockedAsync(ct).ConfigureAwait(false);
            return Last;
        }
        finally
        {
            _checkLock.Release();
        }
    }

    /// <summary>Checks the service every <paramref name="interval"/> until <see cref="StopKeepAlive"/>.</summary>
    public void StartKeepAlive(TimeSpan interval)
    {
        StopKeepAlive();
        var cts = new CancellationTokenSource();
        _keepAlive = cts;
        _ = Task.Run(async () =>
        {
            using var timer = new PeriodicTimer(interval);
            try
            {
                while (await timer.WaitForNextTickAsync(cts.Token).ConfigureAwait(false))
                {
                    await CheckAsync(cts.Token).ConfigureAwait(false);
                }
            }
            catch (OperationCanceledException)
            {
            }
        });
    }

    public void StopKeepAlive()
    {
        _keepAlive?.Cancel();
        _keepAlive = null;
    }

    private async Task<ServiceSnapshot> CheckLockedAsync(CancellationToken ct)
    {
        var status = await client.GetStatusAsync(ct).ConfigureAwait(false);
        if (status.Outcome == SysdOutcome.NotRunning && host.IsInstalled &&
            (_lastStart is null || DateTime.UtcNow - _lastStart.Value >= _startCooldown))
        {
            _lastStart = DateTime.UtcNow;
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
            SysdOutcome.WrongServerUser => new ServiceSnapshot(ServiceState.Untrusted, null, status.Detail),
            _ => new ServiceSnapshot(ServiceState.Faulted, null, status.Detail, status.ErrorCode),
        };
    }
}
