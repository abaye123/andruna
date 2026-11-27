using System.Buffers.Binary;
using System.IO.Pipes;
using System.Text;
using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Tests;

/// <summary>
/// Test-only stand-in for andruna-sysd: a real named pipe server speaking the
/// binary protocol of crates/andruna-ipc, so the production client code runs
/// unchanged against it. Each connection is served until the client closes
/// it, as sysd does.
/// </summary>
internal sealed class FakeSysd : IAsyncDisposable
{
    private readonly CancellationTokenSource _stop = new();
    private readonly Task _loop;
    private int _connections;

    /// <param name="handler">
    /// Maps (request id, request) to the raw response payload (header and
    /// body); null closes the connection without an answer.
    /// </param>
    public FakeSysd(Func<uint, SysdRequest, byte[]?> handler, string? pipeName = null)
    {
        PipeName = pipeName ?? "andruna-test-" + Guid.NewGuid().ToString("N");
        Handler = handler;
        _loop = Task.Run(RunAsync);
    }

    public string PipeName { get; }

    public Func<uint, SysdRequest, byte[]?> Handler { get; set; }

    /// <summary>Connections accepted so far.</summary>
    public int Connections => Volatile.Read(ref _connections);

    /// <summary>A sysd that answers every request like the real one with no packages installed.</summary>
    public static FakeSysd Running(params string[] packages) => new((id, req) => Answer(id, req, packages));

    /// <summary>The same as <see cref="Running"/> on a given pipe name.</summary>
    public static FakeSysd RunningOn(string pipeName) => new((id, req) => Answer(id, req, []), pipeName);

    public static byte[] Answer(uint id, SysdRequest req, IReadOnlyList<string> packages) => req.Kind switch
    {
        SysdWire.Kind.Ping => TestWire.Encode(new SysdResponse.Pong(SysdWire.ProtocolVersion), id),
        SysdWire.Kind.Status => TestWire.Encode(new SysdResponse.StatusReply(new SysdStatus(4321, 61_000, 1, 0, 30_000, "0.1.0")), id),
        SysdWire.Kind.ListPackages => TestWire.Encode(new SysdResponse.Packages(packages), id),
        SysdWire.Kind.StartPackage => TestWire.Encode(new SysdResponse.Error(SysdErrorCode.NotInstalled, $"package `{req.Package}` is not installed"), id),
        SysdWire.Kind.FocusWindow => TestWire.Encode(new SysdResponse.Error(SysdErrorCode.NotRunning, "not running"), id),
        _ => TestWire.Encode(new SysdResponse.Error(SysdErrorCode.UnknownRequest, "unknown request"), id),
    };

    public SysdClient Client() => new(PipeName, TimeSpan.FromSeconds(2), TimeSpan.FromSeconds(2));

    public async ValueTask DisposeAsync()
    {
        _stop.Cancel();
        try
        {
            await _loop;
        }
        catch (OperationCanceledException)
        {
        }
    }

