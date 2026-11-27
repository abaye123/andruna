using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Mvvm;
using Andruna.Manager.Core.Platform;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>
/// The page of one installed app (M-2): what sysd reports for the package
/// (label, icon, version, size), its data size on disk, the permissions its
/// APK requests (read by sysd), and the actions Open, Pin to Start and
/// Uninstall. Clearing data needs a sysd request that does not exist yet and
/// is shown as not available.
/// </summary>
public sealed class AppPageViewModel : ObservableObject
{
    private readonly ServiceConnection _service;
    private readonly IStrings _strings;
    private readonly AppPageServices _services;
    private bool _isLoading;
    private UserMessage? _problem;
    private UserMessage? _notice;
    private PackageSummary? _package;
    private IReadOnlyList<PermissionItem> _permissions = [];
    private string _permissionsNote = string.Empty;
    private string _dataSizeText = string.Empty;
    private bool _uninstalled;
    private bool _uninstallAvailable = true;

    public AppPageViewModel(string packageName, ServiceConnection service, IStrings strings, AppPageServices services)
    {
        PackageName = packageName;
        _service = service;
        _strings = strings;
        _services = services;
        _permissionsNote = strings.Get("AppPermissionsLoading");
    }

    public string PackageName { get; }

    public bool IsLoading
    {
        get => _isLoading;
        private set => SetProperty(ref _isLoading, value);
    }

    public UserMessage? Problem
    {
        get => _problem;
        private set
        {
            if (SetProperty(ref _problem, value))
            {
                OnPropertyChanged(nameof(HasProblem));
            }
        }
    }

    public bool HasProblem => Problem is not null;

    /// <summary>The result of the last action (opened, shortcut created, uninstalled), or null.</summary>
    public UserMessage? Notice
    {
        get => _notice;
        private set
        {
            if (SetProperty(ref _notice, value))
            {
                OnPropertyChanged(nameof(HasNotice));
            }
        }
    }

    public bool HasNotice => Notice is not null;

    private PackageInfo? Info => _package?.Info;

    public string Title => Info is { Label.Length: > 0 } i ? i.Label : PackageName;

    public string VersionText => Info is { } i ? _strings.Format("AppVersionFormat", PackageList.FormatVersion(i)) : _strings.Get("AppVersionUnknown");

    public string SizeText => Info is { } i ? PackageList.FormatSize(i.SizeBytes) : _strings.Get("AppNotAvailableYet");

    public string InstalledText => Info is { InstalledMs: > 0 } i
        ? DateTimeOffset.FromUnixTimeMilliseconds((long)i.InstalledMs).ToLocalTime().ToString("d", System.Globalization.CultureInfo.CurrentCulture)
        : string.Empty;

    public string DataSizeText
    {
        get => _dataSizeText;
        private set => SetProperty(ref _dataSizeText, value);
    }

    public string IconPath => Info?.IconPngPath ?? string.Empty;

    public bool HasIcon => IconPath.Length > 0 && File.Exists(IconPath);

    /// <summary>True once sysd reported the package as installed.</summary>
    public bool IsInstalled => _package is not null && !_uninstalled;

    public bool CanOpen => IsInstalled;

    /// <summary>Pinning needs the package's AppUserModelID, which comes with ListPackageInfo.</summary>
    public bool CanPin => IsInstalled && Info is not null;

    public bool CanUninstall => IsInstalled && _uninstallAvailable;

    /// <summary>No sysd request clears an app's data yet.</summary>
    public bool CanClearData => false;

    public IReadOnlyList<PermissionItem> PermissionList
    {
        get => _permissions;
        private set
        {
            if (SetProperty(ref _permissions, value))
            {
                OnPropertyChanged(nameof(HasPermissions));
            }
        }
    }

    public bool HasPermissions => PermissionList.Count > 0;

    /// <summary>Why no permissions are listed (loading, none requested, not available), or empty when they are.</summary>
    public string PermissionsNote
    {
        get => _permissionsNote;
        private set => SetProperty(ref _permissionsNote, value);
    }

