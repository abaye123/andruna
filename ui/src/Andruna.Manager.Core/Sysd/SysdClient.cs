using System.IO.Pipes;
using System.Security.Principal;

namespace Andruna.Manager.Core.Sysd;

/// <summary>The calls the management app makes to andruna-sysd.</summary>
public interface ISysdClient
{
    Task<SysdResult<ushort>> PingAsync(CancellationToken ct = default);

    Task<SysdResult<SysdStatus>> GetStatusAsync(CancellationToken ct = default);

    Task<SysdResult<IReadOnlyList<PackageSummary>>> ListPackagesAsync(CancellationToken ct = default);

    Task<SysdResult<SysdResponse.Started>> StartPackageAsync(string package, CancellationToken ct = default);

    Task<SysdResult<bool>> FocusWindowAsync(string package, CancellationToken ct = default);
}

/// <summary>
/// Client for the sysd pipe protocol (crates/andruna-ipc). One connection is
/// opened on the first call and kept open while the client lives: an open
/// connection keeps sysd from exiting on idle. Calls are serialized on it.
/// When the connection breaks, the next call opens a new one.
/// </summary>
/// <remarks>
/// Security, as the protocol requires: the pipe is opened with
/// identification-level impersonation (sysd cannot act as this user), and
/// before anything is sent the pipe must be owned by this user (its SID, or
/// the token's default owner) and the server process is checked to run as
/// the same user (<c>GetNamedPipeServerProcessId</c>).
/// <see cref="PipeOptions.CurrentUserOnly"/> is not used: it accepts only the
/// token's default owner, which in an elevated process is
/// BUILTIN\Administrators, while sysd names the user SID as the owner, so an
/// elevated manager would reject its own sysd.
/// </remarks>
public sealed class SysdClient : ISysdClient, IDisposable
{
    private readonly string _pipeName;
    private readonly TimeSpan _connectTimeout;
    private readonly TimeSpan _callTimeout;
    private readonly SemaphoreSlim _lock = new(1, 1);
    private readonly SecurityIdentifier _user = SysdEndpoint.CurrentUserSid();
    private readonly SecurityIdentifier? _defaultOwner = SysdEndpoint.CurrentDefaultOwner();
    private NamedPipeClientStream? _pipe;
    private uint _nextId;

    /// <param name="pipeName">Pipe name without <c>\\.\pipe\</c>; see <see cref="SysdEndpoint"/>.</param>
    public SysdClient(string pipeName, TimeSpan? connectTimeout = null, TimeSpan? callTimeout = null)
    {
        _pipeName = pipeName;
        _connectTimeout = connectTimeout ?? TimeSpan.FromMilliseconds(500);
        _callTimeout = callTimeout ?? TimeSpan.FromSeconds(5);
    }

    /// <summary>The client of the product sysd of this user and session.</summary>
    public static SysdClient ForCurrentUser() => new(SysdEndpoint.PipeNameForCurrentUser());

    public string PipeName => _pipeName;

    /// <summary>True while a connection is open.</summary>
    public bool IsConnected => _pipe?.IsConnected == true;

    /// <summary>Process id of the sysd serving the open connection, or 0.</summary>
    public uint ServerPid { get; private set; }

    public async Task<SysdResult<ushort>> PingAsync(CancellationToken ct = default)
    {
        var r = await RequestAsync(SysdRequest.Ping, ct).ConfigureAwait(false);
        return r.Value is SysdResponse.Pong pong ? SysdResult<ushort>.Ok(pong.Protocol) : Unexpected<ushort>(r);
    }

    public async Task<SysdResult<SysdStatus>> GetStatusAsync(CancellationToken ct = default)
    {
        var r = await RequestAsync(SysdRequest.Status, ct).ConfigureAwait(false);
        return r.Value is SysdResponse.StatusReply s ? SysdResult<SysdStatus>.Ok(s.Status) : Unexpected<SysdStatus>(r);
    }

