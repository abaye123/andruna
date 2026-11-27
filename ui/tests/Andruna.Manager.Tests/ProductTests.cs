using System.IO.Compression;
using Andruna.Manager.Core.Events;
using Andruna.Manager.Core.Platform;
using Andruna.Manager.Core.Sysd;
using Andruna.Manager.Core.ViewModels;

namespace Andruna.Manager.Tests;

/// <summary>Test doubles and fixtures for the product features.</summary>
internal static class TestServices
{
    public static AppPageServices AppPage(AndrunaPaths? paths = null, IAppLauncher? launcher = null, StartMenu? startMenu = null) => new(
        paths ?? new AndrunaPaths(Path.Combine(Path.GetTempPath(), "andruna-tests-none-" + Guid.NewGuid().ToString("N"))),
        launcher ?? new RecordingLauncher(),
        startMenu ?? new StartMenu(Path.Combine(Path.GetTempPath(), "andruna-tests-start-" + Guid.NewGuid().ToString("N")), @"C:\Andruna\andruna-launch.exe"),
        "en-US");

    public static PackageInfo Info(string package, string label = "Example", ulong size = 2_500_000) => new(
        package, label, 7, "1.2", string.Empty, string.Empty, [package + ".Main"], 1_795_000_000_000, size, "Andruna." + package);

    /// <summary>A sysd that knows the package manager messages and answers them from these values.</summary>
    public static FakeSysd PackageSysd(IReadOnlyList<PackageInfo> infos, Func<uint, SysdRequest, byte[]?>? more = null) => new((id, req) =>
        more?.Invoke(id, req) ?? (req.Kind switch
        {
            SysdWire.Kind.ListPackageInfo => TestWire.Encode(new SysdResponse.PackageInfos(infos), id),
            _ => FakeSysd.Answer(id, req, infos.Select(i => i.Package).ToList()),
        }));
}

internal sealed class RecordingLauncher(bool ok = true) : IAppLauncher
{
    public List<string> Opened { get; } = [];

    public bool TryOpen(string package, string? activity, out string? error)
    {
        Opened.Add(LaunchExecutable.Target(package, activity));
        error = ok ? null : "andruna-launch.exe is missing";
        return ok;
    }
}

internal sealed class RecordingToasts : IToastPresenter
{
    public List<ToastRequest> Shown { get; } = [];

    public List<(string Tag, string Group)> Removed { get; } = [];

    public void Show(ToastRequest toast) => Shown.Add(toast);

    public void Remove(string tag, string group) => Removed.Add((tag, group));
}

internal sealed class RecordingCrashes : ICrashPresenter
{
    public List<CrashReport> Reports { get; } = [];

    public void ShowCrash(CrashReport report) => Reports.Add(report);
}

internal sealed class RecordingSink : IEventSink
{
    public List<SysdEvent> Events { get; } = [];

    public void OnEvent(SysdEvent e)
    {
        lock (Events)
        {
            Events.Add(e);
        }
    }
}

public class LaunchRequestTests
{
    [Theory]
    [InlineData(new string[0], "library", null, null, false)]
    [InlineData(new[] { "--page", "settings" }, "settings", null, null, false)]
    [InlineData(new[] { "--app", "com.example.a" }, "app", "com.example.a", null, false)]
    [InlineData(new[] { "--diagnostics", "com.example.a" }, "diagnostics", "com.example.a", null, false)]
    [InlineData(new[] { @"C:\Downloads\My App.APK" }, "library", null, @"C:\Downloads\My App.APK", false)]
    [InlineData(new[] { "--install", @"C:\x\b.xapk" }, "library", null, @"C:\x\b.xapk", false)]
    [InlineData(new[] { "--background" }, "library", null, null, true)]
    [InlineData(new[] { @"C:\notes.txt" }, "library", null, null, false)]
    public void Parse(string[] args, string page, string? package, string? file, bool background) =>
        Assert.Equal(new LaunchRequest(page, package, file, background), LaunchRequest.Parse(args));

