using System.Runtime.CompilerServices;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Tests;

/// <summary>
/// The manager's codec for the package manager messages (golden file
/// <c>v1-pkg.txt</c>) and the event and diagnostics messages
/// (<c>v1-events.txt</c>), both written by the Rust encoder in crates/andruna-ipc.
/// </summary>
public class ProductGoldenTests
{
    private static string Fixture(string name, [CallerFilePath] string here = "") => Path.GetFullPath(Path.Combine(
        Path.GetDirectoryName(here)!, "..", "..", "..", "crates", "andruna-ipc", "tests", "golden", name));

    private static readonly Dictionary<string, (uint Id, byte[] Payload)> Golden = new[] { "v1-pkg.txt", "v1-events.txt" }
        .SelectMany(f => File.ReadAllLines(Fixture(f)))
        .Where(l => l.Length > 0 && !l.StartsWith('#'))
        .Select(l => l.Split(' '))
        .ToDictionary(f => f[0], f => (uint.Parse(f[1]), Convert.FromHexString(f[2])));

    public static TheoryData<string, SysdRequest> Requests => new()
    {
        { "request.inspect_package", SysdRequest.InspectPackage([@"C:\Users\u\Downloads\base.apk", @"C:\Users\u\Downloads\split_config.he.apk"], "he-IL") },
        { "request.install_package", SysdRequest.InstallPackage([@"C:\x\app.apk"]) },
        { "request.uninstall_package", SysdRequest.UninstallPackage("com.example.app", true) },
        { "request.list_package_info", SysdRequest.ListPackageInfo("en-US") },
        { "request.start_activity", SysdRequest.StartActivity("com.example.app", "com.example.app.Second") },
        { "request.wait_events", SysdRequest.WaitEvents(0x0102030405060708, 41, 25_000) },
        { "request.wait_events_now", SysdRequest.WaitEvents(0, ulong.MaxValue, 0) },
        { "request.get_missing_functions", SysdRequest.GetMissingFunctions("com.example.app") },
        { "request.report_missing_functions", SysdRequest.ReportMissingFunctions([new("lx:epoll_pwait2", 7), new("service:package.getInstalledModules", 1)]) },
    };

    public static TheoryData<string, SysdResponse> Responses => new()
    {
        {
            "response.package_details", new SysdResponse.Details(new PackageDetails(
                "com.example.app", 0x1_0000_0007, "1.0", "דוגמה", 21, 34, string.Concat(Enumerable.Repeat("ab", 32)), 3,
                ["android.permission.INTERNET"], ["arm64-v8a", "x86_64"], "x86_64", 0, @"C:\a\icon.png"))
        },
        { "response.installed", new SysdResponse.Installed(new InstallOutcome("com.example.app", 7, "1.0", false, CompileStatus.Unavailable, "andruna-app.exe not found", 1)) },
        { "response.uninstalled", new SysdResponse.Uninstalled() },
        {
            "response.package_infos", new SysdResponse.PackageInfos([new PackageInfo(
                "com.example.app", "Example", 7, "1.0", @"C:\a\icon.ico", @"C:\a\icon.png", ["com.example.app.Main"],
                1_795_000_000_000, 123_456, "Andruna.com.example.app")])
        },
        { "response.error_invalid_signature", new SysdResponse.Error(SysdErrorCode.InvalidSignature, "APK Signature Scheme v2 digest of chunked contents does not match") },
        { "response.events_empty", new SysdResponse.Events(new SysdEventBatch(0x0102030405060708, 41, false, [])) },
        {
            "response.events", new SysdResponse.Events(new SysdEventBatch(0x0102030405060708, 44, true, [
                new SysdEvent.NotificationPosted(42, 1_795_000_000_000, "com.example.chat", 3, string.Empty, "messages", "דנה", "Hello"),
                new SysdEvent.NotificationRemoved(43, 1_795_000_000_500, "com.example.chat", -1, "t"),
                new SysdEvent.AppCrashed(44, 1_795_000_001_000, "com.example.app", 4242, 0xC0000005, "java.lang.IllegalStateException: boom"),
            ]))
        },
        { "response.missing_functions", new SysdResponse.MissingFunctions(new MissingFunctionReport(1_795_000_000_000, [new("lx:epoll_pwait2", 7)])) },
        { "response.reported", new SysdResponse.Reported() },
        { "response.error_not_permitted", new SysdResponse.Error(SysdErrorCode.NotPermitted, "not a host") },
    };

    [Fact]
    public void Fixtures_CoverEveryMessage()
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

        // The test server reads what the client writes.
        var (gotId, decoded) = TestWire.DecodeRequest(payload);
        Assert.Equal(id, gotId);
        Assert.Equal(Convert.ToHexString(payload), Convert.ToHexString(SysdWire.EncodeRequest(decoded, id)));
    }

    [Theory]
    [MemberData(nameof(Responses))]
    public void Responses_DecodeFromTheGoldenBytes(string name, SysdResponse expected)
    {
        var (id, payload) = Golden[name];

        var (gotId, response) = SysdWire.DecodeResponse(payload);

        Assert.Equal(id, gotId);
        Assert.Equal(expected, response);
        Assert.Equal(Convert.ToHexString(payload), Convert.ToHexString(TestWire.Encode(expected, id)));
    }

    [Fact]
    public void Decoder_RejectsEveryTruncation_AndHugeCounts()
    {
        foreach (var (_, payload) in Golden.Where(g => g.Key.StartsWith("response.", StringComparison.Ordinal)).Select(g => g.Value))
        {
            for (var cut = 0; cut < payload.Length; cut++)
            {
                Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse(payload.AsSpan(0, cut)));
            }

            Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse([.. payload, 0]));
        }

        foreach (var name in new[] { "response.package_infos", "response.missing_functions" })
        {
            var huge = Golden[name].Payload.ToArray();
            var at = name == "response.package_infos" ? SysdWire.HeaderLength : SysdWire.HeaderLength + 8;
            huge.AsSpan(at, 4).Fill(0xFF);
            Assert.Throws<SysdProtocolException>(() => SysdWire.DecodeResponse(huge));
        }
    }

    [Fact]
    public void UnknownEventTypes_AreSkipped()
    {
        var batch = new SysdEventBatch(1, 2, false, [new SysdEvent.Unknown(1, 5, 99), new SysdEvent.NotificationRemoved(2, 6, "a.b", 1, string.Empty)]);
        var bytes = TestWire.Encode(new SysdResponse.Events(batch), 3);

        var (_, decoded) = SysdWire.DecodeResponse(bytes);

        Assert.Equal(new SysdResponse.Events(batch), decoded);
    }
}
