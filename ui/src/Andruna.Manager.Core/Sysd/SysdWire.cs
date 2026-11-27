using System.Buffers.Binary;
using System.Text;

namespace Andruna.Manager.Core.Sysd;

/// <summary>
/// The sysd pipe protocol, version 1, as defined by crates/andruna-ipc
/// (README.md and src/proto.rs). Every message is one frame:
/// <code>
/// u32 LE  payload length (header + body), 8 ..= 1 MiB
/// u16 LE  protocol version (1)
/// u16 LE  kind
/// u32 LE  request id (a response echoes the id of its request)
/// ...     body
/// </code>
/// Body fields are little-endian integers, one-byte booleans (0 or 1) and
/// strings as a u16 LE byte length followed by UTF-8. Trailing bytes are an
/// error. Decoding never throws anything but <see cref="SysdProtocolException"/>.
/// </summary>
public static class SysdWire
{
    /// <summary>Protocol version this client speaks.</summary>
    public const ushort ProtocolVersion = 1;

    /// <summary>Size of the header that starts every payload.</summary>
    public const int HeaderLength = 8;

    /// <summary>Largest accepted payload (header + body) in bytes.</summary>
    public const int MaxPayloadBytes = 1 << 20;

    /// <summary>Message kinds.</summary>
    public static class Kind
    {
        public const ushort Ping = 0x0001;
        public const ushort Status = 0x0002;
        public const ushort StartPackage = 0x0003;
        public const ushort FocusWindow = 0x0004;
        public const ushort ListPackages = 0x0005;

        // Package manager (0x0006..=0x000A).
        public const ushort InspectPackage = 0x0006;
        public const ushort InstallPackage = 0x0007;
        public const ushort UninstallPackage = 0x0008;
        public const ushort ListPackageInfo = 0x0009;
        public const ushort StartActivity = 0x000A;

        // Events and diagnostics (0x0010..=0x001F).
        public const ushort WaitEvents = 0x0010;
        public const ushort GetMissingFunctions = 0x0011;
        public const ushort ReportMissingFunctions = 0x0012;

        public const ushort Pong = 0x8001;
        public const ushort StatusReply = 0x8002;
        public const ushort Started = 0x8003;
        public const ushort Focused = 0x8004;
        public const ushort Packages = 0x8005;
        public const ushort PackageDetails = 0x8006;
        public const ushort Installed = 0x8007;
        public const ushort Uninstalled = 0x8008;
        public const ushort PackageInfos = 0x8009;
        public const ushort Events = 0x8010;
        public const ushort MissingFunctions = 0x8011;
        public const ushort Reported = 0x8012;
        public const ushort Error = 0x80FF;
    }

    /// <summary>Event types inside an Events response.</summary>
    public static class EventType
    {
        public const ushort NotificationPosted = 1;
        public const ushort NotificationRemoved = 2;
        public const ushort AppCrashed = 3;
    }

    /// <summary>Encodes a request payload (without the length prefix).</summary>
    public static byte[] EncodeRequest(SysdRequest request, uint id)
    {
        var w = new Writer(request.Kind, id);
        switch (request.Kind)
        {
            case Kind.StartPackage or Kind.FocusWindow or Kind.GetMissingFunctions:
                w.Str(request.Package ?? string.Empty);
                break;
            case Kind.StartActivity:
                w.Str(request.Package ?? string.Empty);
                w.Str(request.Activity ?? string.Empty);
                break;
            case Kind.InspectPackage:
                w.Strs(request.Paths ?? []);
                w.Str(request.Locale ?? string.Empty);
                break;
            case Kind.InstallPackage:
                w.Strs(request.Paths ?? []);
                break;
            case Kind.UninstallPackage:
                w.Str(request.Package ?? string.Empty);
                w.U8(request.KeepData ? (byte)1 : (byte)0);
                break;
            case Kind.ListPackageInfo:
                w.Str(request.Locale ?? string.Empty);
                break;
            case Kind.WaitEvents:
                w.U64(request.Epoch);
                w.U64(request.After);
                w.U32(request.TimeoutMs);
                break;
            case Kind.ReportMissingFunctions:
                var entries = request.Entries ?? [];
                w.U32((uint)entries.Count);
                foreach (var e in entries)
                {
                    w.Str(e.Name);
                    w.U64(e.Calls);
                }

                break;
        }

        return w.ToArray();
    }