    private async Task RunAsync()
    {
        var clients = new List<Task>();
        try
        {
            while (!_stop.IsCancellationRequested)
            {
                var server = new NamedPipeServerStream(
                    PipeName, PipeDirection.InOut, NamedPipeServerStream.MaxAllowedServerInstances,
                    PipeTransmissionMode.Byte, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
                try
                {
                    await server.WaitForConnectionAsync(_stop.Token);
                }
                catch (OperationCanceledException)
                {
                    await server.DisposeAsync();
                    return;
                }

                Interlocked.Increment(ref _connections);
                clients.Add(Task.Run(() => ServeAsync(server)));
            }
        }
        finally
        {
            await Task.WhenAll(clients);
        }
    }

    private async Task ServeAsync(NamedPipeServerStream server)
    {
        await using (server)
        {
            try
            {
                while (true)
                {
                    var payload = await SysdWire.ReadFrameAsync(server, _stop.Token);
                    var (id, request) = TestWire.DecodeRequest(payload);
                    var response = Handler(id, request);
                    if (response is null)
                    {
                        return;
                    }

                    await TestWire.WriteRawFrameAsync(server, response, _stop.Token);
                }
            }
            catch (Exception e) when (e is OperationCanceledException or IOException or SysdProtocolException)
            {
                // The client went away or the test is over.
            }
        }
    }
}

/// <summary>
/// Test-only encoder of responses and decoder of requests: the server half of
/// the codec, which the manager does not need. Checked against the golden
/// payloads of crates/andruna-ipc.
/// </summary>
internal static class TestWire
{
    public static byte[] Encode(SysdResponse response, uint id)
    {
        var body = new List<byte>();
        void U16(ushort v) => body.AddRange(BitConverter.GetBytes(v));
        void U32(uint v) => body.AddRange(BitConverter.GetBytes(v));
        void U64(ulong v) => body.AddRange(BitConverter.GetBytes(v));
        void Str(string s)
        {
            var b = Encoding.UTF8.GetBytes(s);
            U16((ushort)b.Length);
            body.AddRange(b);
        }

        ushort kind;
        switch (response)
        {
            case SysdResponse.Pong p:
                kind = SysdWire.Kind.Pong;
                U16(p.Protocol);
                break;
            case SysdResponse.StatusReply { Status: var s }:
                kind = SysdWire.Kind.StatusReply;
                U32(s.Pid);
                U64(s.UptimeMs);
                U32(s.Connections);
                U32(s.Hosts);
                U64(s.IdleTimeoutMs);
                Str(s.Version);
                break;
            case SysdResponse.Started s:
                kind = SysdWire.Kind.Started;
                U32(s.HostPid);
                body.Add(s.AlreadyRunning ? (byte)1 : (byte)0);
                break;
            case SysdResponse.Focused:
                kind = SysdWire.Kind.Focused;
                break;
            case SysdResponse.Packages p:
                kind = SysdWire.Kind.Packages;
                U32((uint)p.Names.Count);
                foreach (var n in p.Names)
                {
                    Str(n);
                }

                break;
            case SysdResponse.Error e:
                kind = SysdWire.Kind.Error;
                U16((ushort)e.Code);
                Str(e.Message);
                break;
            default:
                throw new ArgumentException(response.GetType().Name);
        }

        return Header(SysdWire.ProtocolVersion, kind, id).Concat(body).ToArray();
    }

    public static byte[] Header(ushort version, ushort kind, uint id)
    {
        var h = new byte[SysdWire.HeaderLength];
        BinaryPrimitives.WriteUInt16LittleEndian(h, version);
        BinaryPrimitives.WriteUInt16LittleEndian(h.AsSpan(2), kind);
        BinaryPrimitives.WriteUInt32LittleEndian(h.AsSpan(4), id);
        return h;
    }

    public static (uint Id, SysdRequest Request) DecodeRequest(byte[] payload)
    {
        var version = BinaryPrimitives.ReadUInt16LittleEndian(payload);
        var kind = BinaryPrimitives.ReadUInt16LittleEndian(payload.AsSpan(2));
        var id = BinaryPrimitives.ReadUInt32LittleEndian(payload.AsSpan(4));
        if (version != SysdWire.ProtocolVersion)
        {
            throw new SysdProtocolException($"version {version}");
        }

        string? package = null;
        if (kind is SysdWire.Kind.StartPackage or SysdWire.Kind.FocusWindow)
        {
            var n = BinaryPrimitives.ReadUInt16LittleEndian(payload.AsSpan(8));
            package = Encoding.UTF8.GetString(payload, 10, n);
            if (10 + n != payload.Length)
            {
                throw new SysdProtocolException("trailing bytes");
            }
        }
        else if (payload.Length != SysdWire.HeaderLength)
        {
            throw new SysdProtocolException("trailing bytes");
        }

        return (id, new SysdRequest(kind, package));
    }

    /// <summary>Writes a length prefix and payload without the client's range checks.</summary>
    public static async Task WriteRawFrameAsync(Stream stream, byte[] payload, CancellationToken ct)
    {
        var prefix = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(prefix, (uint)payload.Length);
        await stream.WriteAsync(prefix.Concat(payload).ToArray(), ct);
        await stream.FlushAsync(ct);
    }
}

/// <summary>Test-only service host.</summary>
internal sealed class FakeHost(bool installed, Func<bool>? start = null) : IServiceHost
{
    public int StartCalls { get; private set; }

    public bool IsInstalled => installed;

    public bool TryStart(out string? error)
    {
        StartCalls++;
        var ok = start?.Invoke() ?? false;
        error = ok ? null : "could not start";
        return ok;
    }
}

/// <summary>Test-only strings: the key itself, with arguments appended.</summary>
internal sealed class KeyStrings : IStrings
{
    public string Get(string key) => key;

    public string Format(string key, params object?[] args) => key + ":" + string.Join("|", args);
}
