using System.Collections.ObjectModel;
using System.Globalization;
using System.IO.Compression;
using System.Text;
using Andruna.Manager.Core.Events;
using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Mvvm;
using Andruna.Manager.Core.Platform;
using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.ViewModels;

/// <summary>An installed app in the diagnostics app picker.</summary>
public sealed record DiagnosticsApp(string Package, string Label)
{
    public override string ToString() => Label == Package ? Package : $"{Label} ({Package})";
}

/// <summary>One row of the missing-function list.</summary>
public sealed record MissingFunctionRow(string Area, string Name, string CallsText);

/// <summary>A crash sysd still remembers, for the list on the page.</summary>
public sealed record CrashRow(string Package, string Title, string Explanation, string TimeText);

/// <summary>
/// Diagnostics (M-3): the state of the Andruna service, the app crashes sysd
/// remembers, the missing functions each app called most (03-components.md,
/// "missing function counters"), the end of the service logs, and a problem
/// report (a .zip of the logs and this information) the user can attach to
/// a bug report.
/// </summary>
public sealed class DiagnosticsViewModel : ObservableObject
{
    /// <summary>Lines of each log shown on the page.</summary>
    public const int LogLines = 200;

    private readonly ServiceConnection _service;
    private readonly Func<ISysdClient> _eventsClient;
    private readonly AndrunaPaths _paths;
    private readonly IStrings _strings;
    private readonly string _locale;
    private readonly string _appVersion;
    private string _serviceText;
    private string _serviceDetail = string.Empty;
    private bool _serviceRunning;
    private DiagnosticsApp? _selectedApp;
    private string _missingNote = string.Empty;
    private string _crashesNote = string.Empty;
    private int _selectedLog;
    private string _logText = string.Empty;
    private bool _isLoading;

    public DiagnosticsViewModel(ServiceConnection service, Func<ISysdClient> eventsClient, AndrunaPaths paths, IStrings strings, string locale, string appVersion)
    {
        _service = service;
        _eventsClient = eventsClient;
        _paths = paths;
        _strings = strings;
        _locale = locale;
        _appVersion = appVersion;
        _serviceText = strings.Get("AboutServiceChecking");
    }

    public string ServiceText
    {
        get => _serviceText;
        private set => SetProperty(ref _serviceText, value);
    }

    public string ServiceDetail
    {
        get => _serviceDetail;
        private set => SetProperty(ref _serviceDetail, value);
    }

    public bool ServiceRunning
    {
        get => _serviceRunning;
        private set => SetProperty(ref _serviceRunning, value);
    }

    public bool IsLoading
    {
        get => _isLoading;
        private set => SetProperty(ref _isLoading, value);
    }

    public ObservableCollection<DiagnosticsApp> Apps { get; } = [];

    public DiagnosticsApp? SelectedApp
    {
        get => _selectedApp;
        set
        {
            if (SetProperty(ref _selectedApp, value))
            {
                _ = LoadMissingAsync();
            }
        }
    }

    public ObservableCollection<MissingFunctionRow> MissingFunctions { get; } = [];

    /// <summary>Why the missing-function list is empty, or empty when it is not.</summary>
    public string MissingNote
    {
        get => _missingNote;
        private set => SetProperty(ref _missingNote, value);
    }

    public ObservableCollection<CrashRow> Crashes { get; } = [];

    public string CrashesNote
    {
        get => _crashesNote;
        private set => SetProperty(ref _crashesNote, value);
    }

    /// <summary>The log files shown: the service and the app processes.</summary>
    public static IReadOnlyList<string> LogFiles { get; } = ["sysd.log", "host.log"];

    public int SelectedLog
    {
        get => _selectedLog;
        set
        {
            if (value >= 0 && value < LogFiles.Count && SetProperty(ref _selectedLog, value))
            {
                LoadLog();
            }
        }
    }

    public string LogText
    {
        get => _logText;
        private set => SetProperty(ref _logText, value);
    }

    public string LogsDirectory => _paths.Logs;

