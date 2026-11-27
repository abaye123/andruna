using Andruna.Manager.Core.Events;
using Andruna.Manager.Core.Platform;
using Andruna.Manager.Localization;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;

namespace Andruna.Manager.Notifications;

/// <summary>Shows crash dialogs on the UI thread, one per crash, at most a few at a time.</summary>
internal sealed class CrashPresenter(DispatcherQueue dispatcher) : ICrashPresenter
{
    private const int MaxOpen = 3;
    private static int _open;

    /// <summary>True while any crash dialog is open (the process then stays).</summary>
    public static bool AnyShowing => Volatile.Read(ref _open) > 0;

    public bool IsShowing => AnyShowing;

    public void ShowCrash(CrashReport report) => dispatcher.TryEnqueue(() =>
    {
        if (_open >= MaxOpen)
        {
            return;
        }

        Interlocked.Increment(ref _open);
        var window = new CrashWindow(report);
        window.Closed += (_, _) =>
        {
            Interlocked.Decrement(ref _open);
            App.Instance?.ExitIfIdle();
        };
        window.Activate();
    });
}

/// <summary>
/// The crash dialog (P-23): which app stopped, why in plain language, and
/// what the user can do: open it again, report the problem (the diagnostics
/// page with a problem report for that app), or close.
/// </summary>
internal sealed class CrashWindow : Window
{
    public CrashWindow(CrashReport report)
    {
        Title = Loc.Get("CrashWindowTitle");
        SystemBackdrop = new MicaBackdrop();
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "Andruna.ico"));
        if (App.IsRightToLeft)
        {
            NativeWindow.SetRightToLeftLayout(this);
        }

        var scale = NativeWindow.GetScale(this);
        AppWindow.Resize(new SizeInt32((int)(520 * scale), (int)(300 * scale)));
        if (AppWindow.Presenter is OverlappedPresenter p)
        {
            p.IsMaximizable = false;
            p.IsMinimizable = false;
            p.IsResizable = false;
        }

        var reopen = new Button { Content = Loc.Get("CrashReopen"), Style = (Style)Application.Current.Resources["AccentButtonStyle"] };
        reopen.Click += (_, _) =>
        {
            App.Launcher.TryOpen(report.Package, null, out _);
            Close();
        };
        var reportButton = new Button { Content = Loc.Get("CrashReportButton") };
        reportButton.Click += (_, _) =>
        {
            App.Instance?.Show(new LaunchRequest("diagnostics", report.Package));
            Close();
        };
        var close = new Button { Content = Loc.Get("CrashClose") };
        close.Click += (_, _) => Close();

        var root = new Grid
        {
            Padding = new Thickness(24),
            RowSpacing = 12,
            FlowDirection = App.FlowDirection,
            RequestedTheme = App.StartupSettings.Theme switch
            {
                Core.Settings.AppTheme.Light => ElementTheme.Light,
                Core.Settings.AppTheme.Dark => ElementTheme.Dark,
                _ => ElementTheme.Default,
            },
        };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        var header = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        header.Children.Add(new FontIcon { Glyph = "", FontSize = 28, Foreground = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"] });
        header.Children.Add(new TextBlock
        {
            Text = report.Title,
            Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"],
            TextWrapping = TextWrapping.Wrap,
            VerticalAlignment = VerticalAlignment.Center,
        });
        root.Children.Add(header);

        var body = new StackPanel { Spacing = 8 };
        body.Children.Add(new TextBlock { Text = report.Explanation, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
        body.Children.Add(new TextBlock
        {
            Text = Loc.Get("CrashHint"),
            TextWrapping = TextWrapping.Wrap,
            Opacity = 0.75,
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
        });
        Grid.SetRow(body, 1);
        root.Children.Add(body);

        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, HorizontalAlignment = HorizontalAlignment.Right };
        buttons.Children.Add(reopen);
        buttons.Children.Add(reportButton);
        buttons.Children.Add(close);
        Grid.SetRow(buttons, 2);
        root.Children.Add(buttons);

        Content = root;
#if DEBUG
        DebugSnapshot.ScheduleIfRequested(root, "ANDRUNA_MANAGER_SNAPSHOT_CRASH");
#endif
    }
}
