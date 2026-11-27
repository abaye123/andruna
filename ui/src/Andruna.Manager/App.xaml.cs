using System.Diagnostics;
using System.Globalization;
using System.Reflection;
using System.Runtime.InteropServices;
using Andruna.Manager.Core.Events;
using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Platform;
using Andruna.Manager.Core.Settings;
using Andruna.Manager.Core.Sysd;
using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Andruna.Manager.Notifications;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;
using Microsoft.Windows.AppNotifications;

namespace Andruna.Manager;

/// <summary>
/// The manager process. It has a window while the user has it open, and
/// keeps running without one while sysd runs, to bridge Android
/// notifications to Windows and show the crash dialog (andruna-launch
/// starts it with <c>--background</c>). Without a window and without sysd
/// it exits.
/// </summary>
public partial class App : Application
{
    private readonly AppActivationArguments _activation;
    private MainWindow? _window;
    private DispatcherQueue _dispatcher = null!;
    private Mutex? _runningMutex;
    private CancellationTokenSource? _bridgeStop;
    private volatile bool _bridgeRunning;
    private ToastPresenter? _toasts;

    public App(AppActivationArguments activation)
    {
        _activation = activation;
        InitializeComponent();
        DispatcherShutdownMode = DispatcherShutdownMode.OnExplicitShutdown;
    }

    /// <summary>Settings file of this user.</summary>
    public static SettingsStore SettingsStore { get; } = new(DebugOverrides.SettingsPath ?? SettingsStore.DefaultPath);

    /// <summary>Settings as loaded at startup, updated by the settings page.</summary>
    public static ManagerSettings StartupSettings { get; private set; } = new();

    /// <summary>The resource language in use (he-IL or en-US).</summary>
    public static string Language { get; private set; } = Languages.English;

    public static bool IsRightToLeft => Languages.IsRightToLeft(Language);

    public static FlowDirection FlowDirection => IsRightToLeft ? FlowDirection.RightToLeft : FlowDirection.LeftToRight;

    /// <summary>The pipe of this user's sysd (another instance only in Debug builds, for screenshots).</summary>
    public static string PipeName { get; } = DebugOverrides.PipeName ?? SysdEndpoint.PipeNameForCurrentUser();

    /// <summary>
    /// The connection to andruna-sysd of this user and session, shared by the
    /// pages. It is open while the window is, which keeps sysd running.
    /// </summary>
    public static ServiceConnection Service { get; } = new(
        new SysdClient(PipeName),
        new InstalledServiceHost(AppContext.BaseDirectory));

    /// <summary>The data tree, <c>%LOCALAPPDATA%\Andruna</c>.</summary>
    public static AndrunaPaths Paths { get; } = DebugOverrides.DataRoot is { } root ? new AndrunaPaths(root) : AndrunaPaths.ForCurrentUser();

    public static IAppLauncher Launcher { get; } = new LaunchExecutable(AppContext.BaseDirectory);

    public static StartMenu StartMenu { get; } = StartMenu.ForCurrentUser(AppContext.BaseDirectory);

    public static AppPageServices AppPageServices => new(Paths, Launcher, StartMenu, Language);

    /// <summary>How often the manager checks that its sysd connection is still open.</summary>
    private static readonly TimeSpan KeepAliveInterval = TimeSpan.FromSeconds(10);

    public static AppIdentity Identity { get; } = new(
        typeof(App).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion.Split('+')[0] ?? "0.0.0",
        RuntimeInformation.ProcessArchitecture switch
        {
            Architecture.Arm64 => "ARM64",
            Architecture.X64 => "x64",
            var other => other.ToString(),
        });

    public static App? Instance => Current as App;

    public static MainWindow? MainWindow => Instance?._window;

    /// <summary>A client of its own for the event stream (its connection does not keep sysd running).</summary>
    public static SysdClient NewEventsClient() => new(PipeName, callTimeout: TimeSpan.FromSeconds(5));

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _dispatcher = DispatcherQueue.GetForCurrentThread();
        StartupSettings = SettingsStore.Load();
        Language = Languages.Resolve(StartupSettings.Language, CultureInfo.CurrentUICulture);
        Loc.Strings = new ResourceStrings(Language);
        var culture = CultureInfo.GetCultureInfo(Language);
        CultureInfo.DefaultThreadCurrentUICulture = culture;
        CultureInfo.CurrentUICulture = culture;

        // andruna-launch checks this mutex to see whether the manager runs.
        _runningMutex = new Mutex(false, $@"Local\Andruna.Manager.{SysdEndpoint.CurrentUserSid().Value}");