    /// <summary>Decodes a response payload and returns the request id it answers.</summary>
    public static (uint Id, SysdResponse Response) DecodeResponse(ReadOnlySpan<byte> payload)
    {
        var r = new Reader(payload);
        var version = r.U16();
        var kind = r.U16();
        var id = r.U32();
        if (version != ProtocolVersion)
        {
            throw new SysdProtocolException($"unsupported protocol version {version}");
        }

        SysdResponse response = kind switch
        {
            Kind.Pong => new SysdResponse.Pong(r.U16()),
            Kind.StatusReply => new SysdResponse.StatusReply(new SysdStatus(
                Pid: r.U32(),
                UptimeMs: r.U64(),
                Connections: r.U32(),
                Hosts: r.U32(),
                IdleTimeoutMs: r.U64(),
                Version: r.Str())),
            Kind.Started => new SysdResponse.Started(r.U32(), r.Bool()),
            Kind.Focused => new SysdResponse.Focused(),
            Kind.Packages => new SysdResponse.Packages(r.StrList()),
            Kind.PackageDetails => new SysdResponse.Details(new PackageDetails(
                Package: r.Str(),
                VersionCode: r.U64(),
                VersionName: r.Str(),
                Label: r.Str(),
                MinSdk: r.U32(),
                TargetSdk: r.U32(),
                SignerSha256: r.Str(),
                SignatureScheme: r.U8(),
                Permissions: r.StrList(),
                NativeAbis: r.StrList(),
                SelectedAbi: r.Str(),
                InstalledVersionCode: r.U64(),
                IconPngPath: r.Str())),
            Kind.Installed => new SysdResponse.Installed(new InstallOutcome(
                Package: r.Str(),
                VersionCode: r.U64(),
                VersionName: r.Str(),
                Updated: r.Bool(),
                Compile: (CompileStatus)r.U8(),
                CompileMessage: r.Str(),
                Shortcuts: r.U32())),
            Kind.Uninstalled => new SysdResponse.Uninstalled(),
            Kind.PackageInfos => new SysdResponse.PackageInfos(r.PackageInfos()),
            Kind.Events => new SysdResponse.Events(r.EventBatch()),
            Kind.MissingFunctions => new SysdResponse.MissingFunctions(new MissingFunctionReport(r.U64(), r.MissingFunctions())),
            Kind.Reported => new SysdResponse.Reported(),
            Kind.Error => new SysdResponse.Error(SysdErrorCodes.FromWire(r.U16()), r.Str()),
            _ => throw new SysdProtocolException($"unknown message kind 0x{kind:x4}"),
        };
        r.End();
        return (id, response);
    }

    /// <summary>Writes one frame (length prefix and payload) in a single write.</summary>
    public static async Task WriteFrameAsync(Stream stream, byte[] payload, CancellationToken ct)
    {
        if (payload.Length < HeaderLength || payload.Length > MaxPayloadBytes)
        {
            throw new SysdProtocolException($"frame length {payload.Length} is out of range");
        }

        var frame = new byte[4 + payload.Length];
        BinaryPrimitives.WriteUInt32LittleEndian(frame, (uint)payload.Length);
        payload.CopyTo(frame, 4);
        await stream.WriteAsync(frame, ct).ConfigureAwait(false);
        await stream.FlushAsync(ct).ConfigureAwait(false);
    }

    /// <summary>
    /// Reads one frame and returns its payload. The length is checked before
    /// the payload is read, so an out-of-range length never allocates.
    /// </summary>
    public static async Task<byte[]> ReadFrameAsync(Stream stream, CancellationToken ct)
    {
        var prefix = new byte[4];
        await ReadExactlyAsync(stream, prefix, isPrefix: true, ct).ConfigureAwait(false);
        var length = BinaryPrimitives.ReadUInt32LittleEndian(prefix);
        if (length < HeaderLength || length > MaxPayloadBytes)
        {
            throw new SysdProtocolException($"frame length {length} is out of range");
        }

        var payload = new byte[length];
        await ReadExactlyAsync(stream, payload, isPrefix: false, ct).ConfigureAwait(false);
        return payload;
    }

    private static async Task ReadExactlyAsync(Stream stream, byte[] buffer, bool isPrefix, CancellationToken ct)
    {
        var offset = 0;
        while (offset < buffer.Length)
        {
            var read = await stream.ReadAsync(buffer.AsMemory(offset), ct).ConfigureAwait(false);
            if (read == 0)
            {
                throw new SysdConnectionClosedException(isPrefix && offset == 0);
            }

            offset += read;
        }
    }

    private sealed class Writer
    {
        private readonly List<byte> _bytes = new(64);

        public Writer(ushort kind, uint id)
        {
            U16(ProtocolVersion);
            U16(kind);
            U32(id);
        }

        public byte[] ToArray() => [.. _bytes];

        public void Strs(IReadOnlyList<string> values)
        {
            U32((uint)values.Count);
            foreach (var v in values)
            {
                Str(v);
            }
        }

