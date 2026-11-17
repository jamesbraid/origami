using Microsoft.UI.Xaml;
using Origami.Logic;

namespace Origami.App;

public partial class App : Application
{
    Window? _window;
    FileStream? _stderr;
    public static string[] Args = Array.Empty<string>();

    /// <summary>A WinExe has no console; startup failures go to app.log beside the settings file.</summary>
    public static void Log(string line)
    {
        try
        {
            var settings = ArgValue("--settings");
            var dir = settings is null ? Path.GetTempPath() : Path.GetDirectoryName(Path.GetFullPath(settings))!;
            File.AppendAllText(Path.Combine(dir, "app.log"), DateTime.Now.ToString("HH:mm:ss.fff ") + line + Environment.NewLine);
        }
        catch { }
    }

    public App()
    {
        AppDomain.CurrentDomain.UnhandledException += (_, e) => Log("domain unhandled: " + e.ExceptionObject);
        if (Array.IndexOf(Environment.GetCommandLineArgs(), "--log-first-chance") >= 0)
            AppDomain.CurrentDomain.FirstChanceException += (_, e) => Log("first chance: " + e.Exception.GetType().Name + ": " + e.Exception.Message);
        InitializeComponent();
        UnhandledException += (_, e) =>
        {
            Log("xaml unhandled: " + e.Exception);
            Selftest.Fail("unhandled exception: " + e.Exception.Message);
        };
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        try { Launch(); }
        catch (Exception e) { Log("launch failed: " + e); Environment.Exit(7); }
    }

    [System.Runtime.InteropServices.DllImport("kernel32")] static extern bool SetStdHandle(int which, IntPtr handle);

    void Launch()
    {
        Log("launching");
        // A WinExe has no stderr; the core and its runner print their notices there. --stderr FILE keeps them.
        if (ArgValue("--stderr") is { } err)
        {
            var fs = new FileStream(err, FileMode.Create, FileAccess.Write, FileShare.ReadWrite);
            SetStdHandle(-12, fs.SafeFileHandle.DangerousGetHandle());
            GC.KeepAlive(fs); _stderr = fs;
        }
        var store = new SettingsStore(ArgValue("--settings") ?? SettingsStore.DefaultPath());
        var settings = store.Load();
        var root = ArgValue("--library")
            ?? (settings.LibraryRoot is { Length: > 0 } r ? r
                : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Origami", "Machines"));
        var core = new UniffiCoreClient(root);
#if ORIGAMI_FAKE
        // A build with only the fake engine has no machines of its own.
        if (Array.IndexOf(Environment.GetCommandLineArgs(), "--seed-fake") >= 0)
            core.SeedFake(Path.Combine(Path.GetTempPath(), "origami-seed"));
#endif
        _window = new MainWindow(core, store);
        Log("window constructed");
        _window.Activate();
        ((MainWindow)_window).SizeToDips(1500, 1000);
        if (ArgValue("--selftest") is { } outFile)
            _ = Selftest.RunAsync((MainWindow)_window, core, outFile);
    }

    public static string? ArgValue(string name)
    {
        var a = Environment.GetCommandLineArgs();
        var i = Array.IndexOf(a, name);
        return i >= 0 && i + 1 < a.Length ? a[i + 1] : null;
    }
}
