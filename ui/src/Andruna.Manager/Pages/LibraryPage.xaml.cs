using Andruna.Manager.Core.Platform;
using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace Andruna.Manager.Pages;

public sealed partial class LibraryPage : Page
{
    public LibraryPage()
    {
        Vm = new LibraryViewModel(App.Service, Loc.Strings, App.Language);
        InitializeComponent();
        NavigationCacheMode = NavigationCacheMode.Required;
    }

    public LibraryViewModel Vm { get; }

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        await Vm.RefreshCommand.ExecuteAsync();
    }

    private async void OnInstallClick(object sender, RoutedEventArgs e)
    {
        var picker = new FileOpenPicker
        {
            SuggestedStartLocation = PickerLocationId.Downloads,
            ViewMode = PickerViewMode.List,
        };
        foreach (var type in LibraryViewModel.PackageFileTypes)
        {
            picker.FileTypeFilter.Add(type);
        }

        InitializeWithWindow.Initialize(picker, NativeWindow.Handle(App.MainWindow!));
        var file = await picker.PickSingleFileAsync();
        if (file is not null)
        {
            App.MainWindow?.ShowInstall(file.Path);
        }
    }

    private void OnDragOver(object sender, DragEventArgs e)
    {
        if (e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = DataPackageOperation.Copy;
            e.DragUIOverride.Caption = Loc.Get("InstallApkButton");
        }
    }

    private async void OnDrop(object sender, DragEventArgs e)
    {
        if (!e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            return;
        }

        var items = await e.DataView.GetStorageItemsAsync();
        var file = items.OfType<StorageFile>().FirstOrDefault(f => LaunchRequest.IsPackageFile(f.Path));
        if (file is not null)
        {
            App.MainWindow?.ShowInstall(file.Path);
        }
    }

    private void OnPackageClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is PackageTile tile)
        {
            App.MainWindow?.Open(new LaunchRequest("app", tile.PackageName));
        }
    }
}