        public void U8(byte v) => _bytes.Add(v);

        public void U64(ulong v)
        {
            for (var i = 0; i < 8; i++)
            {
                _bytes.Add((byte)(v >> (8 * i)));
            }
        }

        public void Str(string value)
        {
            var bytes = Encoding.UTF8.GetBytes(value);
            if (bytes.Length > ushort.MaxValue)
            {
                throw new ArgumentException("string is longer than 65535 UTF-8 bytes", nameof(value));
            }

            U16((ushort)bytes.Length);
            _bytes.AddRange(bytes);
        }

        private void U16(ushort v)
        {
            _bytes.Add((byte)v);
            _bytes.Add((byte)(v >> 8));
        }

        public void U32(uint v)
        {
            for (var i = 0; i < 4; i++)
            {
                _bytes.Add((byte)(v >> (8 * i)));
            }
        }
    }

    private ref struct Reader(ReadOnlySpan<byte> bytes)
    {
        private static readonly UTF8Encoding StrictUtf8 = new(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true);

        private ReadOnlySpan<byte> _rest = bytes;

        public byte U8() => Take(1)[0];

        public ushort U16() => BinaryPrimitives.ReadUInt16LittleEndian(Take(2));

        public int I32() => BinaryPrimitives.ReadInt32LittleEndian(Take(4));

        public uint U32() => BinaryPrimitives.ReadUInt32LittleEndian(Take(4));

        public ulong U64() => BinaryPrimitives.ReadUInt64LittleEndian(Take(8));

        public bool Bool()
        {
            var b = Take(1)[0];
            return b switch
            {
                0 => false,
                1 => true,
                _ => throw new SysdProtocolException($"invalid boolean {b}"),
            };
        }

        public string Str()
        {
            var bytes = Take(U16());
            try
            {
                return StrictUtf8.GetString(bytes);
            }
            catch (DecoderFallbackException)
            {
                throw new SysdProtocolException("string is not UTF-8");
            }
        }

        public IReadOnlyList<string> StrList()
        {
            var count = U32();

            // Each entry needs at least its two length bytes, so a larger
            // count is rejected before allocating.
            if (count > (uint)(_rest.Length / 2))
            {
                throw new SysdProtocolException("message is truncated");
            }

            var list = new List<string>((int)count);
            for (var i = 0; i < count; i++)
            {
                list.Add(Str());
            }

            return list;
        }

        /// <summary>A u32 count of entries of at least <paramref name="min"/> bytes, checked before allocating.</summary>
        public int Count(int min)
        {
            var count = U32();
            if (count > (uint)(_rest.Length / min))
            {
                throw new SysdProtocolException("message is truncated");
            }

            return (int)count;
        }

        public IReadOnlyList<PackageInfo> PackageInfos()
        {
            var n = Count((7 * 2) + (3 * 8) + 4);
            var list = new List<PackageInfo>(n);
            for (var i = 0; i < n; i++)
            {
                list.Add(new PackageInfo(
                    Package: Str(),
                    Label: Str(),
                    VersionCode: U64(),
                    VersionName: Str(),
                    IconIcoPath: Str(),
                    IconPngPath: Str(),
                    LauncherActivities: StrList(),
                    InstalledMs: U64(),
                    SizeBytes: U64(),
                    AppUserModelId: Str()));
            }

            return list;
        }

        public IReadOnlyList<MissingFunction> MissingFunctions()
        {
            var n = Count(10);
            var list = new List<MissingFunction>(n);
            for (var i = 0; i < n; i++)
            {
                list.Add(new MissingFunction(Str(), U64()));
            }

            return list;
        }

        public SysdEventBatch EventBatch()
        {
            var epoch = U64();
            var next = U64();
            var missed = Bool();
            var n = Count(22);
            var events = new List<SysdEvent>(n);
            for (var i = 0; i < n; i++)
            {
                var seq = U64();
                var time = U64();
                var type = U16();
                var body = new Reader(Take((int)Math.Min(U32(), (uint)int.MaxValue)));
                SysdEvent e = type switch
                {
                    EventType.NotificationPosted => new SysdEvent.NotificationPosted(
                        seq, time, body.Str(), body.I32(), body.Str(), body.Str(), body.Str(), body.Str()),
                    EventType.NotificationRemoved => new SysdEvent.NotificationRemoved(seq, time, body.Str(), body.I32(), body.Str()),
                    EventType.AppCrashed => new SysdEvent.AppCrashed(seq, time, body.Str(), body.U32(), body.U32(), body.Str()),
                    _ => new SysdEvent.Unknown(seq, time, type),
                };
                if (e is not SysdEvent.Unknown)
                {
                    body.End();
                }

                events.Add(e);
            }

            return new SysdEventBatch(epoch, next, missed, events);
        }

        public readonly void End()
        {
            if (!_rest.IsEmpty)
            {
                throw new SysdProtocolException("message has trailing bytes");
            }
        }

        private ReadOnlySpan<byte> Take(int n)
        {
            if (_rest.Length < n)
            {
                throw new SysdProtocolException("message is truncated");
            }

            var taken = _rest[..n];
            _rest = _rest[n..];
            return taken;
        }
    }
}

