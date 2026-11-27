using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Mvvm;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>Where the install dialog stands.</summary>
public enum InstallStage
{
    /// <summary>sysd reads and verifies the file.</summary>
    Inspecting,

    /// <summary>The details are shown; the user confirms or cancels.</summary>
    Confirm,

    /// <summary>sysd installs (and compiles) the package.</summary>
    Installing,

    /// <summary>The package is installed.</summary>
    Done,

    /// <summary>Inspecting or installing failed; <see cref="InstallViewModel.Problem"/> says why.</summary>
    Failed,

    /// <summary>The running sysd cannot install packages yet.</summary>
    Unavailable,
}

/// <summary>
/// The install confirmation (P-10, P-12): sysd reads and verifies the
/// package file (InspectPackage), the dialog shows the app's name, icon,
/// version, signer and permissions, and installs on confirmation
/// (InstallPackage). Nothing is parsed in the manager: APK parsing and
/// signature checks are sysd's (04-security.md).
/// </summary>
public sealed class InstallViewModel : ObservableObject
{
    private readonly ServiceConnection _service;
    private readonly IStrings _strings;
    private readonly string _locale;
    private InstallStage _stage = InstallStage.Inspecting;
    private PackageDetails? _details;
    private InstallOutcome? _outcome;
    private UserMessage? _problem;

    public InstallViewModel(string filePath, ServiceConnection service, IStrings strings, string locale)
    {
        FilePath = filePath;
        _service = service;
        _strings = strings;
        _locale = locale;
    }

    public string FilePath { get; }

    public string FileName => Path.GetFileName(FilePath);

    public InstallStage Stage
    {
        get => _stage;
        private set
        {
            if (SetProperty(ref _stage, value))
            {
                OnPropertyChanged(nameof(IsBusy));
                OnPropertyChanged(nameof(CanInstall));
                OnPropertyChanged(nameof(StatusText));
            }
        }
    }

    public bool IsBusy => Stage is InstallStage.Inspecting or InstallStage.Installing;

    public bool CanInstall => Stage == InstallStage.Confirm;

    public PackageDetails? Details
    {
        get => _details;
        private set
        {
            if (SetProperty(ref _details, value))
            {
                OnPropertyChanged(nameof(HasDetails));
                OnPropertyChanged(nameof(Label));
                OnPropertyChanged(nameof(VersionText));
                OnPropertyChanged(nameof(SignerText));
                OnPropertyChanged(nameof(IconPath));
                OnPropertyChanged(nameof(ActionText));
                OnPropertyChanged(nameof(PermissionList));
            }
        }
    }

    public bool HasDetails => Details is not null;

    public string Label => Details is { } d && !string.IsNullOrWhiteSpace(d.Label) ? d.Label : Details?.Package ?? FileName;

    public string VersionText => Details is { } d
        ? d.InstalledVersionCode == 0
            ? _strings.Format("InstallVersionNew", Ltr(d.VersionName), d.VersionCode)
            : _strings.Format("InstallVersionUpdate", Ltr(d.VersionName), d.VersionCode, d.InstalledVersionCode)
        : string.Empty;

    /// <summary>The signing certificate's SHA-256 in groups of four, so it can be compared by eye.</summary>
    public string SignerText => Details is { SignerSha256.Length: > 0 } d
        ? _strings.Format("InstallSigner", FormatFingerprint(d.SignerSha256), d.SignatureScheme == 31 ? "3.1" : d.SignatureScheme.ToString(System.Globalization.CultureInfo.InvariantCulture))
        : string.Empty;

    public string IconPath => Details?.IconPngPath ?? string.Empty;

    /// <summary>"Install" or "Update".</summary>
    public string ActionText => _strings.Get(Details is { InstalledVersionCode: > 0 } ? "InstallActionUpdate" : "InstallActionInstall");

    public IReadOnlyList<PermissionItem> PermissionList => Details is { } d ? Permissions.Describe(d.Permissions, _strings) : [];

    public InstallOutcome? Outcome
    {
        get => _outcome;
        private set => SetProperty(ref _outcome, value);
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

    public string StatusText => Stage switch
    {
        InstallStage.Inspecting => _strings.Format("InstallInspecting", FileName),
        InstallStage.Installing => _strings.Format("InstallInstalling", Label),
        InstallStage.Done when Outcome is { } o => DoneText(o),
        _ => string.Empty,
    };

    private static string Ltr(string text) => "\u2066" + text + "\u2069";

    public static string FormatFingerprint(string hex) =>
        string.Join(' ', Enumerable.Range(0, (hex.Length + 3) / 4).Select(i => hex.Substring(i * 4, Math.Min(4, hex.Length - (i * 4))).ToUpperInvariant()));

    public async Task InspectAsync()
    {
        Stage = InstallStage.Inspecting;
        Problem = null;
        var snapshot = await _service.CheckAsync();
        if (snapshot.State != ServiceState.Running)
        {
            Problem = ServiceMessages.For(snapshot, _strings);
            Stage = InstallStage.Failed;
            return;
        }

        var r = await _service.Client.InspectPackageAsync([FilePath], _locale);
        if (r.IsOk)
        {
            Details = r.Value;
            Stage = InstallStage.Confirm;
            return;
        }

        Fail(r);
    }

    public async Task InstallAsync()
    {
        if (!CanInstall)
        {
            return;
        }

        Stage = InstallStage.Installing;
        var r = await _service.Client.InstallPackageAsync([FilePath]);
        if (r.IsOk)
        {
            Outcome = r.Value;
            Stage = InstallStage.Done;
            OnPropertyChanged(nameof(StatusText));
            return;
        }

        Fail(r);
    }

    private void Fail<T>(SysdResult<T> r)
    {
        if (r.IsUnsupported)
        {
            Problem = new UserMessage(_strings.Get("InstallUnavailableTitle"), _strings.Format("InstallUnavailableMessage", FileName));
            Stage = InstallStage.Unavailable;
            return;
        }

        Problem = r.Outcome == SysdOutcome.ServiceError && r.ErrorCode is { } code
            ? new UserMessage(_strings.Get("InstallFailedTitle"), ServiceMessages.ForError(code, _strings))
            : ServiceMessages.ForResult(r, _service.Host.IsInstalled, _strings);
        Stage = InstallStage.Failed;
    }

    private string DoneText(InstallOutcome o)
    {
        var done = _strings.Format(o.Updated ? "InstallDoneUpdated" : "InstallDoneInstalled", Label);
        return o.Compile switch
        {
            CompileStatus.Compiled or CompileStatus.NoCode => done,
            CompileStatus.Unavailable => done + " " + _strings.Get("InstallCompileUnavailable"),
            _ => done + " " + _strings.Get("InstallCompileFailed"),
        };
    }
}
