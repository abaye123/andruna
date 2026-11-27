using Andruna.Manager.Core.Platform;
using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Andruna.Manager.Pages;

/// <summary>The page of one installed app; the navigation parameter is its package name.</summary>
public sealed partial class AppPage : Page
{
    public AppPage()
    {
        InitializeComponent();
    }

    public AppPageViewModel Vm { get; private set; } = null!;

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        Vm = new AppPageViewModel(e.Parameter as string ?? string.Empty, App.Service, Loc.Strings, App.AppPageServices);
        Bindings.Update();
        base.OnNavigatedTo(e);
        await Vm.LoadAsync();
    }

    private void OnOpenClick(object sender, RoutedEventArgs e) => Vm.Open();

    private void OnPinClick(object sender, RoutedEventArgs e) => Vm.PinToStart();

    private void OnDiagnosticsClick(object sender, RoutedEventArgs e) =>
        App.MainWindow?.Open(new LaunchRequest("diagnostics", Vm.PackageName));

    private async void OnUninstallClick(object sender, RoutedEventArgs e)
    {
        var keep = new CheckBox { Content = Loc.Get("AppUninstallKeepData"), IsChecked = false };
        var body = new StackPanel { Spacing = 12 };
        body.Children.Add(new TextBlock { Text = Loc.Get("AppUninstallConfirmMessage"), TextWrapping = TextWrapping.Wrap });
        body.Children.Add(keep);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = Loc.Format("AppUninstallConfirmTitle", Vm.Title),
            Content = body,
            PrimaryButtonText = Loc.Get("AppUninstallButton"),
            CloseButtonText = Loc.Get("DialogCancel"),
            DefaultButton = ContentDialogButton.Close,
            FlowDirection = App.FlowDirection,
            RequestedTheme = ActualTheme,
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            await Vm.UninstallAsync(keep.IsChecked == true);
        }
    }
}
