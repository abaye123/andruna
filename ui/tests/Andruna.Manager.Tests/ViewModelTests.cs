using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Settings;
using Andruna.Manager.Core.Sysd;
using Andruna.Manager.Core.ViewModels;

namespace Andruna.Manager.Tests;

public class ServiceConnectionTests
{
    private static SysdClient Absent() =>
        new("andruna-test-absent-" + Guid.NewGuid().ToString("N"), TimeSpan.FromMilliseconds(100));

    [Fact]
    public async Task NotInstalled_WhenExecutableMissing()
    {
        var host = new FakeHost(installed: false);
        var snapshot = await new ServiceConnection(Absent(), host).CheckAsync();

        Assert.Equal(ServiceState.NotInstalled, snapshot.State);
        Assert.Equal(0, host.StartCalls);
    }

    [Fact]
    public async Task NotRunning_WhenStartFails_AndStartIsTriedOnce()
    {
        var host = new FakeHost(installed: true, start: () => false);
        var connection = new ServiceConnection(Absent(), host, TimeSpan.FromMilliseconds(200));

        Assert.Equal(ServiceState.NotRunning, (await connection.CheckAsync()).State);
        Assert.Equal(ServiceState.NotRunning, (await connection.CheckAsync()).State);
        Assert.Equal(1, host.StartCalls);
    }

    [Fact]
    public async Task Running_AfterStartingTheService()
    {
        FakeSysd? sysd = null;
        var host = new FakeHost(installed: true, start: () =>
        {
            sysd = FakeSysd.Running();
            return true;
        });

        // The client looks for the pipe the started service creates.
        var connection = new ServiceConnection(new LateClient(() => sysd?.Client()), host, TimeSpan.FromSeconds(3));
        var snapshot = await connection.CheckAsync();
        await sysd!.DisposeAsync();

        Assert.Equal(ServiceState.Running, snapshot.State);
        Assert.Equal("0.1.0", snapshot.Status!.Version);
    }

    [Fact]
    public async Task Start_IsTriedAgain_AfterTheCooldown()
    {
        var host = new FakeHost(installed: true, start: () => false);
        var connection = new ServiceConnection(Absent(), host, TimeSpan.FromMilliseconds(50), TimeSpan.Zero);

        await connection.CheckAsync();
        await connection.CheckAsync();

        Assert.Equal(2, host.StartCalls);
    }

    [Fact]
    public async Task Faulted_OnProtocolError()
    {
        await using var sysd = new FakeSysd((_, _) => "garbage!garbage"u8.ToArray());
        var snapshot = await new ServiceConnection(sysd.Client(), new FakeHost(true)).CheckAsync();

        Assert.Equal(ServiceState.Faulted, snapshot.State);
        Assert.Null(snapshot.ErrorCode);
    }

    [Fact]
    public async Task Faulted_WithCode_OnServiceError()
    {
        await using var sysd = new FakeSysd((id, _) => TestWire.Encode(new SysdResponse.Error(SysdErrorCode.Busy, "busy"), id));
        var snapshot = await new ServiceConnection(sysd.Client(), new FakeHost(true)).CheckAsync();

        Assert.Equal(ServiceState.Faulted, snapshot.State);
        Assert.Equal(SysdErrorCode.Busy, snapshot.ErrorCode);
        Assert.Equal(
            new UserMessage("ServiceErrorTitle", "SysdErrorBusy"),
            ServiceMessages.For(snapshot, new KeyStrings()));
    }

    [Fact]
    public async Task KeepAlive_ReopensTheConnection_AfterSysdRestarts()
    {
        var first = FakeSysd.Running();
        var client = new SysdClient(first.PipeName, TimeSpan.FromMilliseconds(200), TimeSpan.FromSeconds(1));
        var connection = new ServiceConnection(client, new FakeHost(true, () => false), TimeSpan.FromMilliseconds(50));
        Assert.Equal(ServiceState.Running, (await connection.CheckAsync()).State);
        Assert.True(client.IsConnected);

        // sysd exits: the kept connection breaks; a new sysd appears on the same pipe.
        await first.DisposeAsync();
        await using var second = new FakeSysdOnPipe(first.PipeName);
        connection.StartKeepAlive(TimeSpan.FromMilliseconds(50));
        try
        {
            var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(5);
            while (second.Sysd.Connections == 0 && DateTime.UtcNow < deadline)
            {
                await Task.Delay(20);
            }
        }
        finally
        {
            connection.StopKeepAlive();
        }

        Assert.Equal(1, second.Sysd.Connections);
        Assert.Equal(ServiceState.Running, (await connection.CheckAsync()).State);
    }

