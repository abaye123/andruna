using System.Buffers.Binary;
using System.Diagnostics;
using System.IO.Compression;
using System.Runtime.CompilerServices;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Tests;

/// <summary>
/// Not a test of behavior: with ANDRUNA_SCREENSHOTS set to a folder, renders
/// every page of a Debug build of the manager in Hebrew and English, light
/// and dark, against this test's fake sysd with demo apps, into that folder
/// (ANDRUNA_MANAGER_SNAPSHOT, see ui/README.md). Without the variable it
/// does nothing. The demo data exists only here.
/// </summary>
public class Screenshots
{
    private const string Instance = "screenshots";

    private static string ManagerExe([CallerFilePath] string here = "") => Path.GetFullPath(Path.Combine(
        Path.GetDirectoryName(here)!, "..", "..", "src", "Andruna.Manager", "bin", "x64", "Debug", "net10.0-windows10.0.26100.0", "win-x64", "Andruna.Manager.exe"));

    [Fact]
    public async Task RenderAllPages()
    {
        var output = Environment.GetEnvironmentVariable("ANDRUNA_SCREENSHOTS");
        if (string.IsNullOrEmpty(output))
        {
            return;
        }

        Directory.CreateDirectory(output);
        var work = Path.Combine(Path.GetTempPath(), "andruna-screenshots-" + Guid.NewGuid().ToString("N"));
        var root = Path.Combine(work, "root");
        var icons = Path.Combine(work, "icons");
        Directory.CreateDirectory(icons);
        Directory.CreateDirectory(Path.Combine(root, "logs"));
        File.WriteAllLines(Path.Combine(root, "logs", "sysd.log"), [
            "ts=2026-11-27T14:02:11.204Z level=info event=starting version=0.1.0 instance=default",
            "ts=2026-11-27T14:02:11.219Z level=info event=ready pipe=\\\\.\\pipe\\Andruna.sysd.S-1-5-21.1.default",
            "ts=2026-11-27T14:02:15.880Z level=info event=host_started package=com.example.notes pid=8812",
            "ts=2026-11-27T14:09:42.031Z level=info event=host_exited package=com.example.notes pid=8812 code=-1073741819",
        ]);
        var apps = new[]
        {
            Demo("com.example.notes", "Notes", "פתקים", 0x2E7D32, 18_400_000),
            Demo("org.example.maps", "City Maps", "מפות העיר", 0x1565C0, 64_200_000),
            Demo("com.example.chat", "Chat", "צ'אט", 0x6A1B9A, 41_900_000),
            Demo("org.example.weather", "Weather", "מזג אוויר", 0xEF6C00, 9_700_000),
            Demo("com.example.bank", "Bank", "הבנק", 0x00838F, 77_300_000),
        };
        var apk = Path.Combine(work, "transit.apk");
        File.WriteAllBytes(apk, [0x50, 0x4B, 5, 6, .. new byte[18]]);
        var transitIcon = WriteIcon(Path.Combine(icons, "transit.png"), 0xC62828);

        PackageInfo Info((string Package, string En, string He, uint Color, ulong Size) a, string locale) => new(
            a.Package, locale.StartsWith("he", StringComparison.Ordinal) ? a.He : a.En, 30, "3.2.1", string.Empty,
            WriteIcon(Path.Combine(icons, a.Package + ".png"), a.Color), [a.Package + ".MainActivity"], 1_795_000_000_000, a.Size, "Andruna.App." + a.Package);

        var crashSent = 1;
        byte[]? Handle(uint id, SysdRequest req) => req.Kind switch
        {
            SysdWire.Kind.Status => TestWire.Encode(new SysdResponse.StatusReply(new SysdStatus(4120, 3_725_000, 1, 2, 30_000, "0.1.0")), id),
            SysdWire.Kind.ListPackageInfo => TestWire.Encode(new SysdResponse.PackageInfos(apps.Select(a => Info(a, req.Locale ?? "en")).OrderBy(i => i.Package).ToList()), id),
            SysdWire.Kind.InspectPackage when req.Paths is [var p] && p == apk => TestWire.Encode(new SysdResponse.Details(new PackageDetails(
                "org.example.transit", 412, "5.0.3", req.Locale?.StartsWith("he", StringComparison.Ordinal) == true ? "תחבורה ציבורית" : "Transit", 26, 35,
                "3f9a1c0e5b7d2a4c6e8f0a1b2c3d4e5f60718293a4b5c6d7e8f9a0b1c2d3e4f5", 3,
                ["android.permission.INTERNET", "android.permission.ACCESS_FINE_LOCATION", "android.permission.POST_NOTIFICATIONS", "android.permission.ACCESS_NETWORK_STATE", "org.example.transit.permission.C2D_MESSAGE"],
                [], string.Empty, 0, transitIcon)), id),
            SysdWire.Kind.InspectPackage => TestWire.Encode(new SysdResponse.Details(new PackageDetails(
                "com.example.notes", 30, "3.2.1", "Notes", 24, 35, "ab", 2,
                ["android.permission.INTERNET", "android.permission.CAMERA", "android.permission.READ_MEDIA_IMAGES", "android.permission.POST_NOTIFICATIONS", "android.permission.VIBRATE"],
                [], string.Empty, 30, string.Empty)), id),
            SysdWire.Kind.WaitEvents when req.After == ulong.MaxValue => TestWire.Encode(new SysdResponse.Events(new SysdEventBatch(7, 3, false, [])), id),
            SysdWire.Kind.WaitEvents when req.Epoch == 7 && req.After == 3 && Interlocked.Exchange(ref crashSent, 1) == 0 => TestWire.Encode(new SysdResponse.Events(new SysdEventBatch(7, 4, false, [
                new SysdEvent.AppCrashed(4, 1_795_000_000_000, "com.example.notes", 8812, 0xC0000005, string.Empty)])), id),
            SysdWire.Kind.WaitEvents when req.TimeoutMs == 0 => TestWire.Encode(new SysdResponse.Events(new SysdEventBatch(7, 4, false, [
                new SysdEvent.AppCrashed(2, 1_795_000_000_000, "com.example.notes", 8812, 0xC0000005, string.Empty),
                new SysdEvent.AppCrashed(4, 1_795_000_600_000, "org.example.maps", 9120, 1, "java.lang.IllegalStateException: no map tiles cache")])), id),
            SysdWire.Kind.WaitEvents => Delay(() => TestWire.Encode(new SysdResponse.Events(new SysdEventBatch(7, req.After, false, [])), id)),
            SysdWire.Kind.GetMissingFunctions => TestWire.Encode(new SysdResponse.MissingFunctions(new MissingFunctionReport(1_795_000_000_000, [
                new("lx:epoll_pwait2", 1834), new("service:package.getInstalledModules", 412), new("lx:io_uring_setup", 97), new("service:window.getDisplayCutout", 12)])), id),
            _ => FakeSysd.Answer(id, req, apps.Select(a => a.Package).ToList()),
        };

        await using var sysd = new FakeSysd(Handle, SysdEndpoint.PipeNameForCurrentUser(Instance));
        foreach (var language in new[] { "english", "hebrew" })
        {
            foreach (var theme in new[] { "light", "dark" })
            {
                var suffix = $"{(language == "hebrew" ? "he" : "en")}-{theme}";
                var settings = Path.Combine(work, $"settings-{suffix}.json");
                File.WriteAllText(settings, $"{{ \"language\": \"{language}\", \"theme\": \"{theme}\" }}");
                foreach (var (name, args) in new (string, string[])[]
                         {
                             ("library", ["--page", "library"]),
                             ("app", ["--app", "com.example.notes"]),
                             ("install", ["--install", apk]),
                             ("diagnostics", ["--diagnostics", "com.example.notes"]),
                             ("settings", ["--page", "settings"]),
                         })
                {
                    Run(args, settings, root, "ANDRUNA_MANAGER_SNAPSHOT", Path.Combine(output, $"{name}-{suffix}.png"));
                }

                Volatile.Write(ref crashSent, 0);
                Run(["--background"], settings, root, "ANDRUNA_MANAGER_SNAPSHOT_CRASH", Path.Combine(output, $"crash-{suffix}.png"));
            }
        }

        Directory.Delete(work, recursive: true);
    }

