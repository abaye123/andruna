using System.Diagnostics;
using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace Andruna.Manager.Pages;

/// <summary>Diagnostics (M-3); the navigation parameter may name the app to show first.</summary>
public sealed partial class DiagnosticsPage : Page
{
    private bool _syncing;

    public DiagnosticsPage()
    {
        Vm = new DiagnosticsViewModel(App.Service, App.NewEventsClient, App.Paths, Loc.Strings, App.Language, App.Identity.Version);
        InitializeComponent();
        LogChoice.Items.Add(Loc.Get("DiagLogSysd"));
        LogChoice.Items.Add(Loc.Get("DiagLogHost"));
        LogChoice.SelectedIndex = 0;
    }

    public DiagnosticsViewModel Vm { get; }

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        await LoadAsync(e.Parameter as string);
    }

    private async Task LoadAsync(string? package)
    {
        await Vm.LoadAsync(string.IsNullOrEmpty(package) ? Vm.SelectedApp?.Package : package);
        _syncing = true;
        AppChoice.SelectedItem = Vm.SelectedApp;
        _syncing = false;
    }

    private void OnAppChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_syncing && AppChoice.SelectedItem is DiagnosticsApp app)
        {
            Vm.SelectedApp = app;
        }
    }

    private void OnLogChanged(object sender, SelectionChangedEventArgs e) => Vm.SelectedLog = LogChoice.SelectedIndex;

    private async void OnRefreshClick(object sender, RoutedEventArgs e) => await LoadAsync(null);

    private void OnOpenLogsClick(object sender, RoutedEventArgs e)
    {
        Directory.CreateDirectory(Vm.LogsDirectory);
        Process.Start(new ProcessStartInfo(Vm.LogsDirectory) { UseShellExecute = true });
    }

    private async void OnSaveReportClick(object sender, RoutedEventArgs e)
    {
        var picker = new FileSavePicker
        {
            SuggestedStartLocation = PickerLocationId.Desktop,
            SuggestedFileName = $"{Loc.Get("DiagReportFileName")} {DateTime.Now:yyyy-MM-dd HHmm}",
        };
        picker.FileTypeChoices.Add(Loc.Get("DiagReportFileType"), [".zip"]);
        InitializeWithWindow.Initialize(picker, NativeWindow.Handle(App.MainWindow!));
        var file = await picker.PickSaveFileAsync();
        if (file is null)
        {
            return;
        }

        try
        {
            await Vm.SaveReportAsync(file.Path);
            ReportBar.Severity = InfoBarSeverity.Success;
            ReportBar.Title = Loc.Get("DiagReportSavedTitle");
            ReportBar.Message = Loc.Format("DiagReportSavedMessage", file.Path);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            ReportBar.Severity = InfoBarSeverity.Error;
            ReportBar.Title = Loc.Get("DiagReportFailedTitle");
            ReportBar.Message = ex.Message;
        }

        ReportBar.IsOpen = true;
    }
}