    [Theory]
    [InlineData(SysdErrorCode.UnsupportedVersion, "SysdErrorUnsupportedVersion")]
    [InlineData(SysdErrorCode.Malformed, "SysdErrorMalformed")]
    [InlineData(SysdErrorCode.UnknownRequest, "SysdErrorUnknownRequest")]
    [InlineData(SysdErrorCode.InvalidPackageName, "SysdErrorInvalidPackageName")]
    [InlineData(SysdErrorCode.NotInstalled, "SysdErrorNotInstalled")]
    [InlineData(SysdErrorCode.NotRunning, "SysdErrorNotRunning")]
    [InlineData(SysdErrorCode.NoWindow, "SysdErrorNoWindow")]
    [InlineData(SysdErrorCode.HostStartFailed, "SysdErrorHostStartFailed")]
    [InlineData(SysdErrorCode.Busy, "SysdErrorBusy")]
    [InlineData(SysdErrorCode.Internal, "SysdErrorInternal")]
    [InlineData((SysdErrorCode)999, "SysdErrorOther:999")]
    public void EveryErrorCode_HasAMessage(SysdErrorCode code, string expected) =>
        Assert.Equal(expected, ServiceMessages.ForError(code, new KeyStrings()));

    /// <summary>A FakeSysd whose pipe name is chosen by the test.</summary>
    private sealed class FakeSysdOnPipe(string pipeName) : IAsyncDisposable
    {
        public FakeSysd Sysd { get; } = FakeSysd.RunningOn(pipeName);

        public ValueTask DisposeAsync() => Sysd.DisposeAsync();
    }

    /// <summary>A client whose pipe only exists once the fake service started.</summary>
    private sealed class LateClient(Func<SysdClient?> current) : ISysdClient
    {
        private static readonly SysdClient Nothing = Absent();

        public Task<SysdResult<ushort>> PingAsync(CancellationToken ct = default) => (current() ?? Nothing).PingAsync(ct);

        public Task<SysdResult<SysdStatus>> GetStatusAsync(CancellationToken ct = default) => (current() ?? Nothing).GetStatusAsync(ct);

        public Task<SysdResult<IReadOnlyList<PackageSummary>>> ListPackagesAsync(CancellationToken ct = default) =>
            (current() ?? Nothing).ListPackagesAsync(ct);

        public Task<SysdResult<SysdResponse.Started>> StartPackageAsync(string package, CancellationToken ct = default) =>
            (current() ?? Nothing).StartPackageAsync(package, ct);

        public Task<SysdResult<bool>> FocusWindowAsync(string package, CancellationToken ct = default) =>
            (current() ?? Nothing).FocusWindowAsync(package, ct);
    }
}

public class LibraryViewModelTests
{
    private static readonly KeyStrings Strings = new();

    [Fact]
    public async Task ServiceNotInstalled_ShowsProblemAndEmptyState_NoPackages()
    {
        var client = new SysdClient("andruna-test-absent-" + Guid.NewGuid().ToString("N"), TimeSpan.FromMilliseconds(100));
        var vm = new LibraryViewModel(new ServiceConnection(client, new FakeHost(false)), Strings);

        await vm.RefreshAsync();

        Assert.True(vm.HasProblem);
        Assert.Equal("ServiceNotInstalledTitle", vm.Problem!.Title);
        Assert.True(vm.ShowEmptyState);
        Assert.Equal("LibraryNoServiceTitle", vm.EmptyTitle);
        Assert.Empty(vm.Packages);
        Assert.False(vm.IsLoading);
    }

    [Fact]
    public async Task RunningWithoutApps_ShowsEmptyLibrary()
    {
        await using var sysd = FakeSysd.Running();
        var vm = new LibraryViewModel(new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings);

        await vm.RefreshAsync();

        Assert.False(vm.HasProblem);
        Assert.True(vm.ShowEmptyState);
        Assert.Equal("LibraryEmptyTitle", vm.EmptyTitle);
    }

