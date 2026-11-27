using System.IO.Pipes;
using System.Text.Json;

namespace Andruna.Manager.Core.Sysd;

/// <summary>The calls the management app makes to andruna-sysd.</summary>
public interface ISysdClient
{
    Task<SysdResult<bool>> PingAsync(CancellationToken ct = default);

    Task<SysdResult<SysdStatus>> GetStatusAsync(CancellationToken ct = default);

    Task<SysdResult<IReadOnlyList<PackageSummary>>> ListPackagesAsync(CancellationToken ct = default);
}

/// <summary>
/// Client for the sysd named pipe. Each call opens a connection, sends one
/// request, reads one response and closes the connection. The pipe is opened
/// with <see cref="PipeOptions.CurrentUserOnly"/>, so a pipe created by
/// another user is never trusted.
/// </summary>
public sealed class SysdClient : ISysdClient
{
    /// <summary>Pipe name used by sysd (<c>\\.\pipe\andruna-sysd</c>).</summary>
    public const string DefaultPipeName = "andruna-sysd";

    private readonly string _pipeName;
    private readonly TimeSpan _connectTimeout;
    private readonly TimeSpan _callTimeout;
    private long _nextId;

    public SysdClient(string pipeName = DefaultPipeName, TimeSpan? connectTimeout = null, TimeSpan? callTimeout = null)
    {
        _pipeName = pipeName;
        _connectTimeout = connectTimeout ?? TimeSpan.FromMilliseconds(500);
        _callTimeout = callTimeout ?? TimeSpan.FromSeconds(5);
    }

    public async Task<SysdResult<bool>> PingAsync(CancellationToken ct = default)
    {
        var result = await CallAsync("ping", ct).ConfigureAwait(false);
        return result.IsOk ? SysdResult<bool>.Ok(true) : SysdResult<bool>.Fail(result.Outcome, result.Detail);
    }

    public async Task<SysdResult<SysdStatus>> GetStatusAsync(CancellationToken ct = default)
    {
        var result = await CallAsync("status", ct).ConfigureAwait(false);
        return Decode<SysdStatus>(result, s => !string.IsNullOrEmpty(s.Version));
    }

    public async Task<SysdResult<IReadOnlyList<PackageSummary>>> ListPackagesAsync(CancellationToken ct = default)
    {
        var result = await CallAsync("list_packages", ct).ConfigureAwait(false);
        var list = Decode<PackageList>(result, l => l.Packages is not null && l.Packages.All(p => !string.IsNullOrEmpty(p.Name)));
        return list.IsOk
            ? SysdResult<IReadOnlyList<PackageSummary>>.Ok(list.Value!.Packages)
            : SysdResult<IReadOnlyList<PackageSummary>>.Fail(list.Outcome, list.Detail);
    }

    private static SysdResult<T> Decode<T>(SysdResult<JsonElement> raw, Func<T, bool> isValid)
        where T : class
    {
        if (!raw.IsOk)
        {
            return SysdResult<T>.Fail(raw.Outcome, raw.Detail);
        }

        try
        {
            var value = raw.Value.Deserialize<T>(SysdWire.JsonOptions);
            if (value is null || !isValid(value))
            {
                return SysdResult<T>.Fail(SysdOutcome.ProtocolError, $"the {typeof(T).Name} result is incomplete");
            }

            return SysdResult<T>.Ok(value);
        }
        catch (JsonException e)
        {
            return SysdResult<T>.Fail(SysdOutcome.ProtocolError, $"the {typeof(T).Name} result is malformed: {e.Message}");
        }
    }

    private async Task<SysdResult<JsonElement>> CallAsync(string method, CancellationToken ct)
    {
        using var pipe = new NamedPipeClientStream(
            ".",
            _pipeName,
            PipeDirection.InOut,
            PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);

        try
        {
            await pipe.ConnectAsync(_connectTimeout, ct).ConfigureAwait(false);
        }
        catch (TimeoutException)
        {
            return SysdResult<JsonElement>.Fail(SysdOutcome.NotRunning, "no service is listening on the pipe");
        }
        catch (IOException e)
        {
            return SysdResult<JsonElement>.Fail(SysdOutcome.NotRunning, e.Message);
        }
        catch (UnauthorizedAccessException e)
        {
            // The pipe exists but belongs to another user: not our service.
            return SysdResult<JsonElement>.Fail(SysdOutcome.NotRunning, e.Message);
        }

        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(ct);
        timeout.CancelAfter(_callTimeout);
        var id = Interlocked.Increment(ref _nextId);
        try
        {
            var request = JsonSerializer.SerializeToUtf8Bytes(new SysdRequest(id, method), SysdWire.JsonOptions);
            await SysdWire.WriteFrameAsync(pipe, request, timeout.Token).ConfigureAwait(false);
            var payload = await SysdWire.ReadFrameAsync(pipe, timeout.Token).ConfigureAwait(false);

            SysdResponse? response;
            try
            {
                response = JsonSerializer.Deserialize<SysdResponse>(payload, SysdWire.JsonOptions);
            }
            catch (JsonException e)
            {
                return SysdResult<JsonElement>.Fail(SysdOutcome.ProtocolError, $"the response is not valid JSON: {e.Message}");
            }

            if (response is null || response.Id != id)
            {
                return SysdResult<JsonElement>.Fail(SysdOutcome.ProtocolError, "the response does not match the request");
            }

            if (response.Error is { } error)
            {
                return SysdResult<JsonElement>.Fail(SysdOutcome.ServiceError, $"{error.Code}: {error.Message}");
            }

            if (response.Result is not { } result)
            {
                return SysdResult<JsonElement>.Fail(SysdOutcome.ProtocolError, "the response has neither result nor error");
            }

            return SysdResult<JsonElement>.Ok(result.Clone());
        }
        catch (SysdProtocolException e)
        {
            return SysdResult<JsonElement>.Fail(SysdOutcome.ProtocolError, e.Message);
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            return SysdResult<JsonElement>.Fail(SysdOutcome.ProtocolError, "the service did not answer in time");
        }
        catch (IOException e)
        {
            return SysdResult<JsonElement>.Fail(SysdOutcome.ProtocolError, $"the connection failed: {e.Message}");
        }
    }
}