    /// <summary>Loads everything; <paramref name="package"/> preselects an app (from the crash dialog).</summary>
    public async Task LoadAsync(string? package = null)
    {
        IsLoading = true;
        try
        {
            var snapshot = await _service.CheckAsync();
            ServiceRunning = snapshot.State == ServiceState.Running;
            if (ServiceRunning)
            {
                var s = snapshot.Status!;
                ServiceText = _strings.Format("AboutServiceRunningFormat", s.Version);
                ServiceDetail = _strings.Format("AboutServiceStatusFormat", SettingsViewModel.FormatUptime(s.UptimeMs), s.Hosts, s.Pid);
            }
            else
            {
                var problem = ServiceMessages.For(snapshot, _strings)!;
                ServiceText = problem.Title;
                ServiceDetail = problem.Message;
            }

            Apps.Clear();
            if (ServiceRunning)
            {
                var (list, _) = await PackageList.LoadAsync(_service.Client, _locale);
                foreach (var p in list.Value ?? [])
                {
                    Apps.Add(new DiagnosticsApp(p.Name, p.Info is { Label.Length: > 0 } i ? i.Label : p.Name));
                }
            }

            await LoadCrashesAsync();
            LoadLog();
            _selectedApp = Apps.FirstOrDefault(a => a.Package == package) ?? Apps.FirstOrDefault();
            OnPropertyChanged(nameof(SelectedApp));
            await LoadMissingAsync();

            if (Apps.Count == 0)
            {
                MissingFunctions.Clear();
                MissingNote = ServiceRunning ? _strings.Get("DiagNoApps") : _strings.Get("DiagNeedsService");
            }
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Writes the problem report: a summary and the logs, as a .zip.</summary>
    public async Task SaveReportAsync(string zipPath)
    {
        var summary = new StringBuilder();
        summary.AppendLine(CultureInfo.InvariantCulture, $"Andruna problem report, {DateTimeOffset.Now:yyyy-MM-dd HH:mm:ss zzz}");
        summary.AppendLine(CultureInfo.InvariantCulture, $"Manager {_appVersion}, Windows {Environment.OSVersion.Version}, {System.Runtime.InteropServices.RuntimeInformation.OSArchitecture}");
        summary.AppendLine(CultureInfo.InvariantCulture, $"Service: {ServiceText}. {ServiceDetail}");
        summary.AppendLine();
        summary.AppendLine("Crashes:");
        foreach (var c in Crashes)
        {
            summary.AppendLine(CultureInfo.InvariantCulture, $"  {c.TimeText} {c.Package}: {c.Explanation}");
        }

        summary.AppendLine();
        foreach (var app in Apps)
        {
            var r = await _service.Client.GetMissingFunctionsAsync(app.Package);
            if (r.IsOk && r.Value!.Entries.Count > 0)
            {
                summary.AppendLine(CultureInfo.InvariantCulture, $"Missing functions of {app.Package}:");
                foreach (var e in r.Value.Entries.Take(100))
                {
                    summary.AppendLine(CultureInfo.InvariantCulture, $"  {e.Calls,10} {e.Name}");
                }
            }
        }

        await Task.Run(() =>
        {
            if (File.Exists(zipPath))
            {
                File.Delete(zipPath);
            }

            using var zip = ZipFile.Open(zipPath, ZipArchiveMode.Create);
            using (var writer = new StreamWriter(zip.CreateEntry("summary.txt").Open(), new UTF8Encoding(false)))
            {
                writer.Write(summary.ToString());
            }

            if (Directory.Exists(_paths.Logs))
            {
                foreach (var log in Directory.GetFiles(_paths.Logs, "*.log*"))
                {
                    using var source = new FileStream(log, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                    using var target = zip.CreateEntry("logs/" + Path.GetFileName(log)).Open();
                    source.CopyTo(target);
                }
            }
        });
    }

    private async Task LoadCrashesAsync()
    {
        Crashes.Clear();
        if (!ServiceRunning)
        {
            CrashesNote = _strings.Get("DiagNeedsService");
            return;
        }

        // Everything sysd kept: another epoch makes sysd answer from its oldest event.
        var client = _eventsClient();
        try
        {
            var r = await client.WaitEventsAsync(0, 0, 0);
            if (r.IsOk)
            {
                foreach (var c in r.Value!.Events.OfType<SysdEvent.AppCrashed>().Reverse())
                {
                    var label = Apps.FirstOrDefault(a => a.Package == c.Package)?.Label ?? c.Package;
                    var report = NotificationBridge.Explain(c, label, _strings);
                    Crashes.Add(new CrashRow(c.Package, report.Title, report.Explanation, report.Time.ToLocalTime().ToString("g", CultureInfo.CurrentCulture)));
                }

                CrashesNote = Crashes.Count == 0 ? _strings.Get("DiagNoCrashes") : string.Empty;
            }
            else
            {
                CrashesNote = r.IsUnsupported ? _strings.Get("AppNotAvailableYet") : ServiceMessages.ForResult(r, _service.Host.IsInstalled, _strings).Message;
            }
        }
        finally
        {
            (client as IDisposable)?.Dispose();
        }
    }

    private async Task LoadMissingAsync()
    {
        MissingFunctions.Clear();
        if (SelectedApp is not { } app)
        {
            return;
        }

        var r = await _service.Client.GetMissingFunctionsAsync(app.Package);
        if (SelectedApp != app)
        {
            return;
        }

        if (r.IsOk)
        {
            foreach (var e in r.Value!.Entries)
            {
                var (area, name) = e.Name.IndexOf(':', StringComparison.Ordinal) is var i and > 0 ? (e.Name[..i], e.Name[(i + 1)..]) : (string.Empty, e.Name);
                MissingFunctions.Add(new MissingFunctionRow(area, name, e.Calls.ToString("N0", CultureInfo.CurrentCulture)));
            }

            MissingNote = MissingFunctions.Count == 0 ? _strings.Format("DiagNoMissing", app.Label) : string.Empty;
        }
        else
        {
            MissingNote = r.IsUnsupported ? _strings.Get("AppNotAvailableYet") : ServiceMessages.ForResult(r, _service.Host.IsInstalled, _strings).Message;
        }
    }

    private void LoadLog()
    {
        var lines = AndrunaPaths.Tail(Path.Combine(_paths.Logs, LogFiles[SelectedLog]), LogLines);
        LogText = lines.Count == 0 ? _strings.Get("DiagLogEmpty") : string.Join(Environment.NewLine, lines);
    }
}
