using System.Text.Json;

namespace Origami.Logic;

public enum PointerMode { Follow, Capture }

public sealed class AppSettings
{
    public string LibraryRoot { get; set; } = "";
    public PointerMode DefaultPointerMode { get; set; } = PointerMode.Follow;
    /// <summary>Per machine override, by machine key.</summary>
    public Dictionary<string, PointerMode> PointerModeByMachine { get; set; } = new();
    public bool ConfirmQuitWithRunningMachines { get; set; } = true;

    public static string DefaultLibraryRoot() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Origami", "Machines");

    public PointerMode ModeFor(string key) =>
        PointerModeByMachine.TryGetValue(key, out var m) ? m : DefaultPointerMode;
}

public sealed class SettingsStore
{
    readonly string _path;
    static readonly JsonSerializerOptions Opts = new() { WriteIndented = true };

    public SettingsStore(string path) { _path = path; }

    public static string DefaultPath() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Origami", "settings.json");

    public AppSettings Load()
    {
        try
        {
            return JsonSerializer.Deserialize<AppSettings>(File.ReadAllText(_path)) ?? new AppSettings();
        }
        catch (Exception e) when (e is IOException or JsonException or UnauthorizedAccessException)
        {
            // A missing or damaged file is a first run; the next Save replaces it.
            return new AppSettings();
        }
    }

    public void Save(AppSettings s)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(_path)!);
        var tmp = _path + ".tmp";
        File.WriteAllText(tmp, JsonSerializer.Serialize(s, Opts));
        File.Move(tmp, _path, overwrite: true);
    }
}
