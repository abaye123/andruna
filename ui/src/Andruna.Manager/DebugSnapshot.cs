#if DEBUG
using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace Andruna.Manager;

/// <summary>
/// Debug builds only: when ANDRUNA_MANAGER_SNAPSHOT (the main window) or
/// ANDRUNA_MANAGER_SNAPSHOT_CRASH (a crash dialog) names a .png path, the
/// window content, with an open dialog drawn over it, is rendered into that
/// file a few seconds after it opens and the app exits. Used to take
/// screenshots of the pages on a machine whose screen cannot be captured
/// (for example a locked session). Not compiled into Release builds.
/// </summary>
internal static class DebugSnapshot
{
    public static async void ScheduleIfRequested(FrameworkElement root, string variable = "ANDRUNA_MANAGER_SNAPSHOT")
    {
        var path = Environment.GetEnvironmentVariable(variable);
        if (string.IsNullOrEmpty(path))
        {
            return;
        }

        await Task.Delay(TimeSpan.FromSeconds(5));

        // Mica is composed by the system and is not part of the rendered content.
        if (root is Microsoft.UI.Xaml.Controls.Panel panel)
        {
            panel.Background = new SolidColorBrush(root.ActualTheme == ElementTheme.Dark
                ? Windows.UI.Color.FromArgb(0xFF, 0x20, 0x20, 0x20)
                : Windows.UI.Color.FromArgb(0xFF, 0xF3, 0xF3, 0xF3));
            await Task.Delay(300);
        }

        var (width, height, pixels) = await RenderAsync(root);

        // An open dialog lives in a popup outside the root; draw it over the page.
        foreach (var popup in VisualTreeHelper.GetOpenPopupsForXamlRoot(root.XamlRoot))
        {
            if (popup.Child is not FrameworkElement child)
            {
                continue;
            }

            var (w, h, over) = await RenderAsync(child);
            for (var y = 0; y < Math.Min(h, height); y++)
            {
                for (var x = 0; x < Math.Min(w, width); x++)
                {
                    var s = ((y * w) + x) * 4;
                    var d = ((y * width) + x) * 4;
                    var a = over[s + 3];
                    for (var c = 0; c < 4; c++)
                    {
                        pixels[d + c] = (byte)Math.Min(255, over[s + c] + (pixels[d + c] * (255 - a) / 255));
                    }
                }
            }
        }

        using (var stream = new InMemoryRandomAccessStream())
        {
            var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
            encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied, (uint)width, (uint)height, 96, 96, pixels);
            await encoder.FlushAsync();
            stream.Seek(0);
            var bytes = new byte[stream.Size];
            await stream.ReadAsync(bytes.AsBuffer(), (uint)stream.Size, InputStreamOptions.None);
            await File.WriteAllBytesAsync(path, bytes);
        }

        Application.Current.Exit();
    }

    private static async Task<(int Width, int Height, byte[] Pixels)> RenderAsync(UIElement element)
    {
        var bitmap = new RenderTargetBitmap();
        await bitmap.RenderAsync(element);
        var pixels = (await bitmap.GetPixelsAsync()).ToArray();
        return (bitmap.PixelWidth, bitmap.PixelHeight, pixels);
    }
}
#endif