        _toasts = new ToastPresenter(Language);
        _toasts.Invoked += (_, package) => _dispatcher.TryEnqueue(() => OpenFromNotification(package));
        AppInstance.GetCurrent().Activated += (_, a) => _dispatcher.TryEnqueue(() => HandleActivation(a, redirected: true));

        HandleActivation(_activation, redirected: false);
    }

    /// <summary>Opens the window (or brings it forward) for a request.</summary>
    public void Show(LaunchRequest request)
    {
        if (_window is null)
        {
            _window = new MainWindow(request);
            _window.Closed += (_, _) => OnWindowClosed();
            _window.Activate();
            Service.StartKeepAlive(KeepAliveInterval);
        }
        else
        {
            _window.Open(request);
            _window.BringToFront();
        }

        if (request.InstallFile is { } file)
        {
            _window.ShowInstall(file);
        }

        EnsureBridge();
    }

    /// <summary>Starts a new instance on the settings page and ends this one.</summary>
    public static void Restart()
    {
        var exe = Environment.ProcessPath;
        AppInstance.GetCurrent().UnregisterKey();
        if (exe is not null)
        {
            Process.Start(new ProcessStartInfo(exe, "--page settings") { UseShellExecute = false });
        }

        Current.Exit();
    }

    private void HandleActivation(AppActivationArguments activation, bool redirected)
    {
        switch (activation.Kind)
        {
            case ExtendedActivationKind.AppNotification when activation.Data is AppNotificationActivatedEventArgs n:
                OpenFromNotification(NotificationBridge.PackageToOpen(n.Argument));
                EnsureBridge();
                ExitIfIdle();
                break;
            default:
                var args = redirected && activation.Data is Windows.ApplicationModel.Activation.ILaunchActivatedEventArgs launch
                    ? LaunchRequest.SplitCommandLine(launch.Arguments)
                    : Environment.GetCommandLineArgs().Skip(1).ToList();
                var request = LaunchRequest.Parse(args);
                if (request.Background && request.InstallFile is null)
                {
                    EnsureBridge();
                    ExitIfIdle();
                }
                else
                {
                    Show(request);
                }

                break;
        }
    }

    private void OpenFromNotification(string? package)
    {
        if (package is not null && !Launcher.TryOpen(package, null, out _))
        {
            Show(new LaunchRequest("app", package));
        }
    }

    /// <summary>Starts the notification and crash bridge once per process.</summary>
    private void EnsureBridge()
    {
        if (_bridgeRunning)
        {
            return;
        }

        _bridgeRunning = true;

        var labels = new LabelCache(PipeName, Language);
        var crashes = new CrashPresenter(_dispatcher);
        var sink = new NotificationBridge(_toasts!, crashes, labels.LabelOf, Loc.Strings);
        var client = NewEventsClient();
        var pump = new EventPump(client, sink);
        _bridgeStop = new CancellationTokenSource();
        var stop = _bridgeStop.Token;
        _ = Task.Run(async () =>
        {
            try
            {
                await pump.RunAsync(() => _window is not null || crashes.IsShowing, stop);
            }
            finally
            {
                client.Dispose();
                _bridgeRunning = false;
                _dispatcher.TryEnqueue(ExitIfIdle);
            }
        });
    }

    private void OnWindowClosed()
    {
        _window = null;
        Service.StopKeepAlive();

        // The window's connection kept sysd running; without the window,
        // sysd may exit when no app runs, and this process with it.
        (Service.Client as SysdClient)?.Disconnect();
        ExitIfIdle();
    }

    /// <summary>Ends the process when there is no window, no crash dialog and the bridge has ended.</summary>
    public void ExitIfIdle()
    {
        if (_window is null && !CrashPresenter.AnyShowing && !_bridgeRunning)
        {
            _bridgeStop?.Cancel();
            _runningMutex?.Dispose();
            Exit();
        }
    }
}

/// <summary>
/// Labels of installed apps for notifications and crash dialogs, asked from
/// sysd on a short connection the first time a package is seen.
/// </summary>
internal sealed class LabelCache(string pipeName, string locale)
{
    private readonly Dictionary<string, string> _labels = new(StringComparer.Ordinal);
    private readonly Lock _lock = new();

    public string? LabelOf(string package)
    {
        lock (_lock)
        {
            if (_labels.TryGetValue(package, out var known))
            {
                return known;
            }
        }

        using var client = new SysdClient(pipeName, callTimeout: TimeSpan.FromSeconds(2));
        var r = client.ListPackageInfoAsync(locale).GetAwaiter().GetResult();
        lock (_lock)
        {
            foreach (var p in r.Value ?? [])
            {
                _labels[p.Package] = string.IsNullOrWhiteSpace(p.Label) ? p.Package : p.Label;
            }

            return _labels.GetValueOrDefault(package);
        }
    }
}