    public async Task<SysdResult<IReadOnlyList<PackageSummary>>> ListPackagesAsync(CancellationToken ct = default)
    {
        var r = await RequestAsync(SysdRequest.ListPackages, ct).ConfigureAwait(false);
        return r.Value is SysdResponse.Packages p
            ? SysdResult<IReadOnlyList<PackageSummary>>.Ok(p.Names.Select(n => new PackageSummary(n)).ToList())
            : Unexpected<IReadOnlyList<PackageSummary>>(r);
    }

    public async Task<SysdResult<SysdResponse.Started>> StartPackageAsync(string package, CancellationToken ct = default)
    {
        var r = await RequestAsync(SysdRequest.StartPackage(package), ct).ConfigureAwait(false);
        return r.Value is SysdResponse.Started s ? SysdResult<SysdResponse.Started>.Ok(s) : Unexpected<SysdResponse.Started>(r);
    }

    public async Task<SysdResult<bool>> FocusWindowAsync(string package, CancellationToken ct = default)
    {
        var r = await RequestAsync(SysdRequest.FocusWindow(package), ct).ConfigureAwait(false);
        return r.Value is SysdResponse.Focused ? SysdResult<bool>.Ok(true) : Unexpected<bool>(r);
    }

    /// <summary>
    /// Sends one request and returns the decoded response. An Error response
    /// becomes <see cref="SysdOutcome.ServiceError"/> with its code.
    /// </summary>
    public async Task<SysdResult<SysdResponse>> RequestAsync(SysdRequest request, CancellationToken ct = default)
    {
        await _lock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            // A kept connection may have been closed by sysd (idle exit,
            // restart); then one fresh connection is tried, as the launcher does.
            var reused = _pipe is not null;
            var (result, closed) = await RequestOnceAsync(request, ct).ConfigureAwait(false);
            if (reused && closed)
            {
                (result, _) = await RequestOnceAsync(request, ct).ConfigureAwait(false);
            }

            return result;
        }
        finally
        {
            _lock.Release();
        }
    }

    /// <summary>Closes the connection. The next call opens a new one.</summary>
    public void Disconnect()
    {
        _lock.Wait();
        try
        {
            Drop();
        }
        finally
        {
            _lock.Release();
        }
    }

    public void Dispose()
    {
        Disconnect();
        _lock.Dispose();
    }

    /// <summary>One attempt; <c>Closed</c> is true when the connection closed without an answer.</summary>
    private async Task<(SysdResult<SysdResponse> Result, bool Closed)> RequestOnceAsync(SysdRequest request, CancellationToken ct)
    {
        if (_pipe is null)
        {
            var failure = await ConnectAsync(ct).ConfigureAwait(false);
            if (failure is not null)
            {
                return (failure, false);
            }
        }

        var pipe = _pipe!;
        var id = unchecked(++_nextId);
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(ct);
        timeout.CancelAfter(_callTimeout);
        try
        {
            byte[] payload;
            try
            {
                await SysdWire.WriteFrameAsync(pipe, SysdWire.EncodeRequest(request, id), timeout.Token).ConfigureAwait(false);
                payload = await SysdWire.ReadFrameAsync(pipe, timeout.Token).ConfigureAwait(false);
            }
            catch (Exception e) when (e is IOException or ObjectDisposedException or SysdConnectionClosedException { BeforeFrame: true })
            {
                Drop();
                return (SysdResult<SysdResponse>.Fail(SysdOutcome.ProtocolError, $"the connection to the service closed: {e.Message}"), true);
            }

            var (gotId, response) = SysdWire.DecodeResponse(payload);
            if (gotId != id)
            {
                Drop();
                return (SysdResult<SysdResponse>.Fail(SysdOutcome.ProtocolError, $"answer for request {gotId} received while waiting for {id}"), false);
            }

            if (response is SysdResponse.Error error)
            {
                // After these the server closes the connection.
                if (error.Code is SysdErrorCode.UnsupportedVersion or SysdErrorCode.Malformed)
                {
                    Drop();
                }

                return (SysdResult<SysdResponse>.Fail(SysdOutcome.ServiceError, $"{error.Code}: {error.Message}", error.Code), false);
            }

            return (SysdResult<SysdResponse>.Ok(response), false);
        }
        catch (SysdProtocolException e)
        {
            Drop();
            return (SysdResult<SysdResponse>.Fail(SysdOutcome.ProtocolError, e.Message), false);
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            Drop();
            return (SysdResult<SysdResponse>.Fail(SysdOutcome.ProtocolError, "the service did not answer in time"), false);
        }
        catch (OperationCanceledException)
        {
            // The caller cancelled mid-request: the answer may still arrive on
            // this connection and would be read as the answer to the next
            // request, so the connection is dropped before cancelling.
            Drop();
            throw;
        }
    }

    /// <summary>Opens and checks a connection; returns null on success, else the failure.</summary>
    private async Task<SysdResult<SysdResponse>?> ConnectAsync(CancellationToken ct)
    {
        var pipe = new NamedPipeClientStream(
            ".",
            _pipeName,
            PipeDirection.InOut,
            PipeOptions.Asynchronous,
            TokenImpersonationLevel.Identification);
        try
        {
            await pipe.ConnectAsync(_connectTimeout, ct).ConfigureAwait(false);
        }
        catch (TimeoutException)
        {
            pipe.Dispose();
            return SysdResult<SysdResponse>.Fail(SysdOutcome.NotRunning, "no service is listening on the pipe");
        }
        catch (IOException e)
        {
            pipe.Dispose();
            return SysdResult<SysdResponse>.Fail(SysdOutcome.NotRunning, e.Message);
        }
        catch (UnauthorizedAccessException e)
        {
            // The pipe exists but is not owned by this user: not our service.
            pipe.Dispose();
            return SysdResult<SysdResponse>.Fail(SysdOutcome.WrongServerUser, e.Message);
        }

        IdentityReference? owner;
        try
        {
            owner = pipe.GetAccessControl().GetOwner(typeof(SecurityIdentifier));
        }
        catch (Exception e) when (e is UnauthorizedAccessException or InvalidOperationException or IOException)
        {
            pipe.Dispose();
            return SysdResult<SysdResponse>.Fail(SysdOutcome.WrongServerUser, $"the owner of the pipe cannot be read: {e.Message}");
        }

        if (owner is not SecurityIdentifier ownerSid || (ownerSid != _user && ownerSid != _defaultOwner))
        {
            pipe.Dispose();
            return SysdResult<SysdResponse>.Fail(SysdOutcome.WrongServerUser, "the pipe is not owned by the current user");
        }

        uint serverPid;
        SecurityIdentifier? serverUser;
        try
        {
            serverPid = SysdEndpoint.ServerProcessId(pipe.SafePipeHandle);
            serverUser = SysdEndpoint.ProcessUser(serverPid);
        }
        catch (System.ComponentModel.Win32Exception e)
        {
            pipe.Dispose();
            return SysdResult<SysdResponse>.Fail(SysdOutcome.WrongServerUser, $"the server process cannot be identified: {e.Message}");
        }

        if (serverUser != _user)
        {
            pipe.Dispose();
            return SysdResult<SysdResponse>.Fail(
                SysdOutcome.WrongServerUser,
                $"the pipe is served by process {serverPid}, which runs as another user");
        }

        _pipe = pipe;
        ServerPid = serverPid;
        return null;
    }

    private void Drop()
    {
        _pipe?.Dispose();
        _pipe = null;
        ServerPid = 0;
    }

    private static SysdResult<T> Unexpected<T>(SysdResult<SysdResponse> r) => r.IsOk
        ? SysdResult<T>.Fail(SysdOutcome.ProtocolError, $"unexpected response {r.Value!.GetType().Name}")
        : r.Cast<T>();
}
