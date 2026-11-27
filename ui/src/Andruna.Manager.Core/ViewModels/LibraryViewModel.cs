using System.Collections.ObjectModel;
using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Mvvm;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>One installed app as a tile in the library.</summary>
public sealed record PackageTile(string PackageName, string Label, string VersionText);

/// <summary>
/// The library: the installed apps as reported by sysd. Nothing is shown that
/// sysd did not report; when sysd is unavailable the page says so.
/// </summary>
public sealed class LibraryViewModel : ObservableObject
{
    /// <summary>File types the install picker offers (P-3).</summary>
    public static readonly IReadOnlyList<string> PackageFileTypes = [".apk", ".apks", ".xapk", ".apkm"];

    private readonly ServiceConnection _service;
    private readonly IStrings _strings;
    private bool _isLoading;
    private UserMessage? _problem;
    private bool _loaded;

    public LibraryViewModel(ServiceConnection service, IStrings strings)
    {
        _service = service;
        _strings = strings;
        RefreshCommand = new AsyncCommand(RefreshAsync);
    }

    public ObservableCollection<PackageTile> Packages { get; } = [];

    public AsyncCommand RefreshCommand { get; }

    public bool IsLoading
    {
        get => _isLoading;
        private set
        {
            if (SetProperty(ref _isLoading, value))
            {
                OnPropertyChanged(nameof(ShowEmptyState));
            }
        }
    }

    /// <summary>Why the list cannot be shown, or null when sysd answered.</summary>
    public UserMessage? Problem
    {
        get => _problem;
        private set
        {
            if (SetProperty(ref _problem, value))
            {
                OnPropertyChanged(nameof(HasProblem));
                OnPropertyChanged(nameof(EmptyTitle));
                OnPropertyChanged(nameof(EmptyMessage));
            }
        }
    }

    public bool HasProblem => Problem is not null;

    public bool ShowEmptyState => _loaded && !IsLoading && Packages.Count == 0;

    public string EmptyTitle => HasProblem ? _strings.Get("LibraryNoServiceTitle") : _strings.Get("LibraryEmptyTitle");

    public string EmptyMessage => HasProblem ? _strings.Get("LibraryNoServiceMessage") : _strings.Get("LibraryEmptyMessage");

    public async Task RefreshAsync()
    {
        IsLoading = true;
        try
        {
            var snapshot = await _service.CheckAsync();
            Packages.Clear();
            if (snapshot.State != ServiceState.Running)
            {
                Problem = ServiceMessages.For(snapshot, _strings);
                return;
            }

            var list = await _service.Client.ListPackagesAsync();
            if (!list.IsOk)
            {
                Problem = ServiceMessages.ForOutcome(list.Outcome, list.Detail, _service.Host.IsInstalled, _strings);
                return;
            }

            Problem = null;
            foreach (var p in list.Value!.OrderBy(p => p.Label ?? p.Name, StringComparer.CurrentCultureIgnoreCase))
            {
                Packages.Add(new PackageTile(
                    p.Name,
                    string.IsNullOrWhiteSpace(p.Label) ? p.Name : p.Label,
                    string.IsNullOrEmpty(p.VersionName) ? string.Empty : _strings.Format("PackageVersionFormat", p.VersionName)));
            }
        }
        finally
        {
            _loaded = true;
            IsLoading = false;
            OnPropertyChanged(nameof(ShowEmptyState));
        }
    }

    /// <summary>
    /// The user chose a package file to install. Installing is not implemented
    /// yet, so the answer says so and names the file; nothing is installed.
    /// </summary>
    public UserMessage DescribeInstallRequest(string filePath) => new(
        _strings.Get("InstallUnavailableTitle"),
        _strings.Format("InstallUnavailableMessage", Path.GetFileName(filePath)));
}
