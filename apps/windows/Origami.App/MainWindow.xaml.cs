using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Origami.Logic;
using Windows.UI;

namespace Origami.App;

public sealed partial class MainWindow : Window
{
    readonly ICoreClient _core;
    readonly SettingsStore _store;
    AppSettings _settings;
    readonly DispatcherQueue _ui;
    readonly Dictionary<string, ConsoleWindow> _consoles = new();
    bool _prefsOpen, _closeApproved;
    string? _attachedKey, _modeKey;
    int _refreshQueued;

    readonly ViewModeStore _viewStore;

    public MainWindow(ICoreClient core, SettingsStore store)
    {
        _core = core; _store = store; _settings = store.Load();
        _viewStore = new ViewModeStore(App.ArgValue("--settings") ?? SettingsStore.DefaultPath());
        _ui = DispatcherQueue.GetForCurrentThread();
        InitializeComponent();
        Title = "Origami";
        Display.ViewMode = _viewStore.Load();
        Display.WindowHandle = () => WinRT.Interop.WindowNative.GetWindowHandle(this);
        Display.PointerModeChanged = RefreshPointerUi;
        Display.Shortcut = OnDisplayShortcut;

        BuildMenus();
        BuildLibrary();

        Library.SelectionChanged += (_, _) => { if (_rebuilding) return; _prefsOpen = false; if (SelectedKey is { } k) _tabs.Select(k); Refresh(); };
        StartButton.Click += (_, _) => Invoke("machine.start");
        HeroStart.Click += (_, _) => Invoke("machine.start");
        PowerOffButton.Click += (_, _) => Invoke("machine.stop");
        PauseButton.Click += (_, _) => Invoke("machine.pause");
        ConsoleButton.Click += (_, _) => Invoke("window.console");
        SettingsButton.Click += (_, _) => Invoke("machine.settings");
        PointerButton.Click += (_, _) => Invoke("machine.pointer-mode");
        ShowShot.Click += (_, _) => ShowInExplorer();

        Settings.Saved += s => { _settings = s; _store.Save(s); _modeKey = null; Refresh(); };
        Settings.Closed += () => { _prefsOpen = false; Refresh(); };

        // Core callbacks arrive on the core's thread; hop to the UI thread.
        // One refresh per burst; console output and pictures of an attached screen change nothing it shows.
        _core.Events += e =>
        {
            if (e.Kind == MachineEventKind.ConsoleOutput || (e.Kind == MachineEventKind.DisplayChanged && Display.Attached)) return;
            if (Interlocked.Exchange(ref _refreshQueued, 1) == 0 && !_ui.TryEnqueue(() =>
            {
                _refreshQueued = 0;
                try { Refresh(); } catch (Exception ex) { App.Log("refresh failed: " + ex); }
            })) _refreshQueued = 0;
        };

        Root.ActualThemeChanged += (_, _) =>
        {
            foreach (var (img, id) in _rowStills) img.Source = RowArt(id);
            Refresh();
        };
        Activated += (_, a) => { if (a.WindowActivationState == WindowActivationState.Deactivated) Display.LoseFocus(); };
        AppWindow.Closing += OnClosing;
        Library.SelectedIndex = 0;
        try { Refresh(); } catch (Exception e) { App.Log("first refresh failed: " + e.Message); }
        // A machine can end with no event the app sees (its runner or QEMU dies): look again every two seconds.
        _recheck = _ui.CreateTimer();
        _recheck.Interval = TimeSpan.FromSeconds(2);
        _recheck.Tick += (_, _) => { try { Refresh(); } catch (Exception ex) { App.Log("recheck failed: " + ex.Message); } };
        _recheck.Start();
    }

    DispatcherQueueTimer? _recheck;

    // ---- accessors for the automation (self-test) hooks ------------------------------------

    public DisplayView DisplayViewForTest => Display;
    public bool DisplayVisibleForTest => Display.Visibility == Visibility.Visible;

    /// <summary>A key pressed over the screen that matches a command's shortcut; true when a command took it.</summary>
    bool OnDisplayShortcut(int vk, bool ctrl, bool shift, bool alt)
    {
        foreach (var c in _commands)
        {
            if (!Accelerator.TryParse(c.WindowsAccelerator, out var a) || !a.TakenOverGuest) continue;
            if (a.VirtualKey != vk || a.Ctrl != ctrl || a.Shift != shift || a.Alt != alt) continue;
            Invoke(c.Id);
            return true;
        }
        return false;
    }

    public string? SelectedKey => (Library.SelectedItem as ListViewItem)?.Tag as string;
    public string PillTextForTest => PillText.Text;
    public int LibraryCount => Library.Items.Count;
    public string StatusForTest => StatusText.Text;
    public void FocusLibrary() => Library.Focus(FocusState.Programmatic);
    public bool LibraryShown => LibraryColumn.Width.Value > 0;
    public IReadOnlyList<CommandInfo> Commands => _commands;
    /// <summary>Command ID to the menu item's shortcut text, for the menus check.</summary>
    public IReadOnlyDictionary<string, string?> MenuShortcuts => _menuShortcuts;
    public IntPtr Hwnd => WinRT.Interop.WindowNative.GetWindowHandle(this);
    bool Dark => Root.ActualTheme == ElementTheme.Dark;

    // ---- library ----------------------------------------------------------------------------

    // ---- stills and measured colours --------------------------------------------------------