    private static byte[] Delay(Func<byte[]> answer)
    {
        Thread.Sleep(500);
        return answer();
    }

    private static (string Package, string En, string He, uint Color, ulong Size) Demo(string package, string en, string he, uint color, ulong size) =>
        (package, en, he, color, size);

    private static void Run(string[] args, string settings, string root, string variable, string png)
    {
        var info = new ProcessStartInfo(ManagerExe()) { UseShellExecute = false };
        foreach (var a in args)
        {
            info.ArgumentList.Add(a);
        }

        info.Environment["ANDRUNA_MANAGER_INSTANCE"] = Instance;
        info.Environment["ANDRUNA_MANAGER_ROOT"] = root;
        info.Environment["ANDRUNA_MANAGER_SETTINGS"] = settings;
        info.Environment[variable] = png;
        using var p = Process.Start(info)!;
        if (!p.WaitForExit(60_000))
        {
            p.Kill();
            throw new TimeoutException(png);
        }

        Assert.True(File.Exists(png), png);
    }

    /// <summary>A 96 x 96 rounded square in one color with a lighter circle: a stand-in app icon.</summary>
    private static string WriteIcon(string path, uint rgb)
    {
        if (File.Exists(path))
        {
            return path;
        }

        const int size = 96;
        var raw = new byte[size * ((size * 4) + 1)];
        for (var y = 0; y < size; y++)
        {
            var row = y * ((size * 4) + 1);
            for (var x = 0; x < size; x++)
            {
                var dx = Math.Max(Math.Abs(x - 47.5) - 27.5, 0);
                var dy = Math.Max(Math.Abs(y - 47.5) - 27.5, 0);
                var corner = (dx * dx) + (dy * dy) > 400;
                var circle = ((x - 48) * (x - 48)) + ((y - 48) * (y - 48)) < 22 * 22;
                var (r, g, b) = ((byte)(rgb >> 16), (byte)(rgb >> 8), (byte)rgb);
                if (circle)
                {
                    (r, g, b) = ((byte)((r + 255) / 2), (byte)((g + 255) / 2), (byte)((b + 255) / 2));
                }

                var o = row + 1 + (x * 4);
                (raw[o], raw[o + 1], raw[o + 2], raw[o + 3]) = (r, g, b, corner ? (byte)0 : (byte)255);
            }
        }

        using var compressed = new MemoryStream();
        using (var z = new ZLibStream(compressed, CompressionLevel.Optimal, leaveOpen: true))
        {
            z.Write(raw);
        }

        using var png = File.Create(path);
        png.Write([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        var header = new byte[13];
        BinaryPrimitives.WriteInt32BigEndian(header, size);
        BinaryPrimitives.WriteInt32BigEndian(header.AsSpan(4), size);
        header[8] = 8;
        header[9] = 6;
        Chunk(png, "IHDR", header);
        Chunk(png, "IDAT", compressed.ToArray());
        Chunk(png, "IEND", []);
        return path;
    }

    private static void Chunk(Stream s, string type, byte[] data)
    {
        var len = new byte[4];
        BinaryPrimitives.WriteInt32BigEndian(len, data.Length);
        s.Write(len);
        var typed = System.Text.Encoding.ASCII.GetBytes(type).Concat(data).ToArray();
        s.Write(typed);
        var crc = new byte[4];
        BinaryPrimitives.WriteUInt32BigEndian(crc, Crc32(typed));
        s.Write(crc);
    }

    private static uint Crc32(byte[] data)
    {
        var crc = 0xFFFFFFFFu;
        foreach (var b in data)
        {
            crc ^= b;
            for (var k = 0; k < 8; k++)
            {
                crc = (crc & 1) != 0 ? (crc >> 1) ^ 0xEDB88320u : crc >> 1;
            }
        }

        return ~crc;
    }
}
