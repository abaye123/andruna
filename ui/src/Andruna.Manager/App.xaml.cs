using System.Diagnostics;
using System.Globalization;
using System.Reflection;
using System.Runtime.InteropServices;
using Andruna.Manager.Core.Localization;
using Andruna.Manager.Core.Settings;
using Andruna.Manager.Core.Sysd;
using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Microsoft.UI.Xaml;

namespace Andruna.Manager;

/// <summary>Where the window starts, from the command line.</summary>
/// <remarks>
/// <c>--page library</c> (default), <c>--page settings</c>, or
/// <c>--app &lt;package&gt;</c> for the page of one installed app.
/// </remarks>
public sealed record StartPage(string Page, string? Package)
{
    public static StartPage Parse(IReadOnlyList<string> args)
    {
        var page = "library";
        string? package = null;
        for (var i = 0; i < args.Count - 1; i++)
        {
            if (args[i] == "--page")
            {
                page = args[i + 1];
            }
            else if (args[i] == "--app")
            {
                page = "app";
                package = args[i + 1];
            }
        }

        return new StartPage(page, package);
    }
}

public partial class App : Application
{
    private MainWindow? _window;

    public App()
    {
        InitializeComponent();
    }

    /// <summary>Settings file of this user.</summary>
    public static SettingsStore SettingsStore { get; } = new(SettingsStore.DefaultPath);

    /// <summary>Settings as loaded at startup, updated by the settings page.</summary>
    public static ManagerSettings StartupSettings { get; private set; } = new();

    /// <summary>The resource language in use (he-IL or en-US).</summary>
    public static string Language { get; private set; } = Languages.English;

    public static bool IsRightToLeft => Languages.IsRightToLeft(Language);

    public static FlowDirection FlowDirection => IsRightToLeft ? FlowDirection.RightToLeft : FlowDirection.LeftToRight;

    /// <summary>The connection to andruna-sysd, shared by all pages.</summary>
    public static ServiceConnection Service { get; } = new(
        new SysdClient(),
        new InstalledServiceHost(AppContext.BaseDirectory));

    public static AppIdentity Identity { get; } = new(
        typeof(App).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion.Split('+')[0] ?? "0.0.0",
        RuntimeInformation.ProcessArchitecture switch
        {
            Architecture.Arm64 => "ARM64",
            Architecture.X64 => "x64",
            var other => other.ToString(),
        });

    public static MainWindow? MainWindow => (Current as App)?._window;

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        StartupSettings = SettingsStore.Load();
        Language = Languages.Resolve(StartupSettings.Language, CultureInfo.CurrentUICulture);
        Loc.Strings = new ResourceStrings(Language);
        var culture = CultureInfo.GetCultureInfo(Language);
        CultureInfo.DefaultThreadCurrentUICulture = culture;
        CultureInfo.CurrentUICulture = culture;

        _window = new MainWindow(StartPage.Parse(Environment.GetCommandLineArgs().Skip(1).ToList()));
        _window.Activate();
    }

    /// <summary>Starts a new instance on the settings page and closes this one.</summary>
    public static void Restart()
    {
        var exe = Environment.ProcessPath;
        if (exe is not null)
        {
            Process.Start(new ProcessStartInfo(exe, "--page settings") { UseShellExecute = false });
        }

        Current.Exit();
    }
}
