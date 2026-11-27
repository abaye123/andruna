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

        public const ushort Pong = 0x8001;
        public const ushort StatusReply = 0x8002;
        public const ushort Started = 0x8003;
        public const ushort Focused = 0x8004;
        public const ushort Packages = 0x8005;
        public const ushort Error = 0x80FF;
    }

    /// <summary>Encodes a request payload (without the length prefix).</summary>
    public static byte[] EncodeRequest(SysdRequest request, uint id)
    {
        var w = new Writer(request.Kind, id);
        if (request.Package is { } package)
        {
            w.Str(package);
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

        private void U32(uint v)
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

        public ushort U16() => BinaryPrimitives.ReadUInt16LittleEndian(Take(2));

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

/// <summary>A request to sysd. <see cref="Package"/> is set for the kinds that carry one.</summary>
public sealed record SysdRequest(ushort Kind, string? Package = null)
{
    public static SysdRequest Ping { get; } = new(SysdWire.Kind.Ping);

    public static SysdRequest Status { get; } = new(SysdWire.Kind.Status);

    public static SysdRequest ListPackages { get; } = new(SysdWire.Kind.ListPackages);

    public static SysdRequest StartPackage(string package) => new(SysdWire.Kind.StartPackage, package);

    public static SysdRequest FocusWindow(string package) => new(SysdWire.Kind.FocusWindow, package);
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