    [Theory]
    [InlineData(@"""C:\Program Files\Andruna\Andruna.Manager.exe"" ""C:\Users\a b\x.apk""", new[] { @"C:\Users\a b\x.apk" })]
    [InlineData(@"--page settings", new[] { "--page", "settings" })]
    [InlineData(@"a\\""b c"" d\e ""x\""y""", new[] { @"a\b c", @"d\e", @"x""y" })]
    [InlineData("", new string[0])]
    public void SplitCommandLine_FollowsWindowsRules(string line, string[] expected) =>
        Assert.Equal(expected, LaunchRequest.SplitCommandLine(line));
}

public class NotificationBridgeTests
{
    private static readonly KeyStrings Strings = new();

    private static (NotificationBridge Bridge, RecordingToasts Toasts, RecordingCrashes Crashes) Create() =>
        Create(p => p == "com.example.chat" ? "Chat" : null);

    private static (NotificationBridge Bridge, RecordingToasts Toasts, RecordingCrashes Crashes) Create(Func<string, string?> labels)
    {
        var toasts = new RecordingToasts();
        var crashes = new RecordingCrashes();
        return (new NotificationBridge(toasts, crashes, labels, Strings), toasts, crashes);
    }

    [Fact]
    public void Notification_BecomesAToast_ThatOpensTheApp()
    {
        var (bridge, toasts, _) = Create();

        bridge.OnEvent(new SysdEvent.NotificationPosted(1, 0, "com.example.chat", 3, "dm", "messages", "Dana", "Hi there"));

        var toast = Assert.Single(toasts.Shown);
        Assert.Equal(new ToastRequest("3:dm", "com.example.chat", "Dana", "Hi there", "Chat", "action=open;package=com.example.chat"), toast);
        Assert.Equal("com.example.chat", NotificationBridge.PackageToOpen(toast.Arguments));
    }

    [Fact]
    public void Removal_RemovesTheSameTagAndGroup_EmptyTitleUsesTheLabel()
    {
        var (bridge, toasts, _) = Create();

        bridge.OnEvent(new SysdEvent.NotificationPosted(1, 0, "com.example.chat", 9, string.Empty, "c", " ", "x"));
        bridge.OnEvent(new SysdEvent.NotificationRemoved(2, 0, "com.example.chat", 9, string.Empty));

        Assert.Equal("Chat", toasts.Shown[0].Title);
        Assert.Equal([(toasts.Shown[0].Tag, toasts.Shown[0].Group)], toasts.Removed);
    }

    [Fact]
    public void LongIdentities_FitWindowsLimits_AndStayDistinct()
    {
        var longTag = new string('t', 200);
        var a = NotificationBridge.TagFor(1, longTag);
        var b = NotificationBridge.TagFor(1, longTag + "x");
        var group = NotificationBridge.GroupFor("com." + new string('a', 100) + ".app");

        Assert.True(a.Length <= 64 && b.Length <= 64 && group.Length <= 64);
        Assert.NotEqual(a, b);
    }

    [Fact]
    public void AnAppPostingTooMuch_IsThrottled()
    {
        var (bridge, toasts, _) = Create();
        for (var i = 0; i < NotificationBridge.MaxToastsPerMinute + 5; i++)
        {
            bridge.OnEvent(new SysdEvent.NotificationPosted((ulong)i, 0, "com.example.chat", i, string.Empty, "c", "t", "x"));
        }

        bridge.OnEvent(new SysdEvent.NotificationPosted(99, 0, "org.example.other", 1, string.Empty, "c", "t", "x"));

        Assert.Equal(NotificationBridge.MaxToastsPerMinute + 1, toasts.Shown.Count);
        Assert.Equal(5, bridge.Throttled);
    }

    [Theory]
    [InlineData(0xC0000005u, "", "CrashReasonAccessViolation")]
    [InlineData(0xC00000FDu, "", "CrashReasonStackOverflow")]
    [InlineData(1u, "", "CrashReasonExitCode:0x00000001")]
    [InlineData(1u, "java.lang.NullPointerException", "CrashReasonApp:java.lang.NullPointerException")]
    public void Crash_OpensTheDialog_WithAPlainReason(uint code, string reason, string explanation)
    {
        var (bridge, _, crashes) = Create();

        bridge.OnEvent(new SysdEvent.AppCrashed(5, 1_795_000_000_000, "com.example.chat", 77, code, reason));

        var report = Assert.Single(crashes.Reports);
        Assert.Equal("CrashTitle:Chat", report.Title);
        Assert.Equal(explanation, report.Explanation);
        Assert.Equal(77u, report.Pid);
    }

    [Theory]
    [InlineData("action=open;package=com.example.a", "com.example.a")]
    [InlineData("package=com.example.a;action=open", "com.example.a")]
    [InlineData("action=open;package=..\\evil", null)]
    [InlineData("action=delete;package=com.example.a", null)]
    [InlineData("", null)]
    public void ClickArguments_AreValidated(string args, string? package) =>
        Assert.Equal(package, NotificationBridge.PackageToOpen(args));
}

public class EventPumpTests
{
    [Fact]
    public async Task StartsAtTheCurrentPosition_ThenDeliversNewEvents_InOrder()
    {
        var requests = new List<SysdRequest>();
        var crash = new SysdEvent.AppCrashed(6, 0, "a.b", 1, 1, string.Empty);
        var posted = new SysdEvent.NotificationPosted(7, 0, "a.b", 1, string.Empty, "c", "t", "x");
        await using var sysd = new FakeSysd((id, req) =>
        {
            lock (requests)
            {
                requests.Add(req);
            }

            var batch = req.After switch
            {
                ulong.MaxValue => new SysdEventBatch(9, 5, false, []),
                5 => new SysdEventBatch(9, 7, false, [crash, posted]),
                _ => new SysdEventBatch(9, req.After, false, []),
            };
            return TestWire.Encode(new SysdResponse.Events(batch), id);
        });
        using var client = sysd.Client();
        var sink = new RecordingSink();
        var pump = new EventPump(client, sink, TimeSpan.FromMilliseconds(10), waitMs: 50);
        using var cts = new CancellationTokenSource();

        var run = pump.RunAsync(() => true, cts.Token);
        while (pump.Delivered < 2)
        {
            await Task.Delay(10);
        }

        cts.Cancel();
        Assert.Equal(EventPumpEnd.Cancelled, await run);
        Assert.Equal([crash, posted], sink.Events);
        lock (requests)
        {
            Assert.Equal((0ul, ulong.MaxValue, 0u), (requests[0].Epoch, requests[0].After, requests[0].TimeoutMs));
            Assert.Equal((9ul, 5ul, 50u), (requests[1].Epoch, requests[1].After, requests[1].TimeoutMs));
            Assert.Equal(7ul, requests[2].After);
        }
    }

    [Fact]
    public async Task OlderService_EndsThePump_AsUnsupported()
    {
        await using var sysd = FakeSysd.Running();
        using var client = sysd.Client();

        var end = await new EventPump(client, new RecordingSink()).RunAsync(() => true, CancellationToken.None);

        Assert.Equal(EventPumpEnd.Unsupported, end);
    }

    [Fact]
    public async Task NoService_EndsThePump_WhenNotAskedToWait()
    {
        using var client = new SysdClient("andruna-test-absent-" + Guid.NewGuid().ToString("N"), TimeSpan.FromMilliseconds(50));

        var end = await new EventPump(client, new RecordingSink()).RunAsync(() => false, CancellationToken.None);

        Assert.Equal(EventPumpEnd.ServiceGone, end);
    }
}

public class InstallViewModelTests
{
    private static readonly KeyStrings Strings = new();

    private static readonly PackageDetails Details = new(
        "com.example.app", 7, "1.2", "Example", 21, 34, string.Concat(Enumerable.Repeat("ab", 32)), 2,
        ["android.permission.INTERNET", "android.permission.CAMERA", "com.example.CUSTOM"], [], string.Empty, 0, string.Empty);

    [Fact]
    public async Task Inspect_ShowsDetails_ThenInstalls()
    {
        var installed = new List<IReadOnlyList<string>>();
        await using var sysd = TestServices.PackageSysd([], (id, req) => req.Kind switch
        {
            SysdWire.Kind.InspectPackage => TestWire.Encode(new SysdResponse.Details(Details), id),
            SysdWire.Kind.InstallPackage => Record(req, id),
            _ => null,
        });
        byte[] Record(SysdRequest req, uint id)
        {
            installed.Add(req.Paths!);
            return TestWire.Encode(new SysdResponse.Installed(new InstallOutcome("com.example.app", 7, "1.2", false, CompileStatus.Unavailable, "no compiler", 1)), id);
        }

        var vm = new InstallViewModel(@"C:\Downloads\example.apk", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings, "he-IL");

        await vm.InspectAsync();

        Assert.Equal(InstallStage.Confirm, vm.Stage);
        Assert.True(vm.CanInstall);
        Assert.Equal("Example", vm.Label);
        Assert.Equal("InstallVersionNew:1.2|7", vm.VersionText);
        Assert.StartsWith("InstallSigner:ABAB ABAB", vm.SignerText, StringComparison.Ordinal);
        Assert.Equal("InstallActionInstall", vm.ActionText);

        // The sensitive permission first, then the known one, then the app's own.
        Assert.Equal(["android.permission.CAMERA", "android.permission.INTERNET", "com.example.CUSTOM"], vm.PermissionList.Select(p => p.Name));
        Assert.Equal("Perm_CAMERA", vm.PermissionList[0].Description);
        Assert.Equal("PermOther", vm.PermissionList[2].Description);

        await vm.InstallAsync();

        Assert.Equal(InstallStage.Done, vm.Stage);
        Assert.Equal([@"C:\Downloads\example.apk"], Assert.Single(installed));
        Assert.Equal("InstallDoneInstalled:Example InstallCompileUnavailable", vm.StatusText);
    }

    [Fact]
    public async Task Update_IsCalledUpdate()
    {
        await using var sysd = TestServices.PackageSysd([], (id, req) => req.Kind == SysdWire.Kind.InspectPackage
            ? TestWire.Encode(new SysdResponse.Details(Details with { InstalledVersionCode = 5 }), id)
            : null);
        var vm = new InstallViewModel("x.apk", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings, "en-US");

        await vm.InspectAsync();

        Assert.Equal("InstallActionUpdate", vm.ActionText);
        Assert.Equal("InstallVersionUpdate:1.2|7|5", vm.VersionText);
    }

    [Fact]
    public async Task OlderService_SaysInstallingIsNotAvailable_AndNamesTheFile()
    {
        await using var sysd = FakeSysd.Running();
        var vm = new InstallViewModel(@"C:\Users\someone\Downloads\app-release.apk", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings, "en-US");

        await vm.InspectAsync();

        Assert.Equal(InstallStage.Unavailable, vm.Stage);
        Assert.False(vm.CanInstall);
        Assert.Equal(new UserMessage("InstallUnavailableTitle", "InstallUnavailableMessage:app-release.apk"), vm.Problem);
    }

    [Theory]
    [InlineData(SysdErrorCode.InvalidSignature, "SysdErrorInvalidSignature")]
    [InlineData(SysdErrorCode.SignerMismatch, "SysdErrorSignerMismatch")]
    [InlineData(SysdErrorCode.IncompatibleAbi, "SysdErrorIncompatibleAbi")]
    public async Task Refusals_AreExplained(SysdErrorCode code, string message)
    {
        await using var sysd = TestServices.PackageSysd([], (id, req) => req.Kind == SysdWire.Kind.InspectPackage
            ? TestWire.Encode(new SysdResponse.Error(code, "detail"), id)
            : null);
        var vm = new InstallViewModel("x.apk", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings, "en-US");

        await vm.InspectAsync();

        Assert.Equal(InstallStage.Failed, vm.Stage);
        Assert.Equal(new UserMessage("InstallFailedTitle", message), vm.Problem);
    }
}

public sealed class AppPageProductTests : IDisposable
{
    private static readonly KeyStrings Strings = new();
    private readonly string _dir = Path.Combine(Path.GetTempPath(), "andruna-tests-" + Guid.NewGuid().ToString("N"));

    public void Dispose()
    {
        if (Directory.Exists(_dir))
        {
            Directory.Delete(_dir, recursive: true);
        }
    }

    [Fact]
    public async Task ShowsDetails_Sizes_AndPermissionsReadBySysd()
    {
        var paths = new AndrunaPaths(_dir);
        Directory.CreateDirectory(paths.AppDir("com.example.app"));
        File.WriteAllText(Path.Combine(paths.AppDir("com.example.app"), "base.apk"), "x");
        Directory.CreateDirectory(Path.Combine(paths.DataDir("com.example.app"), "files"));
        File.WriteAllBytes(Path.Combine(paths.DataDir("com.example.app"), "files", "a.bin"), new byte[1500]);
        var inspected = new List<IReadOnlyList<string>>();
        await using var sysd = TestServices.PackageSysd([TestServices.Info("com.example.app")], (id, req) =>
        {
            if (req.Kind != SysdWire.Kind.InspectPackage)
            {
                return null;
            }

            inspected.Add(req.Paths!);
            return TestWire.Encode(new SysdResponse.Details(new PackageDetails(
                "com.example.app", 7, "1.2", "Example", 21, 34, "ab", 2, ["android.permission.INTERNET"], [], string.Empty, 7, string.Empty)), id);
        });
        var vm = new AppPageViewModel("com.example.app", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings, TestServices.AppPage(paths));

        await vm.LoadAsync();

        Assert.Equal("Example", vm.Title);
        Assert.Equal("AppVersionFormat:1.2 (7)", vm.VersionText);
        Assert.Equal("2.5 MB", vm.SizeText.Replace(',', '.'));
        Assert.Equal("1.5 KB", vm.DataSizeText.Replace(',', '.'));
        Assert.Equal([Path.Combine(paths.AppDir("com.example.app"), "base.apk")], Assert.Single(inspected));
        Assert.Equal("Perm_INTERNET", Assert.Single(vm.PermissionList).Description);
        Assert.True(vm.CanPin && vm.CanUninstall && vm.CanOpen);
    }

    [Fact]
    public async Task Open_RunsTheLauncher_PinCreatesTheStartShortcut()
    {
        var launcher = new RecordingLauncher();
        var startMenu = new StartMenu(Path.Combine(_dir, "Programs"), @"C:\Andruna\andruna-launch.exe");
        await using var sysd = TestServices.PackageSysd([TestServices.Info("com.example.app", "Ex: ample?")]);
        var vm = new AppPageViewModel("com.example.app", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings,
            TestServices.AppPage(new AndrunaPaths(_dir), launcher, startMenu));
        await vm.LoadAsync();

        vm.Open();
        vm.PinToStart();

        Assert.Equal(["com.example.app"], launcher.Opened);
        Assert.Equal(new UserMessage("AppPinTitle", "AppPinMessage:Ex: ample?"), vm.Notice);
        var link = Path.Combine(_dir, "Programs", "Ex  ample.lnk");
        Assert.True(File.Exists(link));
        Assert.Equal(("com.example.app", "Andruna.com.example.app"), StartMenu.Read(link));
    }

    [Fact]
    public async Task Uninstall_KeepsDataWhenAsked_AndOlderServiceSaysNotAvailable()
    {
        var keep = new List<bool>();
        await using var sysd = TestServices.PackageSysd([TestServices.Info("com.example.app")], (id, req) =>
        {
            if (req.Kind != SysdWire.Kind.UninstallPackage)
            {
                return null;
            }

            keep.Add(req.KeepData);
            return TestWire.Encode(new SysdResponse.Uninstalled(), id);
        });
        var vm = new AppPageViewModel("com.example.app", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings, TestServices.AppPage());
        await vm.LoadAsync();

        await vm.UninstallAsync(keepData: true);

        Assert.Equal([true], keep);
        Assert.Equal("AppUninstalledTitle", vm.Notice!.Title);
        Assert.False(vm.IsInstalled);

        await using var old = FakeSysd.Running("com.example.app");
        var vm2 = new AppPageViewModel("com.example.app", new ServiceConnection(old.Client(), new FakeHost(true)), Strings, TestServices.AppPage());
        await vm2.LoadAsync();
        await vm2.UninstallAsync(keepData: false);
        Assert.Equal("AppUninstallUnavailableTitle", vm2.Notice!.Title);
        Assert.False(vm2.CanUninstall);
    }
}

public class LibraryProductTests
{
    private static readonly KeyStrings Strings = new();

    [Fact]
    public async Task ShowsLabels_Versions_Sizes_SortedByLabel_AndSearches()
    {
        await using var sysd = TestServices.PackageSysd([
            TestServices.Info("org.example.zeta", "Alpha Notes", 1_000),
            TestServices.Info("org.example.alpha", "Zeta Maps", 12_345_678),
        ]);
        var vm = new LibraryViewModel(new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings, "he-IL");

        await vm.RefreshAsync();

        Assert.False(vm.NamesOnly);
        Assert.Equal(["Alpha Notes", "Zeta Maps"], vm.Packages.Select(p => p.Label));
        Assert.Equal("1.2 (7)", vm.Packages[0].VersionText);

        vm.SearchText = "maps";
        Assert.Equal(["org.example.alpha"], vm.Packages.Select(p => p.PackageName));
        vm.SearchText = "org.example.zeta";
        Assert.Equal(["Alpha Notes"], vm.Packages.Select(p => p.Label));
        vm.SearchText = "nothing";
        Assert.True(vm.ShowNoMatches);
        Assert.False(vm.ShowEmptyState);
    }

    [Theory]
    [InlineData(0ul, "0 B")]
    [InlineData(999ul, "999 B")]
    [InlineData(1_500ul, "1.5 KB")]
    [InlineData(2_000_000_000ul, "2 GB")]
    public void Sizes_AreReadable(ulong bytes, string text) =>
        Assert.Equal(text, PackageList.FormatSize(bytes, System.Globalization.CultureInfo.InvariantCulture));
}

public sealed class DiagnosticsViewModelTests : IDisposable
{
    private static readonly KeyStrings Strings = new();
    private readonly string _dir = Path.Combine(Path.GetTempPath(), "andruna-tests-" + Guid.NewGuid().ToString("N"));

    public void Dispose()
    {
        if (Directory.Exists(_dir))
        {
            Directory.Delete(_dir, recursive: true);
        }
    }

    [Fact]
    public async Task ShowsService_Crashes_MissingFunctions_Logs_AndSavesAReport()
    {
        var paths = new AndrunaPaths(_dir);
        Directory.CreateDirectory(paths.Logs);
        File.WriteAllLines(Path.Combine(paths.Logs, "sysd.log"), Enumerable.Range(1, 300).Select(i => $"line {i}"));
        await using var sysd = TestServices.PackageSysd([TestServices.Info("com.example.app")], (id, req) => req.Kind switch
        {
            SysdWire.Kind.WaitEvents => TestWire.Encode(new SysdResponse.Events(new SysdEventBatch(3, 2, false, [
                new SysdEvent.NotificationPosted(1, 0, "com.example.app", 1, string.Empty, "c", "t", "x"),
                new SysdEvent.AppCrashed(2, 1_795_000_000_000, "com.example.app", 9, 0xC0000005, string.Empty),
            ])), id),
            SysdWire.Kind.GetMissingFunctions => TestWire.Encode(new SysdResponse.MissingFunctions(new MissingFunctionReport(5, [
                new("lx:epoll_pwait2", 1234), new("plain", 1)])), id),
            _ => null,
        });
        var vm = new DiagnosticsViewModel(new ServiceConnection(sysd.Client(), new FakeHost(true)), sysd.Client, paths, Strings, "en-US", "0.1.0");

        await vm.LoadAsync("com.example.app");

        Assert.True(vm.ServiceRunning);
        Assert.Equal("AboutServiceRunningFormat:0.1.0", vm.ServiceText);
        var crash = Assert.Single(vm.Crashes);
        Assert.Equal(("CrashTitle:Example", "CrashReasonAccessViolation"), (crash.Title, crash.Explanation));
        Assert.Equal(new DiagnosticsApp("com.example.app", "Example"), vm.SelectedApp);
        Assert.Equal(2, vm.MissingFunctions.Count);
        Assert.Equal(("lx", "epoll_pwait2"), (vm.MissingFunctions[0].Area, vm.MissingFunctions[0].Name));
        Assert.Equal((string.Empty, "plain"), (vm.MissingFunctions[1].Area, vm.MissingFunctions[1].Name));
        var lines = vm.LogText.Split(Environment.NewLine);
        Assert.Equal(DiagnosticsViewModel.LogLines, lines.Length);
        Assert.Equal("line 300", lines[^1]);
        vm.SelectedLog = 1;
        Assert.Equal("DiagLogEmpty", vm.LogText);

        var zip = Path.Combine(_dir, "report.zip");
        await vm.SaveReportAsync(zip);
        using var archive = ZipFile.OpenRead(zip);
        Assert.Equal(["logs/sysd.log", "summary.txt"], archive.Entries.Select(e => e.FullName).Order());
        using var summary = new StreamReader(archive.GetEntry("summary.txt")!.Open());
        var text = summary.ReadToEnd();
        Assert.Contains("lx:epoll_pwait2", text, StringComparison.Ordinal);
        Assert.Contains("CrashReasonAccessViolation", text, StringComparison.Ordinal);
    }

    [Fact]
    public async Task OlderService_SaysNotAvailable()
    {
        await using var sysd = FakeSysd.Running("com.example.app");
        var vm = new DiagnosticsViewModel(new ServiceConnection(sysd.Client(), new FakeHost(true)), sysd.Client, new AndrunaPaths(_dir), Strings, "en-US", "0.1.0");

        await vm.LoadAsync();

        Assert.Equal("AppNotAvailableYet", vm.CrashesNote);
        Assert.Equal("AppNotAvailableYet", vm.MissingNote);
        Assert.Equal("DiagLogEmpty", vm.LogText);
    }
}
