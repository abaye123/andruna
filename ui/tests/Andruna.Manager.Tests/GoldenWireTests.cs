using System.Runtime.CompilerServices;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Tests;

/// <summary>
/// The manager's codec against the golden payloads that the Rust encoder of
/// crates/andruna-ipc produces (crates/andruna-ipc/tests/golden.rs checks the
/// same file against the Rust side).
/// </summary>
public class GoldenWireTests
{
    private static string FixturePath([CallerFilePath] string here = "") => Path.GetFullPath(Path.Combine(
        Path.GetDirectoryName(here)!, "..", "..", "..", "crates", "andruna-ipc", "tests", "golden", "v1.txt"));

    private static readonly Dictionary<string, (uint Id, byte[] Payload)> Golden = File.ReadAllLines(FixturePath())
        .Where(l => l.Length > 0 && !l.StartsWith('#'))
        .Select(l => l.Split(' '))
        .ToDictionary(f => f[0], f => (uint.Parse(f[1]), Convert.FromHexString(f[2])));

    public static TheoryData<string, SysdRequest> Requests => new()
    {
        { "request.ping", SysdRequest.Ping },
        { "request.status", SysdRequest.Status },
        { "request.start_package", SysdRequest.StartPackage("com.example.app") },
        { "request.focus_window", SysdRequest.FocusWindow("org.example.x_1") },
        { "request.list_packages", SysdRequest.ListPackages },
    };

    public static TheoryData<string, SysdResponse> Responses => new()
    {
        { "response.pong", new SysdResponse.Pong(1) },
        { "response.status", new SysdResponse.StatusReply(new SysdStatus(1234, 90_061_000, 2, 1, 30_000, "0.1.0")) },
        { "response.started", new SysdResponse.Started(77, true) },
        { "response.focused", new SysdResponse.Focused() },
        { "response.packages_empty", new SysdResponse.Packages([]) },
        { "response.packages", new SysdResponse.Packages(["a.b", "com.example.א"]) },
        { "response.error_not_installed", new SysdResponse.Error(SysdErrorCode.NotInstalled, "package `org.example.none` is not installed") },
        { "response.error_unknown_code", new SysdResponse.Error((SysdErrorCode)999, string.Empty) },
    };

    [Fact]
    public void Fixture_CoversEveryMessage()
    {
        var names = Requests.Select(r => (string)r[0]).Concat(Responses.Select(r => (string)r[0]));
        Assert.Equal(Golden.Keys.Order(), names.Order());
    }

    [Theory]
    [MemberData(nameof(Requests))]
    public void Requests_EncodeToTheGoldenBytes(string name, SysdRequest request)
    {
        var (id, payload) = Golden[name];

        Assert.Equal(Convert.ToHexString(payload), Convert.ToHexString(SysdWire.EncodeRequest(request, id)));
    }

    [Theory]
    [MemberData(nameof(Responses))]
    public void Responses_DecodeFromTheGoldenBytes(string name, SysdResponse expected)
    {
        var (id, payload) = Golden[name];

        var (gotId, response) = SysdWire.DecodeResponse(payload);

        Assert.Equal(id, gotId);
        Assert.Equal(expected, response);

        // The test server's encoder produces the same bytes, so FakeSysd speaks the real protocol.
        Assert.Equal(Convert.ToHexString(payload), Convert.ToHexString(TestWire.Encode(expected, id)));
    }

    [Fact]
    public void Decoder_RejectsEveryTruncation_AndTrailingBytes()
    {
        foreach (var (_, payload) in Golden.Where(g => g.Key.StartsWith("response.")).Select(g => g.Value))
        {
            for (var cut = 0; cut < payload.Length; cut++)
            {
                Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse(payload.AsSpan(0, cut)));
            }

            Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse([.. payload, 0]));
        }
    }

    [Fact]
    public void Decoder_RejectsBadBool_BadUtf8_UnknownKind_HugeCount()
    {
        var started = Golden["response.started"].Payload.ToArray();
        started[^1] = 2;
        Assert.Contains("boolean", Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse(started)).Message);

        var packages = Golden["response.packages"].Payload.ToArray();
        packages[^1] = 0xFF;
        Assert.Contains("UTF-8", Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse(packages)).Message);

        // A request kind is not a response.
        Assert.Contains("kind", Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse(Golden["request.ping"].Payload)).Message);

        var huge = Golden["response.packages_empty"].Payload.ToArray();
        huge.AsSpan(SysdWire.HeaderLength).Fill(0xFF);
        Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse(huge));
    }

    [Fact]
    public void Decoder_NeverThrowsAnythingElse_OnArbitraryBytes()
    {
        // The same xorshift generator the Rust codec test uses as a stand-in for a fuzzer.
        ulong x = 0x9E3779B97F4A7C15;
        for (var i = 0; i < 20_000; i++)
        {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            var buf = new byte[(int)(x % 40)];
            var y = x;
            for (var j = 0; j < buf.Length; j++)
            {
                y = unchecked((y * 6364136223846793005) + 1);
                buf[j] = (byte)(y >> 56);
            }

            if (buf.Length >= 4)
            {
                buf[0] = 1;
                buf[1] = 0;
                buf[3] = 0x80;
            }

            try
            {
                SysdWire.DecodeResponse(buf);
            }
            catch (SysdProtocolException)
            {
            }
        }
    }
}
