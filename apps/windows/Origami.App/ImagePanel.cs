using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>The one place a disk image, a package or a folder is chosen. The self-test queues answers because a native picker cannot be driven headless.</summary>
public static class PathPicker
{
    public static IntPtr Hwnd;
    public static readonly Queue<string> Answers = new();

    public static async Task<string?> PickFileAsync()
    {
        if (Answers.Count > 0) return Answers.Dequeue();
        var picker = new Windows.Storage.Pickers.FileOpenPicker();
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, Hwnd);
        return (await picker.PickSingleFileAsync())?.Path;
    }

    /// <summary>A folder, from the same queue of self-test answers.</summary>
    public static async Task<string?> PickFolderAsync()
    {
        if (Answers.Count > 0) return Answers.Dequeue();
        var picker = new Windows.Storage.Pickers.FolderPicker();
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, Hwnd);
        return (await picker.PickSingleFolderAsync())?.Path;
    }
}

/// <summary>The questions about one chosen image, as New Machine and Add Disk Image both ask them. A ContentDialog cannot open another, so Settings hosts this inline.</summary>
public sealed class ImagePanel
{
    public StackPanel Root { get; }
    public ImageChoice Choice { get; } = new();
    public event Action? Changed;
    readonly ICoreClient _core;
    readonly string? _machine;
    readonly Button _choose = new() { Content = "Choose an image…" };
    readonly TextBlock _file = new() { TextWrapping = TextWrapping.Wrap };
    readonly TextBlock _badge = new() { Opacity = 0.8 };
    readonly ComboBox _place = new() { Header = "Where should the disk live?", SelectedIndex = 0, HorizontalAlignment = HorizontalAlignment.Stretch };
    readonly ComboBox _keep = new() { Header = "How should changes be kept?", SelectedIndex = 0, HorizontalAlignment = HorizontalAlignment.Stretch };
    readonly TextBlock _note = new() { TextWrapping = TextWrapping.Wrap };

    /// <param name="machineKey">The machine the image is for, once it exists.</param>
    public ImagePanel(ICoreClient core, string? machineKey)
    {
        _core = core;
        _machine = machineKey;
        AutomationProperties.SetAutomationId(_choose, "disk.choose");
        AutomationProperties.SetAutomationId(_file, "disk.file");
        AutomationProperties.SetAutomationId(_badge, "disk.badge");
        AutomationProperties.SetAutomationId(_place, "disk.place");
        AutomationProperties.SetAutomationId(_keep, "disk.keep");
        AutomationProperties.SetAutomationId(_note, "disk.note");
        var labels = core.Labels();
        _place.ItemsSource = labels.Place.ToList();
        _place.SelectedIndex = 0;
        _keep.ItemsSource = labels.Keep.ToList();
        _keep.SelectedIndex = 0;
        var help = new TextBlock { Text = labels.SameKind, TextWrapping = TextWrapping.Wrap, Opacity = 0.7 };
        Root = new StackPanel { Spacing = 8, Children = { _choose, _file, _badge, _place, _keep, _note, help } };
        _choose.Click += async (_, _) => await ChooseAsync();
        _place.SelectionChanged += (_, _) => { Choice.Copy = _place.SelectedIndex == 0; Sync(); };
        _keep.SelectionChanged += (_, _) => { Choice.Apart = _keep.SelectedIndex == 0; Sync(); };
        Sync();
    }

    public async Task ChooseAsync()
    {
        if (await PathPicker.PickFileAsync() is { } path) await SetAsync(path);
    }

    public async Task SetAsync(string path)
    {
        Choice.Path = path; Choice.Check = null; Choice.Error = null; Choice.Badge = "Reading…";
        Sync();
        try
        {
            var check = await _core.CheckDiskImageAsync(path, _machine);
            string os;
            try { os = await _core.DiskInfoAsync(path); } catch { os = "Unknown"; }
            if (Choice.Path != path) return; // another file was chosen meanwhile
            Choice.Check = check; Choice.Badge = os;
        }
        catch (Exception e) when (Choice.Path == path) { Choice.Error = e.Message; }
        Sync();
    }

    void Sync()
    {
        Choice.Verdict = Choice.Check is { } checkd ? _core.Verdict(checkd, Choice.Copy) : null;
        var shown = Choice.Path is not null;
        var read = Choice.Check is not null;
        _file.Visibility = shown ? Visibility.Visible : Visibility.Collapsed;
        _file.Text = shown ? System.IO.Path.GetFileName(Choice.Path!) + (read ? "  " + _core.SizeLabel(Choice.Check!.Bytes) : "") : "";
        _badge.Visibility = shown && Choice.Error is null ? Visibility.Visible : Visibility.Collapsed;
        _badge.Text = Choice.Badge;
        _place.Visibility = read ? Visibility.Visible : Visibility.Collapsed;
        _keep.Visibility = read && Choice.Check!.IsChd ? Visibility.Visible : Visibility.Collapsed;
        var note = Choice.Error ?? Choice.Verdict?.Note;
        var bad = Choice.Error is not null || (Choice.Verdict?.Warning ?? false);
        if (note is not null)
        {
            _note.Text = note;
            _note.Opacity = bad ? 1 : 0.7;
            _note.Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(bad ? Microsoft.UI.Colors.IndianRed : Microsoft.UI.Colors.Gray);
            _note.Visibility = Visibility.Visible;
        }
        else _note.Visibility = Visibility.Collapsed;
        Changed?.Invoke();
    }
}
