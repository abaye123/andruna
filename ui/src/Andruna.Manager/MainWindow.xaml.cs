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

    public MainWindow(StartPage start)
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

        switch (start.Page)
        {
            case "settings":
                Nav.SelectedItem = SettingsItem;
                break;
            case "notices":
                Nav.SelectedItem = SettingsItem;
                Navigate(typeof(NoticesPage), null);
                break;
            case "app" when start.Package is not null:
                Navigate(typeof(LibraryPage), null);
                Navigate(typeof(AppPage), start.Package);
                break;
            default:
                Nav.SelectedItem = LibraryItem;
                break;
        }

#if DEBUG
        DebugSnapshot.ScheduleIfRequested(Root);
#endif
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