    [Fact]
    public async Task RunningWithApps_ListsThemByName_WithoutInventedDetails()
    {
        await using var sysd = FakeSysd.Running("org.example.zeta", "org.example.alpha");
        var vm = new LibraryViewModel(new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings);

        await vm.RefreshAsync();

        Assert.False(vm.ShowEmptyState);
        Assert.Equal(["org.example.alpha", "org.example.zeta"], vm.Packages.Select(p => p.Label));
        Assert.All(vm.Packages, p => Assert.Equal(string.Empty, p.VersionText));
    }

    [Fact]
    public async Task ListFailure_AfterGoodStatus_IsAProblem_InTheUsersLanguage()
    {
        await using var sysd = new FakeSysd((id, req) => req.Kind == SysdWire.Kind.Status
            ? FakeSysd.Answer(id, req, [])
            : TestWire.Encode(new SysdResponse.Error(SysdErrorCode.Internal, "cannot read the package directory"), id));
        var vm = new LibraryViewModel(new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings);

        await vm.RefreshAsync();

        Assert.Equal(new UserMessage("ServiceErrorTitle", "SysdErrorInternal"), vm.Problem);
    }

    [Fact]
    public void InstallRequest_SaysInstallingIsNotAvailable_AndNamesTheFile()
    {
        using var client = new SysdClient("x");
        var vm = new LibraryViewModel(new ServiceConnection(client, new FakeHost(false)), Strings);

        var answer = vm.DescribeInstallRequest(@"C:\Users\someone\Downloads\app-release.apk");

        Assert.Equal("InstallUnavailableTitle", answer.Title);
        Assert.Equal("InstallUnavailableMessage:app-release.apk", answer.Message);
    }
}

public class AppPageViewModelTests
{
    private static readonly KeyStrings Strings = new();

    [Fact]
    public async Task KnownPackage_ShowsWhatSysdReported()
    {
        await using var sysd = FakeSysd.Running("org.example.one");
        var vm = new AppPageViewModel("org.example.one", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings);

        await vm.LoadAsync();

        Assert.False(vm.HasProblem);
        Assert.True(vm.IsInstalled);
        Assert.Equal("org.example.one", vm.Title);
        Assert.Equal("AppVersionUnknown", vm.VersionText);
        Assert.False(vm.ActionsAvailable);
    }

    [Fact]
    public async Task UnknownPackage_IsNotFound()
    {
        await using var sysd = FakeSysd.Running();
        var vm = new AppPageViewModel("org.example.gone", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings);

        await vm.LoadAsync();

        Assert.Equal("AppNotFoundTitle", vm.Problem!.Title);
        Assert.Equal("org.example.gone", vm.Title);
        Assert.Equal("AppVersionUnknown", vm.VersionText);
    }

    [Fact]
    public async Task NoService_ShowsPackageNameOnly()
    {
        var client = new SysdClient("andruna-test-absent-" + Guid.NewGuid().ToString("N"), TimeSpan.FromMilliseconds(100));
        var vm = new AppPageViewModel("org.example.one", new ServiceConnection(client, new FakeHost(true, () => false)), Strings);

        await vm.LoadAsync();

        Assert.Equal("ServiceNotRunningTitle", vm.Problem!.Title);
        Assert.Equal("org.example.one", vm.Title);
        Assert.Equal("AppVersionUnknown", vm.VersionText);
    }
}