    public async Task LoadAsync()
    {
        IsLoading = true;
        try
        {
            var snapshot = await _service.CheckAsync();
            if (snapshot.State != ServiceState.Running)
            {
                Problem = ServiceMessages.For(snapshot, _strings);
                PermissionsNote = string.Empty;
                return;
            }

            var (list, _) = await PackageList.LoadAsync(_service.Client, _services.Locale);
            if (!list.IsOk)
            {
                Problem = ServiceMessages.ForResult(list, _service.Host.IsInstalled, _strings);
                PermissionsNote = string.Empty;
                return;
            }

            _package = list.Value!.FirstOrDefault(p => p.Name == PackageName);
            Problem = _package is null
                ? new UserMessage(_strings.Get("AppNotFoundTitle"), _strings.Format("AppNotFoundMessage", PackageName))
                : null;
            RaiseHeader();
            if (_package is null)
            {
                PermissionsNote = string.Empty;
                return;
            }

            DataSizeText = PackageList.FormatSize(await Task.Run(() => AndrunaPaths.DirectorySize(_services.Paths.DataDir(PackageName))));
            await LoadPermissionsAsync();
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Opens the app through andruna-launch, as its Start menu shortcut does.</summary>
    public void Open()
    {
        Notice = _services.Launcher.TryOpen(PackageName, null, out var error)
            ? null
            : new UserMessage(_strings.Get("AppOpenFailedTitle"), _strings.Format("AppOpenFailedMessage", Title, error ?? string.Empty));
    }

    /// <summary>
    /// Makes sure the app has its Start menu shortcut (andruna-launch with
    /// the package's AppUserModelID) and tells the user how to pin it:
    /// Windows lets only the user pin desktop apps to Start.
    /// </summary>
    public void PinToStart()
    {
        if (Info is not { } info)
        {
            return;
        }

        var shortcut = new AppShortcut(Title, PackageName, null, info.AppUserModelId, info.IconIcoPath);
        try
        {
            var existed = _services.StartMenu.Exists(shortcut);
            if (!existed)
            {
                _services.StartMenu.Create(shortcut);
            }

            Notice = new UserMessage(_strings.Get("AppPinTitle"), _strings.Format("AppPinMessage", Title));
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or System.Runtime.InteropServices.COMException)
        {
            Notice = new UserMessage(_strings.Get("AppPinFailedTitle"), _strings.Format("AppPinFailedMessage", e.Message));
        }
    }

    /// <summary>Removes the app; its data is kept when <paramref name="keepData"/>.</summary>
    public async Task UninstallAsync(bool keepData)
    {
        var r = await _service.Client.UninstallPackageAsync(PackageName, keepData);
        if (r.IsOk)
        {
            _uninstalled = true;
            Notice = new UserMessage(_strings.Get("AppUninstalledTitle"), _strings.Format("AppUninstalledMessage", Title));
        }
        else if (r.IsUnsupported)
        {
            _uninstallAvailable = false;
            Notice = new UserMessage(_strings.Get("AppUninstallUnavailableTitle"), _strings.Get("AppUninstallUnavailableMessage"));
        }
        else
        {
            Notice = new UserMessage(
                _strings.Get("AppUninstallFailedTitle"),
                r.ErrorCode is { } code ? ServiceMessages.ForError(code, _strings) : ServiceMessages.ForResult(r, _service.Host.IsInstalled, _strings).Message);
        }

        RaiseHeader();
    }

    private async Task LoadPermissionsAsync()
    {
        var apks = _services.Paths.InstalledApks(PackageName);
        if (apks.Count == 0)
        {
            PermissionsNote = _strings.Get("AppPermissionsNoApk");
            return;
        }

        var r = await _service.Client.InspectPackageAsync(apks, _services.Locale);
        if (r.IsOk)
        {
            PermissionList = Permissions.Describe(r.Value!.Permissions, _strings);
            PermissionsNote = PermissionList.Count == 0 ? _strings.Get("AppPermissionsNone") : string.Empty;
        }
        else
        {
            PermissionsNote = r.IsUnsupported ? _strings.Get("AppNotAvailableYet") : _strings.Get("AppPermissionsFailed");
        }
    }

    private void RaiseHeader()
    {
        foreach (var name in new[]
                 {
                     nameof(Title), nameof(VersionText), nameof(SizeText), nameof(InstalledText), nameof(IconPath), nameof(HasIcon),
                     nameof(IsInstalled), nameof(CanOpen), nameof(CanPin), nameof(CanUninstall),
                 })
        {
            OnPropertyChanged(name);
        }
    }
}

/// <summary>What the app page works with besides sysd.</summary>
public sealed record AppPageServices(AndrunaPaths Paths, IAppLauncher Launcher, StartMenu StartMenu, string Locale);
