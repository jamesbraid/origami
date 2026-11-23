using Microsoft.UI.Xaml;

namespace Origami.App;

/// <summary>The Origami icon for a window's title bar and taskbar entry; without it a secondary window shows the generic one.</summary>
internal static class AppIcon
{
    public static string Path => System.IO.Path.Combine(AppContext.BaseDirectory, "Origami.ico");

    public static bool Apply(Window w)
    {
        if (!File.Exists(Path)) return false;
        w.AppWindow.SetIcon(Path);
        return true;
    }
}