/// <summary>A request to sysd; the properties a kind does not use stay at their defaults.</summary>
public sealed record SysdRequest(ushort Kind, string? Package = null)
{
    public string? Activity { get; init; }

    public IReadOnlyList<string>? Paths { get; init; }

    public string? Locale { get; init; }

    public bool KeepData { get; init; }

    public ulong Epoch { get; init; }

    public ulong After { get; init; }

    public uint TimeoutMs { get; init; }

    public IReadOnlyList<MissingFunction>? Entries { get; init; }

    public static SysdRequest Ping { get; } = new(SysdWire.Kind.Ping);

    public static SysdRequest Status { get; } = new(SysdWire.Kind.Status);

    public static SysdRequest ListPackages { get; } = new(SysdWire.Kind.ListPackages);

    public static SysdRequest StartPackage(string package) => new(SysdWire.Kind.StartPackage, package);

    public static SysdRequest FocusWindow(string package) => new(SysdWire.Kind.FocusWindow, package);

    public static SysdRequest StartActivity(string package, string activity) =>
        new(SysdWire.Kind.StartActivity, package) { Activity = activity };

    public static SysdRequest InspectPackage(IReadOnlyList<string> paths, string locale) =>
        new(SysdWire.Kind.InspectPackage) { Paths = paths, Locale = locale };

    public static SysdRequest InstallPackage(IReadOnlyList<string> paths) => new(SysdWire.Kind.InstallPackage) { Paths = paths };

    public static SysdRequest UninstallPackage(string package, bool keepData) =>
        new(SysdWire.Kind.UninstallPackage, package) { KeepData = keepData };

    public static SysdRequest ListPackageInfo(string locale) => new(SysdWire.Kind.ListPackageInfo) { Locale = locale };

    public static SysdRequest WaitEvents(ulong epoch, ulong after, uint timeoutMs) =>
        new(SysdWire.Kind.WaitEvents) { Epoch = epoch, After = after, TimeoutMs = timeoutMs };

    public static SysdRequest GetMissingFunctions(string package) => new(SysdWire.Kind.GetMissingFunctions, package);

    public static SysdRequest ReportMissingFunctions(IReadOnlyList<MissingFunction> entries) =>
        new(SysdWire.Kind.ReportMissingFunctions) { Entries = entries };
}

/// <summary>A response from sysd.</summary>
public abstract record SysdResponse
{
    private SysdResponse()
    {
    }

    public sealed record Pong(ushort Protocol) : SysdResponse;

    public sealed record StatusReply(SysdStatus Status) : SysdResponse;

    public sealed record Started(uint HostPid, bool AlreadyRunning) : SysdResponse;

    public sealed record Focused : SysdResponse;

    public sealed record Packages(IReadOnlyList<string> Names) : SysdResponse
    {
        public bool Equals(Packages? other) => other is not null && Names.SequenceEqual(other.Names);

        public override int GetHashCode() => Names.Count;
    }

    public sealed record Details(PackageDetails Value) : SysdResponse;

    public sealed record Installed(InstallOutcome Value) : SysdResponse;

    public sealed record Uninstalled : SysdResponse;

    public sealed record PackageInfos(IReadOnlyList<PackageInfo> Value) : SysdResponse
    {
        public bool Equals(PackageInfos? other) => other is not null && Value.SequenceEqual(other.Value);

        public override int GetHashCode() => Value.Count;
    }

    public sealed record Events(SysdEventBatch Value) : SysdResponse;

    public sealed record MissingFunctions(MissingFunctionReport Value) : SysdResponse;

    public sealed record Reported : SysdResponse;

    public sealed record Error(SysdErrorCode Code, string Message) : SysdResponse;
}

/// <summary>The wire protocol was violated (bad frame, bad body, wrong id).</summary>
public class SysdProtocolException(string message) : Exception(message);

/// <summary>The service closed the connection.</summary>
public sealed class SysdConnectionClosedException(bool beforeFrame)
    : SysdProtocolException(beforeFrame ? "the service closed the connection" : "the service closed the connection in the middle of a message")
{
    /// <summary>True when the connection closed cleanly between frames.</summary>
    public bool BeforeFrame { get; } = beforeFrame;
}
