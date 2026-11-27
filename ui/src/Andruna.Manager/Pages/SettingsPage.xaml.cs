using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Andruna.Manager.Pages;

public sealed partial class SettingsPage : Page
{
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

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        await Vm.LoadServiceInfoAsync();
    }

    private void OnRestartClick(object sender, RoutedEventArgs e) => App.Restart();

    private void OnNoticesClick(object sender, RoutedEventArgs e) => Frame.Navigate(typeof(NoticesPage));
}
