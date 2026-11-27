using Andruna.Manager.Core.Platform;
using Andruna.Manager.Core.Settings;
using Andruna.Manager.Localization;
using Andruna.Manager.Pages;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using Windows.Graphics;
using Windows.UI;

namespace Andruna.Manager;

public sealed partial class MainWindow : Window
{
    private bool _syncingSelection;

    public MainWindow(LaunchRequest start)
    {
        InitializeComponent();

        Title = Loc.Get("AppTitle");
        Root.FlowDirection = App.FlowDirection;
        if (App.IsRightToLeft)
        {
            NativeWindow.SetRightToLeftLayout(this);
        }

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "Andruna.ico"));

        var scale = NativeWindow.GetScale(this);
        AppWindow.Resize(new SizeInt32((int)(1080 * scale), (int)(720 * scale)));
        if (AppWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.PreferredMinimumWidth = (int)(640 * scale);
            presenter.PreferredMinimumHeight = (int)(480 * scale);
        }

        ApplyTheme(App.StartupSettings.Theme);
        Root.ActualThemeChanged += (_, _) => UpdateCaptionButtonColors();

        Open(start);

#if DEBUG
        DebugSnapshot.ScheduleIfRequested(Root);
#endif
    }

    /// <summary>Shows the page a launch request names.</summary>
    public void Open(LaunchRequest request)
    {
        switch (request.Page)
        {
            case "settings":
                Nav.SelectedItem = SettingsItem;
                break;
            case "notices":
                Nav.SelectedItem = SettingsItem;
                Navigate(typeof(NoticesPage), null);
                break;
            case "diagnostics":
                Navigate(typeof(DiagnosticsPage), request.Package ?? string.Empty);
                break;
            case "app" when request.Package is not null:
                if (ContentFrame.Content is null)
                {
                    Navigate(typeof(LibraryPage), null);
                }

                Navigate(typeof(AppPage), request.Package);
                break;
            default:
                if (ContentFrame.Content is null || request.InstallFile is null)
                {
                    Nav.SelectedItem = LibraryItem;
                }

                break;
        }
    }

    /// <summary>Restores and activates the window (a second start of the manager, a crash dialog).</summary>
    public void BringToFront()
    {
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } p)
        {
            p.Restore();
        }

        Activate();
        NativeWindow.SetForeground(this);
    }

    /// <summary>Opens the install dialog for a package file (file association, drag and drop, picker).</summary>
    public async void ShowInstall(string path)
    {
        if (ContentFrame.Content is null)
        {
            Nav.SelectedItem = LibraryItem;
        }

        await InstallDialog.ShowAsync(Root.XamlRoot ?? Content.XamlRoot, path, Root.ActualTheme);
        if (ContentFrame.Content is LibraryPage library)
        {
            await library.Vm.RefreshCommand.ExecuteAsync();
        }
    }

    public void ApplyTheme(AppTheme theme)
    {
        Root.RequestedTheme = theme switch
        {
            AppTheme.Light => ElementTheme.Light,
            AppTheme.Dark => ElementTheme.Dark,
            _ => ElementTheme.Default,
        };
        UpdateCaptionButtonColors();
    }

    /// <summary>Opens the page of one installed app.</summary>
    public void OpenApp(string packageName) => Navigate(typeof(AppPage), packageName);

    private void Navigate(Type page, object? parameter)
    {
        if (ContentFrame.CurrentSourcePageType != page || parameter is not null)
        {
            ContentFrame.Navigate(page, parameter);
        }
    }

    private void OnNavSelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (_syncingSelection)
        {
            return;
        }

        switch ((args.SelectedItem as NavigationViewItem)?.Tag as string)
        {
            case "library":
                Navigate(typeof(LibraryPage), null);
                break;
            case "settings":
                Navigate(typeof(SettingsPage), null);
                break;
            case "diagnostics":
                Navigate(typeof(DiagnosticsPage), null);
                break;
        }
    }

    private void OnNavBackRequested(NavigationView sender, NavigationViewBackRequestedEventArgs args)
    {
        if (ContentFrame.CanGoBack)
        {
            ContentFrame.GoBack();
        }
    }

    private void OnFrameNavigated(object sender, NavigationEventArgs e)
    {
        // Keep the selected menu item in step with the page shown (also after Back).
        _syncingSelection = true;
        Nav.SelectedItem = e.SourcePageType == typeof(SettingsPage) || e.SourcePageType == typeof(NoticesPage) ? SettingsItem
            : e.SourcePageType == typeof(LibraryPage) ? LibraryItem
            : e.SourcePageType == typeof(DiagnosticsPage) ? DiagnosticsItem
            : null;
        _syncingSelection = false;
    }

    private void UpdateCaptionButtonColors()
    {
        var dark = Root.ActualTheme == ElementTheme.Dark;
        var bar = AppWindow.TitleBar;
        bar.ButtonBackgroundColor = Colors.Transparent;
        bar.ButtonInactiveBackgroundColor = Colors.Transparent;
        bar.ButtonForegroundColor = dark ? Colors.White : Colors.Black;
        bar.ButtonHoverBackgroundColor = dark ? Color.FromArgb(0x15, 0xFF, 0xFF, 0xFF) : Color.FromArgb(0x0F, 0, 0, 0);
        bar.ButtonHoverForegroundColor = dark ? Colors.White : Colors.Black;
        bar.ButtonPressedBackgroundColor = dark ? Color.FromArgb(0x0B, 0xFF, 0xFF, 0xFF) : Color.FromArgb(0x0A, 0, 0, 0);
    }
}
