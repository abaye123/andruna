using System.Collections.ObjectModel;
using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Mvvm;
using Andruna.Manager.Core.Platform;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>
/// One installed app as a tile in the library. <see cref="IconPath"/> is the
/// PNG sysd rendered from the APK, or empty when there is none.
/// </summary>
public sealed record PackageTile(string PackageName, string Label, string VersionText, string IconPath = "", string SizeText = "")
{
    public bool HasIcon => IconPath.Length > 0 && File.Exists(IconPath);
}

/// <summary>
/// The library: the installed apps as reported by sysd. Nothing is shown that
/// sysd did not report; when sysd is unavailable the page says so.
/// </summary>
public sealed class LibraryViewModel : ObservableObject
{
    /// <summary>File types the install picker offers (P-3).</summary>
    public static readonly IReadOnlyList<string> PackageFileTypes = LaunchRequest.PackageFileTypes;

    private readonly ServiceConnection _service;
    private readonly IStrings _strings;
    private readonly string _locale;
    private readonly List<PackageTile> _all = [];
    private bool _isLoading;
    private UserMessage? _problem;
    private bool _loaded;
    private bool _namesOnly;
    private string _searchText = string.Empty;

    public LibraryViewModel(ServiceConnection service, IStrings strings, string locale = "en-US")
    {
        _service = service;
        _strings = strings;
        _locale = locale;
        RefreshCommand = new AsyncCommand(RefreshAsync);
    }

    /// <summary>The tiles that match <see cref="SearchText"/>.</summary>
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

    /// <summary>True when sysd reported only package names (it does not know ListPackageInfo yet).</summary>
    public bool NamesOnly
    {
        get => _namesOnly;
        private set => SetProperty(ref _namesOnly, value);
    }

    /// <summary>Filters the tiles by label or package name, ignoring case.</summary>
    public string SearchText
    {
        get => _searchText;
        set
        {
            if (SetProperty(ref _searchText, value ?? string.Empty))
            {
                ApplyFilter();
            }
        }
    }

    /// <summary>The number of installed apps, before filtering.</summary>
    public int TotalCount => _all.Count;

    public bool ShowEmptyState => _loaded && !IsLoading && _all.Count == 0;

    /// <summary>True when apps are installed but none matches the search.</summary>
    public bool ShowNoMatches => _all.Count > 0 && Packages.Count == 0;

    public string EmptyTitle => HasProblem ? _strings.Get("LibraryNoServiceTitle") : _strings.Get("LibraryEmptyTitle");

    public string EmptyMessage => HasProblem ? _strings.Get("LibraryNoServiceMessage") : _strings.Get("LibraryEmptyMessage");

    public async Task RefreshAsync()
    {
        IsLoading = true;
        try
        {
            var snapshot = await _service.CheckAsync();
            _all.Clear();
            if (snapshot.State != ServiceState.Running)
            {
                Problem = ServiceMessages.For(snapshot, _strings);
                return;
            }

            var (list, hasDetails) = await PackageList.LoadAsync(_service.Client, _locale);
            if (!list.IsOk)
            {
                Problem = ServiceMessages.ForResult(list, _service.Host.IsInstalled, _strings);
                return;
            }

            Problem = null;
            NamesOnly = !hasDetails;
            foreach (var p in list.Value!)
            {
                _all.Add(p.Info is { } i
                    ? new PackageTile(p.Name, string.IsNullOrWhiteSpace(i.Label) ? p.Name : i.Label, PackageList.FormatVersion(i), i.IconPngPath, PackageList.FormatSize(i.SizeBytes))
                    : new PackageTile(p.Name, p.Name, string.Empty));
            }

            _all.Sort((a, b) => StringComparer.CurrentCultureIgnoreCase.Compare(a.Label, b.Label));
        }
        finally
        {
            ApplyFilter();
            _loaded = true;
            IsLoading = false;
            OnPropertyChanged(nameof(ShowEmptyState));
            OnPropertyChanged(nameof(TotalCount));
        }
    }

    private void ApplyFilter()
    {
        Packages.Clear();
        var q = SearchText.Trim();
        foreach (var t in _all.Where(t => q.Length == 0 ||
                     t.Label.Contains(q, StringComparison.CurrentCultureIgnoreCase) ||
                     t.PackageName.Contains(q, StringComparison.OrdinalIgnoreCase)))
        {
            Packages.Add(t);
        }

        OnPropertyChanged(nameof(ShowNoMatches));
    }
}
