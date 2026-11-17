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

    public MainWindow(ICoreClient core, SettingsStore store)
    {
        _core = core; _store = store; _settings = store.Load();
        _ui = DispatcherQueue.GetForCurrentThread();
        InitializeComponent();
        Title = "Origami";
        Display.WindowHandle = () => WinRT.Interop.WindowNative.GetWindowHandle(this);
        Display.PointerModeChanged = RefreshPointerUi;
        Display.Shortcut = OnDisplayShortcut;

        BuildMenus();
        BuildLibrary();

        Library.SelectionChanged += (_, _) => { _prefsOpen = false; Refresh(); };
        StartButton.Click += (_, _) => Invoke("machine.start");
        HeroStart.Click += (_, _) => Invoke("machine.start");
        PowerOffButton.Click += (_, _) => Invoke("machine.stop");
        ConsoleButton.Click += (_, _) => Invoke("window.console");
        SettingsButton.Click += (_, _) => Invoke("app.preferences");
        PointerButton.Click += (_, _) => Invoke("machine.pointer-mode");

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

        Activated += (_, a) => { if (a.WindowActivationState == WindowActivationState.Deactivated) Display.LoseFocus(); };
        AppWindow.Closing += OnClosing;
        Library.SelectedIndex = 0;
        try { Refresh(); } catch (Exception e) { App.Log("first refresh failed: " + e.Message); }
    }

    // ---- accessors for the automation (self-test) hooks ------------------------------------

    public DisplayView DisplayViewForTest => Display;

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

    // ---- library ----------------------------------------------------------------------------

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

    static Color ToneColor(string tone) => tone switch
    {
        "green" => Color.FromArgb(255, 0x2E, 0x8B, 0x57),
        "amber" => Color.FromArgb(255, 0xB5, 0x7A, 0x00),
        "red" => Color.FromArgb(255, 0xC0, 0x39, 0x2B),
        _ => Color.FromArgb(255, 0x6E, 0x6E, 0x78),
    };

    readonly Dictionary<string, (Border Pill, TextBlock Text)> _pills = new();

    static string Detail(MachineSummary m) =>
        $"{m.ProductLabel}, {m.Nodes} node{(m.Nodes == 1 ? "" : "s")}, {m.MemoryPerNodeMiB} MB";

    /// <summary>Rebuilds the library list after machines were added.</summary>
    public void Reload()
    {
        Library.Items.Clear(); _pills.Clear();
        BuildLibrary();
        Library.SelectedIndex = 0;
        Refresh();
    }

    /// <summary>Rebuilds the list when the core's library changed (the first read can land after the window opens), keeping the selection.</summary>
    void SyncLibrary()
    {
        var keys = _core.Machines().Select(x => x.Key).ToList();
        var shown = Library.Items.OfType<ListViewItem>().Select(i => (string?)i.Tag).ToList();
        if (keys.SequenceEqual(shown!)) return;
        var sel = SelectedKey;
        Library.Items.Clear(); _pills.Clear();
        BuildLibrary();
        Library.SelectedIndex = sel is not null && keys.IndexOf(sel) is var at and >= 0 ? at : (keys.Count > 0 ? 0 : -1);
    }

    void BuildLibrary()
    {
        foreach (var m in _core.Machines())
        {
            var glyph = new Border { Width = 6, Height = 34, CornerRadius = new CornerRadius(3), Background = new SolidColorBrush(ChassisColor(m.Product)) };
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
            Library.Items.Add(item);
        }
    }

    // ---- menus ------------------------------------------------------------------------------

    /// <summary>Commands only this app has; the core's table has no view or preferences commands.</summary>
    static readonly CommandInfo[] AppOnly =
    {
        new("machine.pointer-mode", "Toggle Follow / Capture", "Ctrl+Shift+M"),
        new("view.library", "Show Library", "F9"),
        new("app.preferences", "Preferences", "Ctrl+,"),
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
            case "machine.screenshot" when key != null:
                {
                    var dir = Environment.GetFolderPath(Environment.SpecialFolder.MyPictures);
                    var path = Path.Combine(dir, $"Origami {DateTime.Now:yyyy-MM-dd HH-mm-ss}.ppm");
                    _ = RunAsync(() => Task.Run(() => _core.Screendump(key, 0, path)));
                    StatusText.Text = "Screenshot saved to " + path;
                    break;
                }
            case "machine.settings":
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
            case "window.console" when key != null: OpenConsole(key); break;
            case "machine.new" when !_newOpen: _ = RunAsync(NewMachineAsync); break;
            case "machine.eject-cd": case "window.diagnostics":
                StatusText.Text = "Not built yet: " + _commands.First(c => c.Id == id).Title; break;
            case "app.quit": Close(); break;
        }
    }

    async Task RunAsync(Func<Task> f)
    {
        try { await f(); }
        catch (Exception e) { StatusText.Text = "Error: " + e.Message; }
    }

    bool _newOpen; // one ContentDialog at a time

    async Task NewMachineAsync()
    {
        _newOpen = true;
        string? made;
        try { made = await NewMachineDialog.ShowAsync(Root.XamlRoot, _core); }
        finally { _newOpen = false; }
        if (made is not { } key) return;
        _prefsOpen = false;
        Reload();
        Library.SelectedItem = Library.Items.Cast<ListViewItem>().FirstOrDefault(i => (string?)i.Tag == key) ?? Library.SelectedItem;
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
        foreach (var m in _core.Machines())
        {
            if (!_pills.TryGetValue(m.Key, out var p)) continue;
            p.Text.Text = Pill.Text(m.State);
            p.Pill.Background = new SolidColorBrush(ToneColor(Pill.Tone(m.State)));
        }

        var key = SelectedKey;
        HeroPanel.Visibility = StartingPanel.Visibility = Display.Visibility = Visibility.Collapsed;
        Settings.Visibility = _prefsOpen ? Visibility.Visible : Visibility.Collapsed;
        if (key is null) return;

        var info = _core.Machines().First(m => m.Key == key);
        var st = _core.Status(key);
        TitleText.Text = info.Name;
        PillText.Text = Pill.Text(st.State);
        PillBorder.Background = new SolidColorBrush(ToneColor(Pill.Tone(st.State)));
        StatusText.Text = st.LastError is { Length: > 0 } e ? st.Reason + " " + e : st.Reason;
        foreach (var (cid, mi) in _menuItems) mi.IsEnabled = _core.CommandEnabled(cid, st.State);
        StartButton.IsEnabled = HeroStart.IsEnabled = _core.CommandEnabled("machine.start", st.State);
        PowerOffButton.IsEnabled = _core.CommandEnabled("machine.stop", st.State);

        HeroName.Text = info.Name;
        HeroDetail.Text = Detail(info);
        HeroBand.Fill = new SolidColorBrush(ChassisColor(info.Product));
        StageText.Text = st.Reason;

        if (_prefsOpen) { Display.Hide(); return; }
        switch (st.State)
        {
            case MachineState.Stopped:
            case MachineState.Exited:
                Detach();
                HeroPanel.Visibility = Visibility.Visible; break;
            case MachineState.Starting:
                Detach();
                StartingPanel.Visibility = Visibility.Visible; break;
            default:
                Display.Visibility = Visibility.Visible;
                Display.Show();
                if (_modeKey != key) { _modeKey = key; Display.Mode = _settings.ModeFor(key); }
                if (!Display.Attached || _attachedKey != key)
                {
                    // The panel must be laid out before the child window is placed over it.
                    _ui.TryEnqueue(DispatcherQueuePriority.Low, () => { if (Display.Attach(_core, key)) _attachedKey = key; });
                }
                break;
        }
        RefreshPointerUi();
    }

    void Detach() { Display.Detach(); _attachedKey = _modeKey = null; }


    // ---- quit -------------------------------------------------------------------------------

    List<MachineSummary> Up() => _core.Machines()
        .Where(m => m.State is MachineState.Running or MachineState.Starting or MachineState.Paused or MachineState.Stopping).ToList();

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
        if (up.Count == 0 || !_settings.ConfirmQuitWithRunningMachines) { Display.Detach(); return; }
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
