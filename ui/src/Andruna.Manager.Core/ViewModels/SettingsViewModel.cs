using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Mvvm;
using Andruna.Manager.Core.Settings;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>Version and architecture of the running app, for the About section.</summary>
public sealed record AppIdentity(string Version, string Architecture);

/// <summary>
/// Settings (M-5) and About (M-6). Changes are saved at once. A theme change
/// applies at once; a language change applies after a restart, because the
/// whole window, including its reading direction, is built for one language.
/// </summary>
public sealed class SettingsViewModel : ObservableObject
{
    private readonly SettingsStore _store;
    private readonly IStrings _strings;
    private readonly ServiceConnection _service;
    private readonly AppLanguage _startupLanguage;
    private ManagerSettings _settings;
    private string? _saveError;
    private string _serviceText;

    public SettingsViewModel(SettingsStore store, ManagerSettings current, AppLanguage startupLanguage, AppIdentity identity, ServiceConnection service, IStrings strings)
    {
        _store = store;
        _settings = current;
        _startupLanguage = startupLanguage;
        _service = service;
        _strings = strings;
        VersionText = strings.Format("AboutVersionFormat", identity.Version, identity.Architecture);
        _serviceText = strings.Get("AboutServiceChecking");
    }

    /// <summary>Raised after the theme setting changed, so the window can apply it.</summary>
    public event EventHandler<AppTheme>? ThemeChanged;

    public string VersionText { get; }

    public string ServiceText
    {
        get => _serviceText;
        private set => SetProperty(ref _serviceText, value);
    }

    /// <summary>Index into System, Hebrew, English (the order of <see cref="AppLanguage"/>).</summary>
    public int LanguageIndex
    {
        get => (int)_settings.Language;
        set
        {
            if (value < 0 || value == (int)_settings.Language)
            {
                return;
            }

            Update(_settings with { Language = (AppLanguage)value });
            OnPropertyChanged();
            OnPropertyChanged(nameof(RestartRequired));
        }
    }

    /// <summary>Index into System, Light, Dark (the order of <see cref="AppTheme"/>).</summary>
    public int ThemeIndex
    {
        get => (int)_settings.Theme;
        set
        {
            if (value < 0 || value == (int)_settings.Theme)
            {
                return;
            }

            Update(_settings with { Theme = (AppTheme)value });
            OnPropertyChanged();
            ThemeChanged?.Invoke(this, _settings.Theme);
        }
    }

    public bool RestartRequired => _settings.Language != _startupLanguage;

    /// <summary>Why the last change could not be saved, or null.</summary>
    public string? SaveError
    {
        get => _saveError;
        private set
        {
            if (SetProperty(ref _saveError, value))
            {
                OnPropertyChanged(nameof(HasSaveError));
            }
        }
    }

    public bool HasSaveError => SaveError is not null;

    public async Task LoadServiceInfoAsync()
    {
        var snapshot = await _service.CheckAsync();
        ServiceText = snapshot.State == ServiceState.Running
            ? _strings.Format("AboutServiceRunningFormat", snapshot.Status!.Version)
            : ServiceMessages.For(snapshot, _strings)!.Title;
    }

    private void Update(ManagerSettings next)
    {
        _settings = next;
        try
        {
            _store.Save(next);
            SaveError = null;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            SaveError = _strings.Get("SettingsSaveFailed");
        }
    }
}