    static readonly string IdentityDir = Path.Combine(AppContext.BaseDirectory, "identity");
    readonly StillCatalog _stills = LoadStills();
    readonly List<(Image Img, string Id)> _rowStills = new();
    public int LibraryStills { get; private set; }
    public string ChassisHex(string key) { var c = ChassisOf(_core.Machines().First(x => x.Key == key)); return $"#{c.R:x2}{c.G:x2}{c.B:x2}"; }
    /// <summary>The screen position of a library row's centre, for a real click in a self-test.</summary>
    public (int X, int Y)? RowCenter(string key)
    {
        foreach (ListViewItem it in Library.Items)
            if ((string?)it.Tag == key)
            {
                var s = Root.XamlRoot.RasterizationScale;
                var p = it.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(it.ActualWidth / 2, it.ActualHeight / 2));
                var pt = new Win32.POINT { X = (int)(p.X * s), Y = (int)(p.Y * s) };
                Win32.ClientToScreen(Hwnd, ref pt);
                return (pt.X, pt.Y);
            }
        return null;
    }
    public int ConsolesOpen => _consoles.Count;
    public void CloseConsoles() { foreach (var c in _consoles.Values.ToList()) c.Close(); }
    public string PauseLabelForTest => PauseButton.Label;
    public bool HeroShowsStill => HeroImage.Source != null;

    static StillCatalog LoadStills()
    {
        try { return StillCatalog.Parse(File.ReadAllText(Path.Combine(IdentityDir, "stills", "manifest.json"))); }
        catch (Exception e) when (e is IOException or System.Text.Json.JsonException) { App.Log("no stills manifest: " + e.Message); return StillCatalog.Empty; }
    }

    Still? StillOf(MachineSummary m) => _stills.Resolve(m.Product, m.Topology);

    static Microsoft.UI.Xaml.Media.Imaging.BitmapImage? Art(string id, string file)
    {
        var path = Path.Combine(IdentityDir, "stills", id, file);
        return File.Exists(path) ? new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(path)) : null;
    }

    Microsoft.UI.Xaml.Media.Imaging.BitmapImage? RowArt(string id) => Art(id, $"34-{(Dark ? "dark" : "light")}@2x.png");

    /// <summary>The measured chassis colour from the stills manifest; a hash-derived one only for a machine with no still.</summary>
    Color ChassisOf(MachineSummary m) =>
        StillOf(m) is { } s ? Color.FromArgb(255, (byte)(s.Rgb >> 16), (byte)(s.Rgb >> 8), (byte)s.Rgb) : ChassisColor(m.Product);

    static readonly (string Match, Color Color)[] Chassis =
    {
        ("octane2", Color.FromArgb(255, 0x3B, 0x6F, 0xD1)),
        ("octane", Color.FromArgb(255, 0x1B, 0xA3, 0x9C)),
        ("o2", Color.FromArgb(255, 0x6B, 0x5B, 0xD6)),
        ("fuel", Color.FromArgb(255, 0xC8, 0x3B, 0x3B)),
        ("tezro", Color.FromArgb(255, 0x80, 0x4B, 0xB0)),
        ("origin", Color.FromArgb(255, 0x55, 0x58, 0x60)),
        ("onyx", Color.FromArgb(255, 0x44, 0x55, 0x6E)),
    };

    /// <summary>The product's chassis colour; an unknown product gets a stable one from its name.</summary>
    static Color ChassisColor(string product)
    {
        var p = product.ToLowerInvariant();
        foreach (var (m, c) in Chassis) if (p.Contains(m)) return c;
        var h = (uint)p.GetHashCode();
        return Color.FromArgb(255, (byte)(80 + h % 120), (byte)(80 + (h >> 8) % 120), (byte)(80 + (h >> 16) % 120));
    }


    readonly Dictionary<string, (Border Pill, TextBlock Text)> _pills = new();

    static string Detail(MachineSummary m) =>
        $"{m.ProductLabel}, {m.Nodes} node{(m.Nodes == 1 ? "" : "s")}, {m.MemoryPerNodeMiB} MB";

    // Clearing the list raises SelectionChanged, which would refresh and rebuild again mid-Clear (COMException 0x8000FFFF).
    bool _rebuilding;

    /// <summary>Rebuilds the library list after machines were added.</summary>
    public void Reload()
    {
        _rebuilding = true;
        try { Library.Items.Clear(); _pills.Clear(); _rowStills.Clear(); BuildLibrary(); Library.SelectedIndex = 0; }
        finally { _rebuilding = false; }
        Refresh();
    }

    /// <summary>Rebuilds the list when the core's library changed (the first read can land after the window opens), keeping the selection.</summary>
    void SyncLibrary()
    {
        var keys = _core.Machines().Select(x => x.Key).ToList();
        var shown = Library.Items.OfType<ListViewItem>().Select(i => (string?)i.Tag).ToList();
        if (keys.SequenceEqual(shown!)) return;
        var sel = SelectedKey;
        _rebuilding = true;
        try
        {
            Library.Items.Clear(); _pills.Clear(); _rowStills.Clear();
            BuildLibrary();
            Library.SelectedIndex = sel is not null && keys.IndexOf(sel) is var at and >= 0 ? at : (keys.Count > 0 ? 0 : -1);
        }
        finally { _rebuilding = false; }
    }

    void BuildLibrary()
    {
        LibraryStills = 0;
        foreach (var m in _core.Machines())
        {
            FrameworkElement glyph = new Border { Width = 6, Height = 34, CornerRadius = new CornerRadius(3), Background = new SolidColorBrush(ChassisOf(m)) };
            if (StillOf(m) is { } still && RowArt(still.Id) is { } img)
            {
                // The 34 pt treated still, shown as it is: no badge, whatever the machine's review status.
                var pic = new Image { Width = 34, Height = 34, Source = img };
                _rowStills.Add((pic, still.Id));
                glyph = pic;
                LibraryStills++;
            }
            var name = new TextBlock { Text = m.Name, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
            var detail = new TextBlock { Text = Detail(m), FontSize = 12, Opacity = 0.7 };
            var ptext = new TextBlock { Foreground = new SolidColorBrush(Colors.White), FontSize = 11 };
            var pill = new Border { CornerRadius = new CornerRadius(8), Padding = new Thickness(7, 1, 7, 1), Child = ptext, HorizontalAlignment = HorizontalAlignment.Left };
            var text = new StackPanel { Spacing = 2, Children = { name, detail, pill } };
            var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10, Children = { glyph, text } };
            var item = new ListViewItem { Content = row, Tag = m.Key };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(item, "library." + m.Key);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, m.Name);
            _pills[m.Key] = (pill, ptext);
            item.DoubleTapped += (_, _) => OpenInNewTab(m.Key);
            var menu = new MenuFlyout();
            var openTab = new MenuFlyoutItem { Text = "Open in New Tab" };
            openTab.Click += (_, _) => OpenInNewTab(m.Key);
            var show = new MenuFlyoutItem { Text = "Show in Explorer" };
            show.Click += (_, _) => ShowMachineInExplorer(m.Key);
            menu.Items.Add(openTab); menu.Items.Add(show);
            item.ContextFlyout = menu;
            Library.Items.Add(item);
        }
    }

    // ---- tabs ---------------------------------------------------------------------------------

    readonly TabSet _tabs = new();
    string _tabSig = "";
    public int TabCount => _tabs.Keys.Count;
    public string? ActiveTabKey => _tabs.Active;
    public bool TabBarVisible => TabBar.Visibility == Visibility.Visible;
    public IReadOnlyList<string> RowMenuTexts(string key) =>
        Library.Items.OfType<ListViewItem>().First(i => (string?)i.Tag == key).ContextFlyout is MenuFlyout f ? f.Items.OfType<MenuFlyoutItem>().Select(i => i.Text).ToList() : new List<string>();
    public string CardsText => string.Join(" | ", Cards.Children.OfType<Border>().Select(b => ((StackPanel)b.Child).Children.OfType<TextBlock>().Select(t => t.Text).Aggregate((a, c) => a + " " + c)));

    ListViewItem? RowFor(string key) => Library.Items.OfType<ListViewItem>().FirstOrDefault(i => (string?)i.Tag == key);

    public void SelectForTabs(string key) { if (RowFor(key) is { } row) Library.SelectedItem = row; }

    public void OpenInNewTab(string key)
    {
        _tabs.OpenNew(key);
        if (RowFor(key) is { } row && !ReferenceEquals(Library.SelectedItem, row)) Library.SelectedItem = row; else Refresh();
    }

    void ShowMachineInExplorer(string key)
    {
        var dir = key.StartsWith(@"\\?\") ? key[4..] : key;
        if (Directory.Exists(dir)) System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("explorer.exe", $"\"{dir}\"") { UseShellExecute = true });
    }

    /// <summary>Closing a tab never stops anything silently: a live machine asks Keep Running / Power Off / Cancel.</summary>
    public async Task CloseTabAsync(string key, bool ask = true)
    {
        var m = _core.Machines().FirstOrDefault(x => x.Key == key);
        if (ask && m is { State: MachineState.Running or MachineState.Starting or MachineState.Paused })
        {
            var dlg = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, Title = $"Close {m.Name}?",
                Content = "The machine is running. Closing its tab can leave it running or power it off.",
                PrimaryButtonText = "Keep Running", SecondaryButtonText = "Power Off", CloseButtonText = "Cancel", DefaultButton = ContentDialogButton.Primary,
            };
            var r = await dlg.ShowAsync();
            if (r == ContentDialogResult.None) return;
            if (r == ContentDialogResult.Secondary && !await StopClean(m))
            {
                StatusText.Text = $"{m.Name} did not power off; its tab stays open.";
                return; // the machine is still up: keep the tab
            }
        }
        var next = _tabs.Close(key);
        if (next != null && RowFor(next) is { } row) Library.SelectedItem = row; else Library.SelectedItem = null;
        Refresh();
    }

    /// <summary>Rebuilds the tab strip when the set, the active tab or a state changes.</summary>
    void RebuildTabs()
    {
        _tabs.Retain(_core.Machines().Select(x => x.Key));
        var machines = _core.Machines().ToDictionary(x => x.Key);
        var sig = string.Join("|", _tabs.Keys.Select(k => k + ":" + (machines.TryGetValue(k, out var mm) ? mm.State : MachineState.Stopped))) + "@" + _tabs.Active;
        TabBar.Visibility = Visibility.Visible; // always shown, as in the other apps, even with one tab
        if (sig == _tabSig) return;
        _tabSig = sig;
        TabStrip.Children.Clear();
        foreach (var key in _tabs.Keys)
        {
            if (!machines.TryGetValue(key, out var info)) continue;
            var active = key == _tabs.Active;
            var chassis = ChassisColor(info.Product);
            var dot = new Microsoft.UI.Xaml.Shapes.Ellipse { Width = 8, Height = 8, Fill = new SolidColorBrush(Tones.For(Pill.Tone(info.State))), VerticalAlignment = VerticalAlignment.Center };
            var close = new Button
            {
                Content = new FontIcon { Glyph = "\uE711", FontSize = 9 }, Width = 18, Height = 18, Padding = new Thickness(0),
                Background = new SolidColorBrush(Colors.Transparent), BorderThickness = new Thickness(0),
                Visibility = active ? Visibility.Visible : Visibility.Collapsed,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(close, "Close tab " + info.Name);
            close.Click += async (_, _) => await CloseTabAsync(key);
            var content = new StackPanel
            {
                Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center,
                Children = { new Border { Width = 3, Height = 16, CornerRadius = new CornerRadius(2), Background = new SolidColorBrush(chassis) }, new TextBlock { Text = info.Name, VerticalAlignment = VerticalAlignment.Center, FontSize = 12 }, dot, close },
            };
            var tab = new Border
            {
                Height = Tokens.Size.TabHeight, CornerRadius = new CornerRadius(6), Padding = new Thickness(8, 0, 4, 0), Child = content,
                BorderThickness = new Thickness(1), Tag = key,
                BorderBrush = new SolidColorBrush(active ? Color.FromArgb((byte)(255 * Tokens.Chassis.TabSelectedStroke), chassis.R, chassis.G, chassis.B) : Colors.Transparent),
                Background = new SolidColorBrush(active ? Color.FromArgb((byte)(255 * Tokens.Chassis.TabSelectedFill), chassis.R, chassis.G, chassis.B) : Colors.Transparent),
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(tab, "tab." + key);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(tab, info.Name);
            tab.PointerEntered += (_, _) => close.Visibility = Visibility.Visible;
            tab.PointerExited += (_, _) => close.Visibility = active ? Visibility.Visible : Visibility.Collapsed;
            tab.Tapped += (_, e) => { if (RowFor(key) is { } row) Library.SelectedItem = row; };
            TabStrip.Children.Add(tab);
        }
    }

    /// <summary>The screen position of a library row's centre, for a real click in a self-test.</summary>
    public (int X, int Y)? RowScreenPoint(string key)
    {
        if (RowFor(key) is not { } it) return null;
        var s = Root.XamlRoot.RasterizationScale;
        var p = it.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(it.ActualWidth / 2, it.ActualHeight / 2));
        var pt = new Win32.POINT { X = (int)(p.X * s), Y = (int)(p.Y * s) };
        Win32.ClientToScreen(Hwnd, ref pt);
        return (pt.X, pt.Y);
    }

    /// <summary>The screen position of a tab's centre, for a real click in a self-test.</summary>
    public (int X, int Y)? TabCenter(string key)
    {
        foreach (var b in TabStrip.Children.OfType<Border>())
            if ((string?)b.Tag == key && b.ActualWidth > 0) // a tab built this turn has no size until it is laid out
            {
                var s = Root.XamlRoot.RasterizationScale;
                var p = b.TransformToVisual(null).TransformPoint(new Windows.Foundation.Point(b.ActualWidth / 2 - 14, b.ActualHeight / 2));
                var pt = new Win32.POINT { X = (int)(p.X * s), Y = (int)(p.Y * s) };
                Win32.ClientToScreen(Hwnd, ref pt);
                return (pt.X, pt.Y);
            }
        return null;
    }

    /// <summary>Memory, Nodes and Topology as fixed 140x104 cards with a fixed icon box, centred as a group.</summary>
    string _cardsFor = "";

    void BuildCards(MachineSummary m)
    {
        var sig = $"{m.Key}|{m.MemoryPerNodeMiB}|{m.Nodes}|{m.Topology}|{m.Product}";
        if (sig == _cardsFor) return; // rebuilt only when the summary changes
        _cardsFor = sig;
        Cards.Children.Clear();
        foreach (var (glyph, value, label) in new[] { ("\uE950", $"{m.MemoryPerNodeMiB} MB", "Memory"), ("\uE80F", m.Nodes.ToString(), m.Nodes == 1 ? "Node" : "Nodes"), ("\uE9D9", m.Topology ?? m.Product, "Topology") })
        {
            var icon = new Border { Width = 32, Height = 32, Child = new FontIcon { Glyph = glyph, FontSize = 20, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center } };
            var body = new StackPanel
            {
                Spacing = 4, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center,
                Children = { icon, new TextBlock { Text = value, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextAlignment = TextAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis, MaxWidth = 120 }, new TextBlock { Text = label, FontSize = 11, Opacity = 0.7, TextAlignment = TextAlignment.Center } },
            };
            Cards.Children.Add(new Border
            {
                Width = Tokens.Size.CardW, Height = Tokens.Size.CardH, CornerRadius = new CornerRadius(8), Child = body,
                Background = new SolidColorBrush(Color.FromArgb(24, 128, 128, 128)), BorderThickness = new Thickness(1), BorderBrush = new SolidColorBrush(Color.FromArgb(48, 128, 128, 128)),
            });
        }
    }

    // ---- menus ------------------------------------------------------------------------------

    /// <summary>Commands only this app has; the core's table has no view or preferences commands.</summary>
    static readonly CommandInfo[] AppOnly =
    {
        new("machine.pointer-mode", "Toggle Follow / Capture", "Ctrl+Shift+M"),
        new("view.library", "Show Library", "F9"),
        new("app.preferences", "Preferences", "Ctrl+,"),
        new("app.about", "About Origami", null),
        new("view.fit", "Fit to Window", "Ctrl+9"),
        new("view.scale1x", "Scale 1x", "Ctrl+Alt+1"),
        new("view.scale2x", "Scale 2x", "Ctrl+Alt+2"),
        new("view.actual-size", "Actual Size", "Ctrl+0"),
    };

    readonly List<CommandInfo> _commands = new();
    readonly Dictionary<string, MenuFlyoutItem> _menuItems = new();
    readonly Dictionary<string, string?> _menuShortcuts = new();

    void BuildMenus()
    {
        _commands.AddRange(_core.Commands());
        _commands.AddRange(AppOnly);
        var taken = new HashSet<(bool, bool, bool, string)>();
        foreach (var group in _commands.GroupBy(c => c.Menu))
        {
            var bar = new MenuBarItem { Title = group.Key };
            foreach (var c in group)
            {
                var item = new MenuFlyoutItem { Text = c.Title };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(item, "menu." + c.Id);
                // Accelerators live on the window, not the item: a disabled item's accelerator never fires, and
                // Pause and Resume share one (Invoke turns Pause into Resume while paused).
                if (Accelerator.TryParse(c.WindowsAccelerator, out var a) && a.VirtualKey != 0 && !(a.Alt && a.Key == "F4"))
                {
                    item.KeyboardAcceleratorTextOverride = c.WindowsAccelerator;
                    if (taken.Add((a.Ctrl, a.Shift, a.Alt, a.Key)))
                    {
                        var mods = (a.Ctrl ? Windows.System.VirtualKeyModifiers.Control : 0)
                                 | (a.Shift ? Windows.System.VirtualKeyModifiers.Shift : 0)
                                 | (a.Alt ? Windows.System.VirtualKeyModifiers.Menu : 0);
                        var acc = new KeyboardAccelerator { Key = (Windows.System.VirtualKey)a.VirtualKey, Modifiers = mods };
                        var cmd = c.Id;
                        acc.Invoked += (_, e) => { Invoke(cmd); e.Handled = true; };
                        Root.KeyboardAccelerators.Add(acc);
                    }
                }
                var id = c.Id;
                item.Click += (_, _) => Invoke(id);
                _menuItems[id] = item;
                _menuShortcuts[id] = c.WindowsAccelerator;
                bar.Items.Add(item);
            }
            Menus.Items.Add(bar);
        }
    }

    // ---- commands ---------------------------------------------------------------------------

    /// <summary>Runs a command by table ID. Menus, toolbar, shortcuts and tests all come here.</summary>
    public void Invoke(string id)
    {
        var key = SelectedKey;
        MachineState? state = null;
        if (key != null) state = _core.Status(key).State;
        // The core gives Pause and Resume one shortcut and enables only one of them: pressing it toggles.
        if (id == "machine.pause" && state == MachineState.Paused) id = "machine.resume";
        // Diagnostics needs a machine; the shared gate would drop the command silently, so say why first.
        if (id == "window.diagnostics" && key == null) { StatusText.Text = "Select a machine first."; return; }
        if (!_core.CommandEnabled(id, state)) return;
        switch (id)
        {
            case "machine.start" when key != null: _ = RunAsync(() => _core.StartAsync(key)); break;
            case "machine.stop" when key != null: _ = RunAsync(() => _core.PowerOffAsync(key)); break;
            case "machine.force-quit" when key != null: _ = RunAsync(() => _core.ForceQuitAsync(key)); break;
            case "machine.reset" when key != null: _ = RunAsync(() => _core.ControlAsync(key, MachineControl.Reset)); break;
            case "machine.pause" when key != null: _ = RunAsync(() => _core.ControlAsync(key, MachineControl.Pause)); break;
            case "machine.resume" when key != null: _ = RunAsync(() => _core.ControlAsync(key, MachineControl.Resume)); break;
            case "machine.release-keys" when key != null: Display.LoseFocus(); break;
            case "machine.screenshot" when key != null: _ = ScreenshotAsync(key); break;
            case "machine.settings" when key != null && !_dialogOpen: _ = RunAsync(() => MachineSettingsAsync(key)); break;
            case "app.preferences": _prefsOpen = true; Settings.Load(_settings); Refresh(); break; // load once: a refresh must not reset unsaved edits
            case "machine.pointer-mode" when key != null:
                {
                    var next = Display.Mode == PointerMode.Follow ? PointerMode.Capture : PointerMode.Follow;
                    _settings.PointerModeByMachine[key] = next;
                    _store.Save(_settings);
                    Display.Mode = next;
                    break;
                }
            case "view.library": LibraryColumn.Width = LibraryColumn.Width.Value > 0 ? new GridLength(0) : new GridLength(260); break;
            case "window.console" when key != null: if (_paneKey == key) _pane?.FocusTerminal(); else OpenConsole(key); break;
            case "machine.new" when !_dialogOpen: _ = RunAsync(NewMachineAsync); break;
            case "window.diagnostics" when key != null: OpenDiagnostics(key); break;
            case "machine.eject-cd":
                StatusText.Text = "Not built yet: " + _commands.First(c => c.Id == id).Title; break;
            case "view.fit": SetViewMode(ViewMode.FitToWindow); break;
            case "view.scale1x": SetViewMode(ViewMode.Scale1x); break;
            case "view.scale2x": SetViewMode(ViewMode.Scale2x); break;
            case "view.actual-size": SetViewMode(ViewMode.ActualSize); break;
            case "app.about": OpenAbout(); break;
            case "app.quit": Close(); break;
        }
    }

    async Task RunAsync(Func<Task> f)
    {
        try { await f(); }
        catch (Exception e) { StatusText.Text = "Error: " + e.Message; }
    }

    bool _dialogOpen; // one ContentDialog at a time

    async Task MachineSettingsAsync(string key)
    {
        _dialogOpen = true;
        try { await MachineSettingsDialog.ShowAsync(Root.XamlRoot, _core, key, _core.Machines().First(m => m.Key == key).Name, () => Hwnd); }
        finally { _dialogOpen = false; Refresh(); }
    }

    async Task NewMachineAsync()
    {
        _dialogOpen = true;
        string? made;
        try { made = await NewMachineDialog.ShowAsync(Root.XamlRoot, _core); }
        finally { _dialogOpen = false; }
        if (made is not { } key) return;
        _prefsOpen = false;
        Reload();
        Library.SelectedItem = Library.Items.Cast<ListViewItem>().FirstOrDefault(i => (string?)i.Tag == key) ?? Library.SelectedItem;
    }

    public void ClearSelectionForTest() => Library.SelectedItem = null;
    public void SelectForTest(string key) { foreach (ListViewItem i in Library.Items) if ((string?)i.Tag == key) Library.SelectedItem = i; }
    readonly Dictionary<string, DiagnosticsWindow> _diags = new();
    public DiagnosticsWindow? DiagnosticsForTest(string key) => _diags.GetValueOrDefault(key);

    void OpenDiagnostics(string key)
    {
        if (!_diags.TryGetValue(key, out var w))
        {
            w = new DiagnosticsWindow(_core, key, () => (Display.Wakes, Display.Presents));
            w.Closed += (_, _) => _diags.Remove(key);
            _diags[key] = w;
        }
        w.Activate();
    }
    public ConsoleView? ConsoleForTest(string key) => _consoles.GetValueOrDefault(key)?.View;
    bool _sized;
    const double MinWidthDips = 900, MinHeightDips = 600;

    PixelRect WindowRect => new(AppWindow.Position.X, AppWindow.Position.Y, AppWindow.Size.Width, AppWindow.Size.Height);
    PixelRect WorkArea
    {
        get
        {
            var w = Microsoft.UI.Windowing.DisplayArea.GetFromWindowId(AppWindow.Id, Microsoft.UI.Windowing.DisplayAreaFallback.Nearest).WorkArea;
            return new(w.X, w.Y, w.Width, w.Height);
        }
    }

    void MoveResize(PixelRect r) => AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(r.X, r.Y, r.W, r.H));

    /// <summary>The opening size, in DIPs, kept inside the work area.</summary>
    public void SizeToDips(double w, double h)
    {
        var s = Win32.GetDpiForWindow(Hwnd) / 96.0;
        MoveResize(ViewScale.Within(WindowRect, WorkArea, (int)Math.Round(w * s), (int)Math.Round(h * s)));
    }

    /// <summary>Once per run, when the first picture arrives: size the window so the picture shows at 1 DIP per guest pixel, in the work area.
    /// A maximised, minimised or full-screen window is left alone.</summary>
    void SizeForPicture()
    {
        if (_sized || Display.HeadWidth <= 0) return;
        _sized = true;
        if (AppWindow.Presenter is not Microsoft.UI.Windowing.OverlappedPresenter { State: Microsoft.UI.Windowing.OverlappedPresenterState.Restored }) return;
        var (pw, ph) = Display.PaneSize;
        MoveResize(ViewScale.WindowFor(WindowRect, WorkArea, Root.XamlRoot.RasterizationScale, pw, ph, Display.HeadWidth, Display.HeadHeight, MinWidthDips, MinHeightDips));
    }

    /// <summary>Self-test: the window lies in its monitor's work area.</summary>
    public bool InsideWorkArea
    {
        get { var (r, wa) = (WindowRect, WorkArea); return r.X >= wa.X - 2 && r.Y >= wa.Y - 2 && r.X + r.W <= wa.X + wa.W + 2 && r.Y + r.H <= wa.Y + wa.H + 2; }
    }

    void SetViewMode(ViewMode mode)
    {
        Display.ViewMode = mode; _viewStore.Save(mode);
        StatusText.Text = "Picture size: " + mode switch { ViewMode.FitToWindow => "fit to window", ViewMode.Scale1x => "scale 1x", ViewMode.Scale2x => "scale 2x", _ => "actual size" };
    }

    public ViewMode CurrentViewMode => Display.ViewMode;

    AboutWindow? _about;
    public AboutWindow? AboutForTest => _about;

    void OpenAbout()
    {
        if (_about != null) { _about.Activate(); return; }
        // The cube shows a running machine's screen only while AboutWindow.ReflectsRunningMachine is on.
        var key = SelectedKey;
        var live = key != null && _core.Status(key).State is MachineState.Running && Display.Attached ? key : null;
        _about = new AboutWindow(_core, live, Dark);
        _about.Closed += (_, _) => _about = null;
        _about.Activate();
    }

    string? _lastShot;
    public string? LastScreenshot => _lastShot;
    public bool ShowInExplorerVisible => ShowShot.Visibility == Visibility.Visible;

    async Task ScreenshotAsync(string key)
    {
        try
        {
            var name = _core.Machines().FirstOrDefault(x => x.Key == key)?.Name ?? "Origami";
            _lastShot = await Screenshots.SaveAsync(_core, key, name);
            StatusText.Text = "Screenshot saved: " + Path.GetFileName(_lastShot);
            ShowShot.Visibility = Visibility.Visible;
        }
        catch (Exception e) { StatusText.Text = "Screenshot failed: " + e.Message; }
    }

    void ShowInExplorer()
    {
        if (_lastShot is { } p && File.Exists(p))
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("explorer.exe", $"/select,\"{p}\"") { UseShellExecute = true });
    }

    void OpenConsole(string key)
    {
        if (!_consoles.TryGetValue(key, out var w))
        {
            w = new ConsoleWindow(_core, key);
            w.Closed += (_, _) => _consoles.Remove(key);
            _consoles[key] = w;
        }
        w.Activate();
    }

    // ---- view -------------------------------------------------------------------------------

    void RefreshPointerUi()
    {
        PointerButton.IsChecked = Display.Mode == PointerMode.Capture;
        PointerButton.Label = Display.Mode == PointerMode.Capture ? "Capture" : "Follow";
        PointerStatus.Text = Display.Captured ? "Pointer captured (Ctrl+Alt releases)"
            : Display.Mode == PointerMode.Capture ? "Capture on click" : "Following your mouse";
    }

    void Refresh()
    {
        SyncLibrary();
        RebuildTabs();
        foreach (var m in _core.Machines())
        {
            if (!_pills.TryGetValue(m.Key, out var p)) continue;
            p.Text.Text = Pill.Text(m.State, !m.HasDisplay);
            p.Pill.Background = new SolidColorBrush(Tones.For(Pill.Tone(m.State)));
        }

        var key = SelectedKey;
        EmptyPanel.Visibility = Visibility.Collapsed;
        // The cube holds a child window and a GPU surface, so it lives only while the starting pane shows.
        var starting = key != null && !_prefsOpen && _core.Status(key).State == MachineState.Starting;
        if (!starting) StartCube.Close();
        if (key is not null || _prefsOpen) EmptyCube.Close();
        HeroPanel.Visibility = StartingPanel.Visibility = Display.Visibility = Tint.Visibility = Visibility.Collapsed;
        if (_pane is not null) _pane.Visibility = Visibility.Collapsed;
        Settings.Visibility = _prefsOpen ? Visibility.Visible : Visibility.Collapsed;
        if (key is null)
        {
            EmptyPanel.Visibility = Visibility.Visible;
            var any = _core.Machines().Count > 0;
            EmptyTitle.Text = any ? "Choose a machine" : "No machines yet";
            EmptyHint.Text = any ? "Pick one in the library." : "Create one with the origami command line; it appears here.";
            if (!_prefsOpen) ShowEmptyCube();
            return;
        }

        var info = _core.Machines().First(m => m.Key == key);
        var st = _core.Status(key);
        TitleText.Text = info.Name;
        PillText.Text = Pill.Text(st.State, !info.HasDisplay);
        PillBorder.Background = new SolidColorBrush(Tones.For(Pill.Tone(st.State)));
        StatusText.Text = st.LastError is { Length: > 0 } e ? st.Reason + " " + e : st.Reason;
        foreach (var (cid, mi) in _menuItems) mi.IsEnabled = _core.CommandEnabled(cid, st.State);
        StartButton.IsEnabled = HeroStart.IsEnabled = _core.CommandEnabled("machine.start", st.State);
        HeroStart.Content = st.State == MachineState.LeftRunning ? "Reconnect" : "Start";
        PowerOffButton.IsEnabled = _core.CommandEnabled("machine.stop", st.State);
        var paused = st.State == MachineState.Paused;
        if (PauseButton.Label != (paused ? "Resume" : "Pause"))
        {
            PauseButton.Label = paused ? "Resume" : "Pause";
            PauseButton.Icon = new SymbolIcon(paused ? Symbol.Play : Symbol.Pause);
        }
        PauseButton.IsEnabled = _core.CommandEnabled(paused ? "machine.resume" : "machine.pause", st.State);

        // A machine that ended on its own says why, from what the core reports.
        ExitNote.Text = Pill.StoppedNote(st);
        ExitNote.Visibility = ExitNote.Text.Length > 0 ? Visibility.Visible : Visibility.Collapsed;

        HeroName.Text = info.Name;
        HeroDetail.Text = Detail(info);
        BuildCards(info);
        HeroBand.Fill = new SolidColorBrush(ChassisOf(info));
        HeroImage.Source = StillOf(info) is { } hs ? Art(hs.Id, "hero@2x.png") : null;
        HeroImage.Visibility = HeroImage.Source is null ? Visibility.Collapsed : Visibility.Visible;
        StageText.Text = st.Reason;

        if (_prefsOpen) { Display.Hide(); return; }
        switch (st.State)
        {
            case MachineState.Stopped:
            case MachineState.Exited:
            case MachineState.LeftRunning: // the hero, with Start attaching to the runner that is still up
                Detach();
                ShowTint(ChassisOf(info));
                HeroPanel.Visibility = Visibility.Visible; break;
            case MachineState.Starting:
                Detach();
                StartingPanel.Visibility = Visibility.Visible;
                ShowStartCube(info);
                break;
            case var _ when !info.HasDisplay: // serial only: the console is the machine's view
                Display.Detach(); _attachedKey = _modeKey = null;
                ShowConsolePane(key);
                break;
            default:
                ClosePane();
                Display.Visibility = Visibility.Visible;
                Display.Show();
                if (_modeKey != key) { _modeKey = key; Display.Mode = _settings.ModeFor(key); }
                if (!Display.Attached || _attachedKey != key)
                {
                    // The panel must be laid out before the child window is placed over it.
                    _ui.TryEnqueue(DispatcherQueuePriority.Low, () => { if (Display.Attach(_core, key)) { _attachedKey = key; SizeForPicture(); } });
                }
                break;
        }
        RefreshPointerUi();
    }

    /// <summary>The window's background as 0xRRGGBB; the cube's child window has no alpha, so it is filled with this.</summary>
    uint WindowBackground()
    {
        if (Application.Current.Resources.TryGetValue("SolidBackgroundFillColorBase", out var v) && v is Color c)
            return (uint)(c.R << 16 | c.G << 8 | c.B);
        return Dark ? 0x202020u : 0xF3F3F3u;
    }

    /// <summary>The live cube, spinning, on the window's own background, with the machine's chassis colour in the studio horizon.</summary>
    void ShowStartCube(MachineSummary info)
    {
        var reduce = !new Windows.UI.ViewManagement.UISettings().AnimationsEnabled;
        StartCube.WindowHandle = () => Hwnd;
        StartCube.SetStyle(Frame.CubeEnvStudio, 0, StillOf(info)?.Rgb, Dark, true, reduce, WindowBackground());
        StartCube.Show(true);
    }

    Color? _tintFor;

    /// <summary>The chassis colour at the token's top alpha fading to clear at the bottom, over the window colour.</summary>
    void ShowTint(Color chassis)
    {
        if (_tintFor != chassis)
        {
            _tintFor = chassis;
            var brush = new LinearGradientBrush { StartPoint = new Windows.Foundation.Point(0.5, 0), EndPoint = new Windows.Foundation.Point(0.5, 1) };
            brush.GradientStops.Add(new GradientStop { Color = Color.FromArgb(PaneTint.TopAlpha, chassis.R, chassis.G, chassis.B), Offset = 0 });
            brush.GradientStops.Add(new GradientStop { Color = Color.FromArgb(PaneTint.BottomAlpha, chassis.R, chassis.G, chassis.B), Offset = 1 });
            Tint.Background = brush;
        }
        Tint.Visibility = Visibility.Visible;
    }

    public Color? TintForTest => Tint.Visibility == Visibility.Visible ? _tintFor : null;
    public double TintTopAlphaForTest => (Tint.Background as LinearGradientBrush)?.GradientStops[0].Color.A ?? -1;
    public double TintBottomAlphaForTest => (Tint.Background as LinearGradientBrush)?.GradientStops[1].Color.A ?? -1;

    /// <summary>The live cube, slowly turning, on the window's own background, while no machine is chosen.</summary>
    void ShowEmptyCube()
    {
        var reduce = !new Windows.UI.ViewManagement.UISettings().AnimationsEnabled;
        EmptyCube.WindowHandle = () => Hwnd;
        EmptyCube.SetStyle(Frame.CubeEnvStudio, 0, null, Dark, true, reduce, WindowBackground());
        EmptyCube.Show(true);
    }

    public CubeHost EmptyCubeForTest => EmptyCube;
    public string EmptyTitleForTest => EmptyTitle.Text;

    /// <summary>Self-test hook: the starting pane as it looks while a machine starts.</summary>
    public void PreviewStarting()
    {
        _prefsOpen = false;
        Display.Hide(); // the screen's child window is not hidden by collapsing its pane
        HeroPanel.Visibility = Display.Visibility = Visibility.Collapsed;
        StartingPanel.Visibility = Visibility.Visible;
        ShowStartCube(_core.Machines().First(x => x.Key == SelectedKey));
    }
    public CubeHost StartCubeForTest => StartCube;
    public void Restore() => Refresh();

    void Detach() { Display.Detach(); _attachedKey = _modeKey = null; ClosePane(); }

    ConsoleView? _pane;
    string? _paneKey;
    public ConsoleView? ConsolePaneForTest => _pane is { Visibility: Visibility.Visible } ? _pane : null;

    void ShowConsolePane(string key)
    {
        if (_paneKey != key) ClosePane();
        if (_pane is null)
        {
            try { _pane = new ConsoleView(_core, key); }
            catch (Exception e) { StatusText.Text = "Console: " + e.Message; return; }
            _paneKey = key;
            Stage.Children.Add(_pane);
            _pane.Loaded += (_, _) => _pane?.FocusTerminal();
        }
        _pane.Visibility = Visibility.Visible;
    }

    void ClosePane()
    {
        if (_pane is null) return;
        Stage.Children.Remove(_pane); _pane.Dispose(); _pane = null; _paneKey = null;
    }


    // ---- quit -------------------------------------------------------------------------------

    List<MachineSummary> Up() => _core.Machines()
        .Where(m => m.State is MachineState.Running or MachineState.Starting or MachineState.Paused or MachineState.Stopping or MachineState.LeftRunning).ToList();

    /// <summary>The names of the machines that are up now, read from the core (not from what the window last showed).</summary>
    public IReadOnlyList<string> UpNames => Up().Select(m => m.Name).ToList();

    bool _quitting;

    /// <summary>Clean stop, waited for up to a minute; a Stopping or Busy machine is waited on, not failed.</summary>
    async Task<bool> StopClean(MachineSummary m)
    {
        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(60);
        bool Ended() => _core.Machines().All(x => x.Key != m.Key || x.State is MachineState.Stopped or MachineState.Exited);
        if (_core.Machines().FirstOrDefault(x => x.Key == m.Key)?.State != MachineState.Stopping)
        {
            var clean = _core.PowerOffAsync(m.Key);
            try
            {
                if (await Task.WhenAny(clean, Task.Delay(deadline - DateTime.UtcNow)) != clean) return false;
                await clean;
                return true;
            }
            catch (CoreBusyException) { /* a stop is already running: wait for it below */ }
            catch (Exception) { return Ended(); }
        }
        while (DateTime.UtcNow < deadline)
        {
            if (Ended()) return true;
            await Task.Delay(500);
        }
        return false;
    }

    async void OnClosing(Microsoft.UI.Windowing.AppWindow sender, Microsoft.UI.Windowing.AppWindowClosingEventArgs args)
    {
        if (_closeApproved) return;
        if (_quitting) { args.Cancel = true; return; } // one prompt at a time, even if closed again while powering off
        var up = Up(); // live state, so only machines that are up right now are named
        if (up.Count == 0 || !_settings.ConfirmQuitWithRunningMachines) { _recheck?.Stop(); Display.Detach(); return; }
        args.Cancel = true;
        _quitting = true;
        try
        {
            var dlg = new ContentDialog
            {
                XamlRoot = Root.XamlRoot,
                Title = up.Count == 1 ? $"{up[0].Name} is running" : $"{up.Count} machines are running",
                Content = "Machines keep running without the app. Power Off asks each to stop cleanly.",
                PrimaryButtonText = "Keep Running",
                SecondaryButtonText = "Power Off",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Primary,
            };
            var r = await dlg.ShowAsync();
            if (r == ContentDialogResult.None) return;
            if (r == ContentDialogResult.Secondary && !await PowerOffAll(up)) return;
            _closeApproved = true;
            _recheck?.Stop();
            Display.Detach();
            Close();
        }
        finally { _quitting = false; }
    }

    /// <summary>Powers machines off one at a time behind a progress panel; on a failure, offers Force Quit. False cancels the quit.</summary>
    async Task<bool> PowerOffAll(List<MachineSummary> machines)
    {
        bool Ended(MachineSummary m) => _core.Machines().All(x => x.Key != m.Key || x.State is MachineState.Stopped or MachineState.Exited);
        var todo = machines.Where(m => !Ended(m)).ToList();
        if (todo.Count == 0) return true; // everything ended already: no panel
        var text = new TextBlock { TextWrapping = TextWrapping.Wrap };
        var panel = new ContentDialog
        {
            XamlRoot = Root.XamlRoot, Title = "Powering off",
            Content = new StackPanel { Spacing = 12, Children = { text, new ProgressBar { IsIndeterminate = true } } },
        };
        var shown = panel.ShowAsync().AsTask();
        var failed = new List<MachineSummary>();
        try
        {
            foreach (var m in todo)
            {
                text.Text = $"Powering off {m.Name}…";
                if (Ended(m)) continue; // ended meanwhile
                if (!await StopClean(m)) failed.Add(m);
            }
        }
        finally
        {
            panel.Hide(); // always closed, whatever happened above
            await shown;  // the next dialog opens only after this one has closed
        }
        if (failed.Count == 0) return true;
        var alert = new ContentDialog
        {
            XamlRoot = Root.XamlRoot,
            Title = failed.Count == 1 ? $"{failed[0].Name} did not power off" : $"{failed.Count} machines did not power off",
            Content = "A clean stop can take a while while a disk folds its changes. Force Quit ends the emulator at once.",
            PrimaryButtonText = "Force Quit Instead",
            SecondaryButtonText = "Quit and Leave Running",
            CloseButtonText = "Cancel Quit",
            DefaultButton = ContentDialogButton.Close,
        };
        var choice = await alert.ShowAsync();
        if (choice == ContentDialogResult.None) return false;
        if (choice == ContentDialogResult.Primary)
            foreach (var m in failed) await RunAsync(() => _core.ForceQuitAsync(m.Key));
        return true;
    }
}
