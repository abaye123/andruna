using System.Diagnostics;
using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Andruna.Manager.Pages;

public sealed partial class SettingsPage : Page
{
    private static readonly string NoticesPath = Path.Combine(AppContext.BaseDirectory, "THIRD_PARTY_NOTICES.txt");

    public SettingsPage()
    {
        Vm = new SettingsViewModel(
            App.SettingsStore,
            App.SettingsStore.Load(),
            App.StartupSettings.Language,
            App.Identity,
            App.Service,
            Loc.Strings);
        Vm.ThemeChanged += (_, theme) => App.MainWindow?.ApplyTheme(theme);
        InitializeComponent();
    }

    public SettingsViewModel Vm { get; }

    public bool NoticesAvailable => File.Exists(NoticesPath);

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        await Vm.LoadServiceInfoAsync();
    }

    private void OnRestartClick(object sender, RoutedEventArgs e) => App.Restart();

    private void OnNoticesClick(object sender, RoutedEventArgs e)
    {
        Process.Start(new ProcessStartInfo(NoticesPath) { UseShellExecute = true });
    }
}
