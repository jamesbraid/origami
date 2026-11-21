using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

public sealed partial class SettingsPane : UserControl
{
    AppSettings _s = new();
    /// <summary>Where the downloaded disks are listed from; set once by the window.</summary>
    public ICoreClient? Core { get; set; }
    public event Action<AppSettings>? Saved;
    public event Action? Closed;

    public SettingsPane()
    {
        InitializeComponent();
        SaveButton.Click += (_, _) =>
        {
            _s.LibraryRoot = RootBox.Text.Trim();
            _s.DefaultPointerMode = ModeButtons.SelectedIndex == 1 ? PointerMode.Capture : PointerMode.Follow;
            _s.ConfirmQuitWithRunningMachines = ConfirmSwitch.IsOn;
            Saved?.Invoke(_s);
        };
        CloseButton.Click += (_, _) => Closed?.Invoke();
    }

    public void Load(AppSettings s)
    {
        _s = s;
        RootBox.Text = s.LibraryRoot;
        ModeButtons.SelectedIndex = s.DefaultPointerMode == PointerMode.Capture ? 1 : 0;
        ConfirmSwitch.IsOn = s.ConfirmQuitWithRunningMachines;
        ListDownloaded();
    }

    /// <summary>One row per downloaded disk with Remove, or a line saying there are none.</summary>
    void ListDownloaded()
    {
        DownloadedList.Children.Clear();
        var disks = Core?.CachedDisks() ?? Array.Empty<CachedDisk>();
        if (disks.Count == 0) DownloadedList.Children.Add(new TextBlock { Text = "No downloaded disks", Opacity = 0.7 });
        foreach (var d in disks)
        {
            var remove = new Button { Content = "Remove", VerticalAlignment = Microsoft.UI.Xaml.VerticalAlignment.Center };
            remove.Click += (_, _) => { Core?.RemoveCachedDisk(d.Key); ListDownloaded(); };
            var text = new StackPanel { Children = { new TextBlock { Text = d.Label }, new TextBlock { Text = Core!.SizeLabel(d.Bytes), Opacity = 0.7 } } };
            var row = new Grid { ColumnSpacing = 12, ColumnDefinitions = { new ColumnDefinition(), new ColumnDefinition { Width = Microsoft.UI.Xaml.GridLength.Auto } } };
            row.Children.Add(text);
            Grid.SetColumn(remove, 1);
            row.Children.Add(remove);
            DownloadedList.Children.Add(row);
        }
    }
}
