using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Mvvm;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>
/// The page of one installed app (M-2). The sections are laid out; their
/// actions are disabled until sysd implements them. The header shows only
/// what sysd reported for the package.
/// </summary>
public sealed class AppPageViewModel : ObservableObject
{
    private readonly ServiceConnection _service;
    private readonly IStrings _strings;
    private bool _isLoading;
    private UserMessage? _problem;
    private PackageSummary? _package;

    public AppPageViewModel(string packageName, ServiceConnection service, IStrings strings)
    {
        PackageName = packageName;
        _service = service;
        _strings = strings;
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

    /// <summary>The package name; sysd does not report app labels yet (protocol version 1).</summary>
    public string Title => PackageName;

    /// <summary>sysd does not report versions yet (protocol version 1).</summary>
    public string VersionText => _strings.Get("AppVersionUnknown");

    /// <summary>True once sysd reported the package as installed.</summary>
    public bool IsInstalled => _package is not null;

    /// <summary>True when the actions on this page can be used. Always false until sysd implements them.</summary>
    public bool ActionsAvailable => false;

    public async Task LoadAsync()
    {
        IsLoading = true;
        try
        {
            var snapshot = await _service.CheckAsync();
            if (snapshot.State != ServiceState.Running)
            {
                Problem = ServiceMessages.For(snapshot, _strings);
                return;
            }

            var list = await _service.Client.ListPackagesAsync();
            if (!list.IsOk)
            {
                Problem = ServiceMessages.ForResult(list, _service.Host.IsInstalled, _strings);
                return;
            }

            _package = list.Value!.FirstOrDefault(p => p.Name == PackageName);
            Problem = _package is null
                ? new UserMessage(_strings.Get("AppNotFoundTitle"), _strings.Format("AppNotFoundMessage", PackageName))
                : null;
            OnPropertyChanged(nameof(IsInstalled));
        }
        finally
        {
            IsLoading = false;
        }
    }
}
