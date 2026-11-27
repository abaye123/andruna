using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace Andruna.Manager.Pages;

/// <summary>Small conversions for x:Bind function bindings.</summary>
public static class Ui
{
    public static Visibility Visible(bool value) => value ? Visibility.Visible : Visibility.Collapsed;

    public static Visibility Hide(bool value) => value ? Visibility.Collapsed : Visibility.Visible;

    public static Visibility Empty(string? value) => string.IsNullOrEmpty(value) ? Visibility.Visible : Visibility.Collapsed;

    public static Visibility NotEmpty(string? value) => string.IsNullOrEmpty(value) ? Visibility.Collapsed : Visibility.Visible;

    /// <summary>An image from a file sysd wrote (an app icon), or null when there is none.</summary>
    public static ImageSource? Image(string? path) =>
        !string.IsNullOrEmpty(path) && File.Exists(path) ? new BitmapImage(new Uri(path)) : null;
}
