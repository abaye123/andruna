using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
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
        Vm = new AppPageViewModel(e.Parameter as string ?? string.Empty, App.Service, Loc.Strings);
        Bindings.Update();
        base.OnNavigatedTo(e);
        await Vm.LoadAsync();
    }
}