public sealed class SettingsTests : IDisposable
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

    private SettingsViewModel CreateVm(SettingsStore store, ManagerSettings current) => new(
        store,
        current,
        current.Language,
        new AppIdentity("0.1.0", "x64"),
        new ServiceConnection(new SysdClient("andruna-test-absent-" + Guid.NewGuid().ToString("N"), TimeSpan.FromMilliseconds(100)), new FakeHost(false)),
        Strings);

    [Fact]
    public void Store_MissingFile_GivesDefaults()
    {
        var settings = new SettingsStore(Path.Combine(_dir, "none.json")).Load();

        Assert.Equal(AppLanguage.System, settings.Language);
        Assert.Equal(AppTheme.System, settings.Theme);
    }

    [Fact]
    public void Store_CorruptFile_GivesDefaults()
    {
        Directory.CreateDirectory(_dir);
        var path = Path.Combine(_dir, "manager.json");
        File.WriteAllText(path, "{ \"language\": 7 ");

        Assert.Equal(new ManagerSettings(), new SettingsStore(path).Load());
    }

    [Fact]
    public void Store_RoundTrips_AsReadableJson()
    {
        var store = new SettingsStore(Path.Combine(_dir, "sub", "manager.json"));
        store.Save(new ManagerSettings { Language = AppLanguage.Hebrew, Theme = AppTheme.Dark });

        Assert.Equal(new ManagerSettings { Language = AppLanguage.Hebrew, Theme = AppTheme.Dark }, store.Load());
        Assert.Contains("\"hebrew\"", File.ReadAllText(store.Path));
    }

    [Fact]
    public void LanguageChange_IsSaved_AndNeedsRestart()
    {
        var store = new SettingsStore(Path.Combine(_dir, "manager.json"));
        var vm = CreateVm(store, new ManagerSettings());

        vm.LanguageIndex = (int)AppLanguage.Hebrew;

        Assert.True(vm.RestartRequired);
        Assert.Equal(AppLanguage.Hebrew, store.Load().Language);

        vm.LanguageIndex = (int)AppLanguage.System;
        Assert.False(vm.RestartRequired);
    }

    [Fact]
    public void ThemeChange_IsSaved_AndRaised()
    {
        var store = new SettingsStore(Path.Combine(_dir, "manager.json"));
        var vm = CreateVm(store, new ManagerSettings());
        AppTheme? raised = null;
        vm.ThemeChanged += (_, t) => raised = t;

        vm.ThemeIndex = (int)AppTheme.Dark;

        Assert.Equal(AppTheme.Dark, raised);
        Assert.Equal(AppTheme.Dark, store.Load().Theme);
        Assert.False(vm.RestartRequired);
    }

    [Fact]
    public void SaveFailure_IsShown()
    {
        Directory.CreateDirectory(_dir);

        // The settings path is an existing directory, so the file cannot be written.
        var blocked = Path.Combine(_dir, "blocked");
        Directory.CreateDirectory(blocked);
        Directory.CreateDirectory(blocked + ".tmp");
        var vm = CreateVm(new SettingsStore(blocked), new ManagerSettings());

        vm.ThemeIndex = (int)AppTheme.Light;

        Assert.Equal("SettingsSaveFailed", vm.SaveError);
    }

    [Fact]
    public async Task About_ReportsServiceState()
    {
        var vm = CreateVm(new SettingsStore(Path.Combine(_dir, "m.json")), new ManagerSettings());

        Assert.Equal("AboutVersionFormat:0.1.0|x64", vm.VersionText);
        await vm.LoadServiceInfoAsync();
        Assert.Equal("ServiceNotInstalledTitle", vm.ServiceText);
        Assert.Equal("ServiceNotInstalledMessage", vm.ServiceDetail);
    }

    [Fact]
    public async Task About_ShowsVersionUptimeAndHosts_WhenRunning()
    {
        await using var sysd = new FakeSysd((id, _) =>
            TestWire.Encode(new SysdResponse.StatusReply(new SysdStatus(4321, 90_061_000, 1, 2, 30_000, "0.1.0")), id));
        var vm = new SettingsViewModel(
            new SettingsStore(Path.Combine(_dir, "m.json")),
            new ManagerSettings(),
            AppLanguage.System,
            new AppIdentity("0.1.0", "x64"),
            new ServiceConnection(sysd.Client(), new FakeHost(true)),
            Strings);

        await vm.LoadServiceInfoAsync();

        Assert.Equal("AboutServiceRunningFormat:0.1.0", vm.ServiceText);
        Assert.Equal("AboutServiceStatusFormat:25:01:01|2|4321", vm.ServiceDetail);
        Assert.True(vm.HasServiceDetail);
    }

    [Theory]
    [InlineData(0UL, "0:00:00")]
    [InlineData(59_999UL, "0:00:59")]
    [InlineData(3_600_000UL, "1:00:00")]
    [InlineData(ulong.MaxValue, "256204778:48:05")]
    public void Uptime_IsHoursMinutesSeconds(ulong ms, string expected) =>
        Assert.Equal(expected, SettingsViewModel.FormatUptime(ms));
}
