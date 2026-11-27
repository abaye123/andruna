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
    public async Task Faulted_OnProtocolError()
    {
        await using var sysd = new FakeSysd((_, _) => "garbage"u8.ToArray());
        var snapshot = await new ServiceConnection(sysd.Client(), new FakeHost(true)).CheckAsync();

        Assert.Equal(ServiceState.Faulted, snapshot.State);
    }

    /// <summary>A client whose pipe only exists once the fake service started.</summary>
    private sealed class LateClient(Func<SysdClient?> current) : ISysdClient
    {
        private static readonly SysdClient Nothing = Absent();

        public Task<SysdResult<bool>> PingAsync(CancellationToken ct = default) => (current() ?? Nothing).PingAsync(ct);

        public Task<SysdResult<SysdStatus>> GetStatusAsync(CancellationToken ct = default) => (current() ?? Nothing).GetStatusAsync(ct);

        public Task<SysdResult<IReadOnlyList<PackageSummary>>> ListPackagesAsync(CancellationToken ct = default) =>
            (current() ?? Nothing).ListPackagesAsync(ct);
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
    public async Task RunningWithApps_ListsThemSortedByLabel()
    {
        await using var sysd = FakeSysd.Running(
            new PackageSummary("org.example.zeta", "Zeta", "2.0", 2),
            new PackageSummary("org.example.alpha", "alpha", null, 1),
            new PackageSummary("org.example.nolabel", null, "1.0", 1));
        var vm = new LibraryViewModel(new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings);

        await vm.RefreshAsync();

        Assert.False(vm.ShowEmptyState);
        Assert.Equal(["alpha", "org.example.nolabel", "Zeta"], vm.Packages.Select(p => p.Label));
        Assert.Equal("PackageVersionFormat:2.0", vm.Packages[2].VersionText);
        Assert.Equal(string.Empty, vm.Packages[0].VersionText);
    }

    [Fact]
    public async Task ListFailure_AfterGoodStatus_IsAProblem()
    {
        await using var sysd = new FakeSysd((method, id) => method == "status"
            ? FakeSysd.Result(id, new SysdStatus("0.1.0", 1, 1))
            : FakeSysd.Error(id, "internal", "boom"));
        var vm = new LibraryViewModel(new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings);

        await vm.RefreshAsync();

        Assert.Equal("ServiceFaultedTitle", vm.Problem!.Title);
        Assert.Contains("boom", vm.Problem.Message);
    }

    [Fact]
    public void InstallRequest_SaysInstallingIsNotAvailable_AndNamesTheFile()
    {
        var vm = new LibraryViewModel(new ServiceConnection(new SysdClient("x"), new FakeHost(false)), Strings);

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
        await using var sysd = FakeSysd.Running(new PackageSummary("org.example.one", "One", "3.1", 31));
        var vm = new AppPageViewModel("org.example.one", new ServiceConnection(sysd.Client(), new FakeHost(true)), Strings);

        await vm.LoadAsync();

        Assert.False(vm.HasProblem);
        Assert.Equal("One", vm.Title);
        Assert.Equal("PackageVersionFormat:3.1", vm.VersionText);
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
    }
}
