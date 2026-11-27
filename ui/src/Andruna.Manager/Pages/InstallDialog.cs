using Andruna.Manager.Core.ViewModels;
using Andruna.Manager.Localization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace Andruna.Manager.Pages;

/// <summary>
/// The install confirmation (P-12): the app's icon, name, version, signer
/// and permissions as sysd read them from the file, then Install or Update,
/// progress, and the result with an Open button.
/// </summary>
internal static class InstallDialog
{
    private static bool _open;

    public static async Task ShowAsync(XamlRoot xamlRoot, string path, ElementTheme theme)
    {
        // One dialog at a time (WinUI allows only one ContentDialog per window).
        if (_open)
        {
            return;
        }

        _open = true;
        try
        {
            var vm = new InstallViewModel(path, App.Service, Loc.Strings, App.Language);
            var view = new View(vm);
            var dialog = new ContentDialog
            {
                XamlRoot = xamlRoot,
                Title = Loc.Get("InstallDialogTitle"),
                Content = new ScrollViewer { Content = view.Root, MaxHeight = 480 },
                CloseButtonText = Loc.Get("DialogCancel"),
                DefaultButton = ContentDialogButton.Primary,
                FlowDirection = App.FlowDirection,
                RequestedTheme = theme,
            };
            void Sync()
            {
                view.Update();
                dialog.PrimaryButtonText = vm.Stage switch
                {
                    InstallStage.Done => Loc.Get("InstallOpenApp"),
                    InstallStage.Confirm or InstallStage.Installing => vm.ActionText,
                    _ => string.Empty,
                };
                dialog.IsPrimaryButtonEnabled = vm.CanInstall || vm.Stage == InstallStage.Done;
                dialog.CloseButtonText = Loc.Get(vm.Stage is InstallStage.Done or InstallStage.Failed or InstallStage.Unavailable ? "DialogClose" : "DialogCancel");
            }

            vm.PropertyChanged += (_, _) => Sync();
            dialog.PrimaryButtonClick += async (sender, args) =>
            {
                if (vm.Stage == InstallStage.Done && vm.Outcome is { } o)
                {
                    App.Launcher.TryOpen(o.Package, null, out _);
                    return;
                }

                // Stay open while installing; the result is shown in the dialog.
                args.Cancel = true;
                var deferral = args.GetDeferral();
                try
                {
                    await vm.InstallAsync();
                }
                finally
                {
                    deferral.Complete();
                }
            };
            dialog.Closing += (_, args) =>
            {
                if (vm.Stage == InstallStage.Installing)
                {
                    args.Cancel = true;
                }
            };

            Sync();
            var shown = dialog.ShowAsync();
            await vm.InspectAsync();
            await shown;
        }
        finally
        {
            _open = false;
        }
    }

