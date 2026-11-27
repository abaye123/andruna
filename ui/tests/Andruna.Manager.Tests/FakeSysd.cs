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
        // The first pipe instance exists before the constructor returns, so
        // a client made right after never races the accept loop, which may
        // start late on a busy thread pool.
        var first = CreateInstance();
        _loop = Task.Run(() => RunAsync(first));
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

    private NamedPipeServerStream CreateInstance() => new(
        PipeName, PipeDirection.InOut, NamedPipeServerStream.MaxAllowedServerInstances,
        PipeTransmissionMode.Byte, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);

    private async Task RunAsync(NamedPipeServerStream first)
    {
        var clients = new List<Task>();
        var next = first;
        try
        {
            while (!_stop.IsCancellationRequested)
            {
                var server = next ?? CreateInstance();
                next = null;
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
            next?.Dispose();
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

        void Strs(IReadOnlyList<string> list)
        {
            U32((uint)list.Count);
            foreach (var s in list)
            {
                Str(s);
            }
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
            case SysdResponse.Details { Value: var d }:
                kind = SysdWire.Kind.PackageDetails;
                Str(d.Package);
                U64(d.VersionCode);
                Str(d.VersionName);
                Str(d.Label);
                U32(d.MinSdk);
                U32(d.TargetSdk);
                Str(d.SignerSha256);
                body.Add(d.SignatureScheme);
                Strs(d.Permissions);
                Strs(d.NativeAbis);
                Str(d.SelectedAbi);
                U64(d.InstalledVersionCode);
                Str(d.IconPngPath);
                break;
            case SysdResponse.Installed { Value: var o }:
                kind = SysdWire.Kind.Installed;
                Str(o.Package);
                U64(o.VersionCode);
                Str(o.VersionName);
                body.Add(o.Updated ? (byte)1 : (byte)0);
                body.Add((byte)o.Compile);
                Str(o.CompileMessage);
                U32(o.Shortcuts);
                break;
            case SysdResponse.Uninstalled:
                kind = SysdWire.Kind.Uninstalled;
                break;
            case SysdResponse.PackageInfos { Value: var infos }:
                kind = SysdWire.Kind.PackageInfos;
                U32((uint)infos.Count);
                foreach (var i in infos)
                {
                    Str(i.Package);
                    Str(i.Label);
                    U64(i.VersionCode);
                    Str(i.VersionName);
                    Str(i.IconIcoPath);
                    Str(i.IconPngPath);
                    Strs(i.LauncherActivities);
                    U64(i.InstalledMs);
                    U64(i.SizeBytes);
                    Str(i.AppUserModelId);
                }

                break;
            case SysdResponse.Events { Value: var batch }:
                kind = SysdWire.Kind.Events;
                U64(batch.Epoch);
                U64(batch.Next);
                body.Add(batch.Missed ? (byte)1 : (byte)0);
                U32((uint)batch.Events.Count);
                foreach (var e in batch.Events)
                {
                    var outer = body;
                    body = [];
                    ushort type;
                    switch (e)
                    {
                        case SysdEvent.NotificationPosted n:
                            type = SysdWire.EventType.NotificationPosted;
                            Str(n.Package);
                            U32((uint)n.Id);
                            Str(n.Tag);
                            Str(n.Channel);
                            Str(n.Title);
                            Str(n.Text);
                            break;
                        case SysdEvent.NotificationRemoved r:
                            type = SysdWire.EventType.NotificationRemoved;
                            Str(r.Package);
                            U32((uint)r.Id);
                            Str(r.Tag);
                            break;
                        case SysdEvent.AppCrashed c:
                            type = SysdWire.EventType.AppCrashed;
                            Str(c.Package);
                            U32(c.Pid);
                            U32(c.ExitCode);
                            Str(c.Reason);
                            break;
                        case SysdEvent.Unknown u:
                            type = u.Type;
                            break;
                        default:
                            throw new ArgumentException(e.GetType().Name);
                    }

                    var eventBody = body;
                    body = outer;
                    U64(e.Seq);
                    U64(e.TimeMs);
                    U16(type);
                    U32((uint)eventBody.Count);
                    body.AddRange(eventBody);
                }

                break;
            case SysdResponse.MissingFunctions { Value: var m }:
                kind = SysdWire.Kind.MissingFunctions;
                U64(m.UpdatedMs);
                U32((uint)m.Entries.Count);
                foreach (var e in m.Entries)
                {
                    Str(e.Name);
                    U64(e.Calls);
                }

                break;
            case SysdResponse.Reported:
                kind = SysdWire.Kind.Reported;
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

        var at = SysdWire.HeaderLength;
        string Str()
        {
            var n = BinaryPrimitives.ReadUInt16LittleEndian(payload.AsSpan(at));
            var s = Encoding.UTF8.GetString(payload, at + 2, n);
            at += 2 + n;
            return s;
        }

        uint U32()
        {
            var v = BinaryPrimitives.ReadUInt32LittleEndian(payload.AsSpan(at));
            at += 4;
            return v;
        }

        ulong U64()
        {
            var v = BinaryPrimitives.ReadUInt64LittleEndian(payload.AsSpan(at));
            at += 8;
            return v;
        }

        List<string> Strs()
        {
            var n = U32();
            var list = new List<string>();
            for (var i = 0; i < n; i++)
            {
                list.Add(Str());
            }

            return list;
        }

        var request = kind switch
        {
            SysdWire.Kind.StartPackage or SysdWire.Kind.FocusWindow or SysdWire.Kind.GetMissingFunctions => new SysdRequest(kind, Str()),
            SysdWire.Kind.StartActivity => new SysdRequest(kind, Str()) { Activity = Str() },
            SysdWire.Kind.InspectPackage => new SysdRequest(kind) { Paths = Strs(), Locale = Str() },
            SysdWire.Kind.InstallPackage => new SysdRequest(kind) { Paths = Strs() },
            SysdWire.Kind.UninstallPackage => new SysdRequest(kind, Str()) { KeepData = payload[at++] == 1 },
            SysdWire.Kind.ListPackageInfo => new SysdRequest(kind) { Locale = Str() },
            SysdWire.Kind.WaitEvents => new SysdRequest(kind) { Epoch = U64(), After = U64(), TimeoutMs = U32() },
            SysdWire.Kind.ReportMissingFunctions => new SysdRequest(kind)
            {
                Entries = Enumerable.Range(0, (int)U32()).Select(_ => new MissingFunction(Str(), U64())).ToList(),
            },
            _ => new SysdRequest(kind),
        };
        if (at != payload.Length)
        {
            throw new SysdProtocolException("trailing bytes");
        }

        return (id, request);
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
