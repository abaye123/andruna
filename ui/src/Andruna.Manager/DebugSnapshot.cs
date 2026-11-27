#if DEBUG
using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace Andruna.Manager;

/// <summary>
/// Debug builds only: when ANDRUNA_MANAGER_SNAPSHOT names a .png path, the
/// window content is rendered into that file a few seconds after start and the
/// app exits. Used to take screenshots of the pages on a machine whose screen
/// cannot be captured (for example a locked session). Not compiled into
/// Release builds.
/// </summary>
internal static class DebugSnapshot
{
    public static async void ScheduleIfRequested(FrameworkElement root)
    {
        var path = Environment.GetEnvironmentVariable("ANDRUNA_MANAGER_SNAPSHOT");
        if (string.IsNullOrEmpty(path))
        {
            return;
        }

        await Task.Delay(TimeSpan.FromSeconds(4));

        // Mica is composed by the system and is not part of the rendered content.
        if (root is Microsoft.UI.Xaml.Controls.Panel panel)
        {
            panel.Background = new SolidColorBrush(root.ActualTheme == ElementTheme.Dark
                ? Windows.UI.Color.FromArgb(0xFF, 0x20, 0x20, 0x20)
                : Windows.UI.Color.FromArgb(0xFF, 0xF3, 0xF3, 0xF3));
            await Task.Delay(300);
        }

        var bitmap = new RenderTargetBitmap();
        await bitmap.RenderAsync(root);
        var pixels = await bitmap.GetPixelsAsync();
        using (var stream = new InMemoryRandomAccessStream())
        {
            var encoder = await BitmapEncoder.CreateAsync(BitmapEncoder.PngEncoderId, stream);
            encoder.SetPixelData(
                BitmapPixelFormat.Bgra8,
                BitmapAlphaMode.Premultiplied,
                (uint)bitmap.PixelWidth,
                (uint)bitmap.PixelHeight,
                96,
                96,
                pixels.ToArray());
            await encoder.FlushAsync();
            stream.Seek(0);
            var bytes = new byte[stream.Size];
            await stream.ReadAsync(bytes.AsBuffer(), (uint)stream.Size, InputStreamOptions.None);
            await File.WriteAllBytesAsync(path, bytes);
        }

        Application.Current.Exit();
    }
}
#endif
