using System.Diagnostics;
using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Navigation;

namespace Andruna.Manager.Pages;

/// <summary>Third-party notices, opened from Settings > About.</summary>
public sealed partial class NoticesPage : Page
{
    private CancellationTokenSource? _loading;

    public NoticesPage()
    {
        Vm = new NoticesViewModel(Path.Combine(AppContext.BaseDirectory, NoticesViewModel.FileName), Loc.Strings);
        InitializeComponent();
        Breadcrumb.ItemsSource = new[] { Loc.Get("SettingsTitle"), Loc.Get("NoticesTitle") };
    }

    public NoticesViewModel Vm { get; }

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        _loading = new CancellationTokenSource();
        try
        {
            await Vm.LoadAsync(_loading.Token);
            if (Vm.IsLoaded)
            {
                // Put the keyboard on the text, so arrows and Page Down scroll it at
                // once, with the caret at the start so the view stays at the top.
                // Queued, because the frame sets its own focus once the page is loaded.
                DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
                {
                    NoticesText.Select(0, 0);
                    NoticesText.Focus(FocusState.Programmatic);
                });
            }
        }
        catch (OperationCanceledException)
        {
            // The user left the page while the file was being read.
        }
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        _loading?.Cancel();
        _loading?.Dispose();
        _loading = null;
        base.OnNavigatedFrom(e);
    }

    private void GoBack()
    {
        if (Frame.CanGoBack)
        {
            Frame.GoBack();
        }
        else
        {
            Frame.Navigate(typeof(SettingsPage));
        }
    }

    private void OnBreadcrumbItemClicked(BreadcrumbBar sender, BreadcrumbBarItemClickedEventArgs args)
    {
        if (args.Index == 0)
        {
            GoBack();
        }
    }

    private void OnBackAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        args.Handled = true;
        GoBack();
    }

    private void OnOpenExternallyClick(object sender, RoutedEventArgs e)
    {
        Process.Start(new ProcessStartInfo(Vm.FilePath) { UseShellExecute = true });
    }
}
