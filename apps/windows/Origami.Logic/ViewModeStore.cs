namespace Origami.Logic;

/// <summary>The chosen view mode, kept in view.txt beside the settings file. A missing, damaged or unwritable file means the default.</summary>
public sealed class ViewModeStore
{
    readonly string _path;
    public ViewModeStore(string settingsPath) { _path = Path.Combine(Path.GetDirectoryName(Path.GetFullPath(settingsPath))!, "view.txt"); }

    public ViewMode Load()
    {
        try
        {
            var text = File.ReadAllText(_path).Trim();
            foreach (var m in Enum.GetValues<ViewMode>()) if (m.ToString() == text) return m;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException) { }
        return ViewMode.FitToWindow;
    }

    public void Save(ViewMode m)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(_path)!);
            File.WriteAllText(_path, m.ToString());
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException) { } // the choice then lasts for this run only
    }
}
