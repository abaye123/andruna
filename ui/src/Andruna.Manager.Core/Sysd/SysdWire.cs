using System.Buffers.Binary;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Andruna.Manager.Core.Sysd;

/// <summary>
/// Framing and message shapes of the manager-to-sysd named pipe protocol.
/// See ui/README.md ("sysd protocol") for the wire format. Every message is a
/// 4-byte little-endian length followed by that many bytes of UTF-8 JSON.
/// </summary>
public static class SysdWire
{
    /// <summary>Protocol version this client speaks.</summary>
    public const int ProtocolVersion = 1;

    /// <summary>Largest message the client accepts. Larger frames are a protocol error.</summary>
    public const int MaxMessageBytes = 4 * 1024 * 1024;

    public static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    public static async Task WriteFrameAsync(Stream stream, byte[] payload, CancellationToken ct)
    {
        if (payload.Length > MaxMessageBytes)
        {
            throw new SysdProtocolException($"outgoing message of {payload.Length} bytes exceeds the limit");
        }

        var header = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(header, (uint)payload.Length);
        await stream.WriteAsync(header, ct).ConfigureAwait(false);
        await stream.WriteAsync(payload, ct).ConfigureAwait(false);
        await stream.FlushAsync(ct).ConfigureAwait(false);
    }

    public static async Task<byte[]> ReadFrameAsync(Stream stream, CancellationToken ct)
    {
        var header = new byte[4];
        await ReadExactlyAsync(stream, header, ct).ConfigureAwait(false);
        var length = BinaryPrimitives.ReadUInt32LittleEndian(header);
        if (length > MaxMessageBytes)
        {
            throw new SysdProtocolException($"incoming message of {length} bytes exceeds the limit");
        }

        var payload = new byte[length];
        await ReadExactlyAsync(stream, payload, ct).ConfigureAwait(false);
        return payload;
    }

    private static async Task ReadExactlyAsync(Stream stream, byte[] buffer, CancellationToken ct)
    {
        var offset = 0;
        while (offset < buffer.Length)
        {
            var read = await stream.ReadAsync(buffer.AsMemory(offset), ct).ConfigureAwait(false);
            if (read == 0)
            {
                throw new SysdProtocolException("the service closed the connection in the middle of a message");
            }

            offset += read;
        }
    }
}

/// <summary>A request: <c>{"id": 1, "method": "status"}</c>.</summary>
public sealed record SysdRequest(long Id, string Method);

/// <summary>A response: either <c>result</c> or <c>error</c> is set.</summary>
public sealed record SysdResponse(long Id, JsonElement? Result, SysdError? Error);

/// <summary>An error reported by the service.</summary>
public sealed record SysdError(string Code, string? Message);

/// <summary>The wire protocol was violated (bad frame, bad JSON, wrong id).</summary>
public sealed class SysdProtocolException(string message, Exception? inner = null) : Exception(message, inner);