    private sealed class View
    {
        private readonly InstallViewModel _vm;
        private readonly Image _icon = new() { Width = 64, Height = 64 };
        private readonly FontIcon _placeholder = new() { Glyph = "", FontSize = 32 };
        private readonly TextBlock _label = new() { Style = Style("SubtitleTextBlockStyle"), TextWrapping = TextWrapping.Wrap };
        private readonly TextBlock _version = new() { Opacity = 0.8 };
        private readonly TextBlock _file = new() { Style = Style("CaptionTextBlockStyle"), Opacity = 0.6, TextTrimming = TextTrimming.CharacterEllipsis };
        private readonly ProgressRing _ring = new() { Width = 20, Height = 20, IsActive = true };
        private readonly TextBlock _status = new() { TextWrapping = TextWrapping.Wrap, VerticalAlignment = VerticalAlignment.Center };
        private readonly InfoBar _problem = new() { IsClosable = false, Severity = InfoBarSeverity.Error };
        private readonly TextBlock _signer = new() { Style = Style("CaptionTextBlockStyle"), TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
        private readonly TextBlock _permissionsHeader = new() { Text = Loc.Get("InstallPermissionsHeader"), Style = Style("BodyStrongTextBlockStyle"), Margin = new Thickness(0, 8, 0, 0) };
        private readonly StackPanel _permissions = new() { Spacing = 6 };
        private readonly TextBlock _trust = new() { Text = Loc.Get("InstallTrustNote"), Style = Style("CaptionTextBlockStyle"), TextWrapping = TextWrapping.Wrap, Opacity = 0.8 };
        private readonly StackPanel _busy = new() { Orientation = Orientation.Horizontal, Spacing = 12 };
        private readonly TextBlock _done = new() { TextWrapping = TextWrapping.Wrap };

        public View(InstallViewModel vm)
        {
            _vm = vm;
            var iconBox = new Grid { Width = 72, Height = 72, CornerRadius = new CornerRadius(16), Background = Brush("SubtleFillColorSecondaryBrush") };
            iconBox.Children.Add(_placeholder);
            iconBox.Children.Add(_icon);
            var titles = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
            titles.Children.Add(_label);
            titles.Children.Add(_version);
            titles.Children.Add(_file);
            var header = new Grid { ColumnSpacing = 16 };
            header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            header.ColumnDefinitions.Add(new ColumnDefinition());
            header.Children.Add(iconBox);
            Grid.SetColumn(titles, 1);
            header.Children.Add(titles);

            _busy.Children.Add(_ring);
            _busy.Children.Add(_status);

            Root.Children.Add(header);
            Root.Children.Add(_busy);
            Root.Children.Add(_done);
            Root.Children.Add(_problem);
            Root.Children.Add(_signer);
            Root.Children.Add(_permissionsHeader);
            Root.Children.Add(_permissions);
            Root.Children.Add(_trust);
            Update();
        }

        public StackPanel Root { get; } = new() { Spacing = 12, MinWidth = 420 };

        public void Update()
        {
            _label.Text = _vm.Label;
            _version.Text = _vm.VersionText;
            _version.Visibility = Show(_vm.VersionText.Length > 0);
            _file.Text = _vm.FileName;
            var hasIcon = _vm.IconPath.Length > 0 && File.Exists(_vm.IconPath);
            _icon.Source = hasIcon ? new BitmapImage(new Uri(_vm.IconPath)) : null;
            _placeholder.Visibility = Show(!hasIcon);
            _busy.Visibility = Show(_vm.IsBusy);
            _status.Text = _vm.StatusText;
            _done.Text = _vm.Stage == InstallStage.Done ? _vm.StatusText : string.Empty;
            _done.Visibility = Show(_vm.Stage == InstallStage.Done);
            _problem.IsOpen = _vm.HasProblem;
            _problem.Severity = _vm.Stage == InstallStage.Unavailable ? InfoBarSeverity.Informational : InfoBarSeverity.Error;
            _problem.Title = _vm.Problem?.Title ?? string.Empty;
            _problem.Message = _vm.Problem?.Message ?? string.Empty;
            var details = _vm.HasDetails && _vm.Stage is InstallStage.Confirm or InstallStage.Installing;
            _signer.Text = _vm.SignerText;
            _signer.Visibility = Show(details && _vm.SignerText.Length > 0);
            _permissionsHeader.Visibility = Show(details);
            _trust.Visibility = Show(details);
            _permissions.Visibility = Show(details);
            _permissions.Children.Clear();
            if (details)
            {
                if (_vm.PermissionList.Count == 0)
                {
                    _permissions.Children.Add(new TextBlock { Text = Loc.Get("InstallNoPermissions"), Opacity = 0.8 });
                }

                foreach (var p in _vm.PermissionList)
                {
                    _permissions.Children.Add(PermissionRow(p));
                }
            }
        }

        private static StackPanel PermissionRow(PermissionItem p)
        {
            var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10 };
            row.Children.Add(new FontIcon
            {
                Glyph = p.IsSensitive ? "" : "",
                FontSize = 14,
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, 3, 0, 0),
            });
            var text = new StackPanel();
            text.Children.Add(new TextBlock { Text = p.Description, TextWrapping = TextWrapping.Wrap });
            text.Children.Add(new TextBlock
            {
                Text = p.Name,
                Style = Style("CaptionTextBlockStyle"),
                Opacity = 0.6,
                FlowDirection = FlowDirection.LeftToRight,
                HorizontalAlignment = App.IsRightToLeft ? HorizontalAlignment.Right : HorizontalAlignment.Left,
            });
            row.Children.Add(text);
            return row;
        }

        private static Visibility Show(bool visible) => visible ? Visibility.Visible : Visibility.Collapsed;

        private static Style Style(string key) => (Style)Application.Current.Resources[key];

        private static Brush Brush(string key) => (Brush)Application.Current.Resources[key];
    }
}
