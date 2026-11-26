using uniffi.origami_ffi;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Automation;
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
    bool _prefsOpen, _closeApproved, _installQuitApproved;
    string? _attachedKey, _modeKey;
    int _refreshQueued;

    // ---- upgrading an older machine ----------------------------------------------------------

    readonly Dictionary<string, Problem> _upgradeFailed = new();
    readonly HashSet<string> _upgrading = new();

    public bool UpgradeCardForTest => CardKindsForTest.Contains(CardActionKind.Upgrade);
    public string UpgradeErrorForTest => _upgradeFailed.Values.FirstOrDefault()?.Message ?? "";

    async void RunUpgrade()
    {
        if (SelectedKey is not { } key || !_upgrading.Add(key)) return;
        _upgradeFailed.Remove(key);
        var name = _core.Machines().FirstOrDefault(m => m.Key == key)?.Name ?? "";
        Refresh();
        try
        {
            await _core.UpgradeAsync(key);
            Say(OrigamiFfiMethods.OrigamiToast(new OrigamiToastKind.Upgraded(name), OrigamiPlatform.Windows).Text);
        }
        catch (Exception e) { _upgradeFailed[key] = _core.ProblemOf(e); }
        _upgrading.Remove(key);
        Refresh();
    }

    /// <summary>Upgrade with firmware files the person picks, after a download failed.</summary>
    async Task ChooseFirmwareAsync(string key)
    {
        PathPicker.Hwnd = Hwnd;
        if (await PathPicker.PickFileAsync() is not { } prom) return;
        var ioProm = await PathPicker.PickFileAsync();
        if (!_upgrading.Add(key)) return;
        _upgradeFailed.Remove(key);
        var name = _core.Machines().FirstOrDefault(m => m.Key == key)?.Name ?? "";
        Refresh();
        try
        {
            await _core.UpgradeWithAsync(key, prom, ioProm);
            Say(OrigamiFfiMethods.OrigamiToast(new OrigamiToastKind.Upgraded(name), OrigamiPlatform.Windows).Text);
        }
        catch (Exception e) { _upgradeFailed[key] = _core.ProblemOf(e); }
        _upgrading.Remove(key);
        Refresh();
    }

    public MainWindow(ICoreClient core, SettingsStore store)
    {
        _core = core; _store = store; _settings = store.Load();
        _ui = DispatcherQueue.GetForCurrentThread();
        InitializeComponent();
        Title = AppTitleBar.Title = Ui.T(OrigamiUiString.Origami);
        HeroStart.Content = Ui.T(OrigamiUiString.Start);
        NewMachineText.Text = Ui.T(OrigamiUiString.NewMachine);
        StartingText.Text = Ui.T(OrigamiUiString.StartingUpTheSystem);
        DropHint.CornerRadius = new CornerRadius(Tokens.Radius.Control);
        if (AppIcon.Apply(this))
        {
            try { TitleIcon.ImageSource = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(AppIcon.Path)); } catch { }
        }
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        // Without Mica (Windows 10, some remote sessions) the window paints a plain theme background.
        if (!Microsoft.UI.Composition.SystemBackdrops.MicaController.IsSupported())
        {
            SystemBackdrop = null;
            Root.Background = (Brush)Application.Current.Resources["ApplicationPageBackgroundThemeBrush"];
        }
        Display.WindowHandle = () => WinRT.Interop.WindowNative.GetWindowHandle(this);
        Display.PointerModeChanged = RefreshPointerUi;
        Display.Shortcut = OnDisplayShortcut;

        LibraryHeading.Text = Labels.SidebarHeading;
        BuildToolbar();
        BuildAccelerators();
        BuildLibrary();
        ToolTipService.SetToolTip(PaneToggle, Ui.T(OrigamiUiString.ShowOrHideTheLibrary));
        AutomationProperties.SetName(PaneToggle, Ui.T(OrigamiUiString.ShowOrHideTheLibrary));
        ToolTipService.SetToolTip(GuideBack, Labels.SidebarHeading);
        AutomationProperties.SetName(GuideBack, Labels.SidebarHeading);

        Library.SelectionChanged += (_, _) => { if (_rebuilding) return; _prefsOpen = false; if (SelectedKey is { } k) _tabs.Select(k); Refresh(); };
        Library.KeyDown += (_, e) =>
        {
            // Delete is the library's, not the window's: a text box that has focus keeps it.
            if (e.Key == Windows.System.VirtualKey.Delete && SelectedKey != null) { Invoke("machine.remove"); e.Handled = true; }
        };
        PaneToggle.Click += (_, _) => Invoke("view.library");
        Library.AllowDrop = true;
        Library.DragOver += (_, e) => _ = OverAsync(e, OrigamiDropKind.DiskImage);
        Library.Drop += (_, e) => _ = DropAsync(e, OrigamiDropKind.DiskImage);
        Stage.DragOver += (_, e) => _ = OverAsync(e, OrigamiDropKind.CdImage);
        Stage.Drop += (_, e) => _ = DropAsync(e, OrigamiDropKind.CdImage);
        Stage.DragLeave += (_, _) => DropHint.Visibility = Visibility.Collapsed;
        GuideBack.Click += (_, _) => ShowGuide(false);
        GuideShow.Click += (_, _) => ShowGuide(true);
        ToolTipService.SetToolTip(GuideShow, Ui.T(OrigamiUiString.InstallIrix));
        AutomationProperties.SetName(GuideShow, Ui.T(OrigamiUiString.InstallIrix));
        NewMachineButton.Click += (_, _) => Invoke("machine.new");
        EmptyButton.Click += (_, _) => Invoke("machine.new");
        HeroStart.Click += (_, _) => Invoke("machine.start");

        Settings.Core = core;
        Settings.Ask = AskAsync;
        Settings.Saved += s => { _store.Save(s); ApplySettings(s); };
        Settings.Closed += () => { _prefsOpen = false; Refresh(); };

        // Core callbacks arrive on the core's thread; hop to the UI thread.
        // One refresh per burst; console output and pictures of an attached screen change nothing it shows.
        _core.Events += e =>
        {
            if (e.Kind == MachineEventKind.ConsoleOutput && _guideShown && e.Key == SelectedKey) _ui.TryEnqueue(RefreshGuide);
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
            _rowSigs.Clear();
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

    /// <summary>Takes a saved setting into effect at once: pointer mode and quit prompt are read from it; a new library folder is listed now.</summary>
    public void ApplySettings(AppSettings s)
    {
        var want = s.LibraryRoot is { Length: > 0 } r ? r : AppSettings.DefaultLibraryRoot();
        var newRoot = want != _core.LibraryRoot ? want : null;
        _settings = s; _modeKey = null;
        if (newRoot is not null)
            _ = Task.Run(() => { try { _core.SetLibraryRoot(newRoot); } catch (Exception e) { App.Log("library folder: " + e.Message); } });
        Refresh();
    }

    // ---- accessors for the automation (self-test) hooks ------------------------------------

    public DisplayView DisplayViewForTest => Display;
    public bool DisplayVisibleForTest => Display.Visibility == Visibility.Visible;

    /// <summary>The names of toolbar buttons with no tooltip.</summary>
    public string[] ToolbarWithoutTipForTest => Toolbar.PrimaryCommands.Concat(Toolbar.SecondaryCommands).OfType<FrameworkElement>()
        .Where(e => (e is AppBarButton || e is AppBarToggleButton) && string.IsNullOrEmpty(ToolTipService.GetToolTip(e) as string))
        .Select(e => e.Name).Concat(string.IsNullOrEmpty(ToolTipService.GetToolTip(PowerSplit) as string) ? new[] { PowerSplit.Name } : Array.Empty<string>()).ToArray();

    /// <summary>The names of the primary and the More commands, in order, for the layout check.</summary>
    public string[] PrimaryCommandNamesForTest => Toolbar.PrimaryCommands.OfType<FrameworkElement>().Where(e => e is not AppBarSeparator).Select(e => e.Name).ToArray();
    public string[] SecondaryCommandNamesForTest => Toolbar.SecondaryCommands.OfType<FrameworkElement>().Select(e => e.Name).ToArray();
    /// <summary>Whether a toolbar command is on show in the bar, not hidden for the machine's state.</summary>
    public bool CommandShownForTest(string name) => ((FrameworkElement)((FrameworkElement)Content).FindName(name)).Visibility == Visibility.Visible;

    /// <summary>What the title bar holds: its title and whether it carries anything else.</summary>
    public string TitleBarTitleForTest => AppTitleBar.Title;
    public bool TitleBarHasContentForTest => AppTitleBar.Content is not null;

    /// <summary>Presses a toolbar button the way a click does, through its automation pattern.</summary>
    public void PressToolbarForTest(string name)
    {
        var peer = Microsoft.UI.Xaml.Automation.Peers.FrameworkElementAutomationPeer.CreatePeerForElement((FrameworkElement)((FrameworkElement)Content).FindName(name));
        if (peer.GetPattern(Microsoft.UI.Xaml.Automation.Peers.PatternInterface.Toggle) is Microsoft.UI.Xaml.Automation.Provider.IToggleProvider t) t.Toggle();
        else (peer.GetPattern(Microsoft.UI.Xaml.Automation.Peers.PatternInterface.Invoke) as Microsoft.UI.Xaml.Automation.Provider.IInvokeProvider)?.Invoke();
    }

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
    /// <summary>The state word in the page header.</summary>
    public string PillTextForTest => HeaderState.Text;
    public string LibraryHeadingForTest => LibraryHeading.Text;
    public int LibraryCount => Library.Items.Count;
    /// <summary>The words of the last toast, or of the last note when no toast was needed.</summary>
    public string StatusForTest => _lastSaid;
    /// <summary>The last View menu scale command run, for the self-test: the status line it sets can be replaced at once.</summary>
    public string? LastViewCommand { get; private set; }
    public void FocusLibrary() => Library.Focus(FocusState.Programmatic);
    public bool LibraryShown => _paneOpen;
    public IReadOnlyList<CommandInfo> Commands => _commands;
    /// <summary>Command ID to its shortcut text, for the shortcuts check.</summary>
    public IReadOnlyDictionary<string, string?> Shortcuts => _shortcuts;
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


    /// <summary>A library row's parts: the name (cut in the middle to fit), the second line and the dot before it.</summary>
    sealed record RowParts(ListViewItem Item, Grid Row, TextBlock Name, TextBlock Line, ContentControl DotHost, StackPanel Text, string FullName);
    readonly Dictionary<string, RowParts> _rows = new();
    readonly Dictionary<string, Tone> _rowTone = new();
    readonly Dictionary<string, string> _rowSigs = new();
    bool _paneOpen = true;
    /// <summary>The tooltip and accessible label the core wrote for a row, for the self-test.</summary>
    public string? RowDescriptionForTest(string key) => _rows.GetValueOrDefault(key) is { } r ? AutomationProperties.GetName(r.Item) : null;
    /// <summary>The name a row shows after the middle was cut out, and the second line it shows.</summary>
    public string? RowNameForTest(string key) => _rows.GetValueOrDefault(key)?.Name.Text;
    public string? RowLineForTest(string key) => _rows.GetValueOrDefault(key)?.Line.Text;
    /// <summary>Whether a row draws a status dot beside its second line.</summary>
    public bool RowHasDotForTest(string key) => _rows.GetValueOrDefault(key)?.DotHost.Content is not null;

    // Clearing the list raises SelectionChanged, which would refresh and rebuild again mid-Clear (COMException 0x8000FFFF).
    bool _rebuilding;

    /// <summary>Rebuilds the library list after machines were added.</summary>
    public void Reload()
    {
        _rebuilding = true;
        try { Library.Items.Clear(); _rows.Clear(); _rowTone.Clear(); _rowSigs.Clear(); _rowStills.Clear(); BuildLibrary(); Library.SelectedIndex = 0; }
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
            Library.Items.Clear(); _rows.Clear(); _rowTone.Clear(); _rowSigs.Clear(); _rowStills.Clear();
            BuildLibrary();
            Library.SelectedIndex = sel is not null && keys.IndexOf(sel) is var at and >= 0 ? at : (keys.Count > 0 ? 0 : -1);
        }
        finally { _rebuilding = false; }
    }

    readonly TextBlock _probe = new() { FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };

    double NameWidth(string text)
    {
        _probe.Text = text;
        _probe.Measure(new Windows.Foundation.Size(double.PositiveInfinity, double.PositiveInfinity));
        return _probe.DesiredSize.Width;
    }

    /// <summary>WinUI trims only the end of a name; the middle is cut here, so "Octane2 desk…2" cannot hide which machine it is.</summary>
    void TrimName(RowParts r)
    {
        var available = r.Row.ActualWidth - Tokens.Size.ArtSidebar - r.Row.ColumnSpacing;
        if (available <= 0) return;
        var fit = MiddleTrim.Fit(r.FullName, available, NameWidth);
        if (r.Name.Text != fit) r.Name.Text = fit;
    }

    void BuildLibrary()
    {
        LibraryStills = 0;
        foreach (var m in _core.Machines())
        {
            FrameworkElement glyph = new Border { Width = 6, Height = Tokens.Size.ArtSidebar, CornerRadius = new CornerRadius(3), Background = new SolidColorBrush(ChassisOf(m)) };
            if (StillOf(m) is { } still && RowArt(still.Id) is { } img)
            {
                // The 34 pt treated still, shown as it is: no badge, whatever the machine's review status.
                var pic = new Image { Width = Tokens.Size.ArtSidebar, Height = Tokens.Size.ArtSidebar, Source = img };
                _rowStills.Add((pic, still.Id));
                glyph = pic;
                LibraryStills++;
            }
            var name = new TextBlock { Text = m.Name, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextWrapping = TextWrapping.NoWrap };
            var dotHost = new ContentControl { VerticalAlignment = VerticalAlignment.Center, IsTabStop = false };
            var line = new TextBlock { Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"], Foreground = Ui.Secondary, TextTrimming = TextTrimming.CharacterEllipsis };
            var text = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center,
                Children = { name, new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, Children = { dotHost, line } } } };
            var row = new Grid { ColumnSpacing = 10, ColumnDefinitions = { new ColumnDefinition { Width = GridLength.Auto }, new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) } } };
            var art = new Border { Width = Tokens.Size.ArtSidebar, Height = Tokens.Size.ArtSidebar, Child = glyph };
            row.Children.Add(art);
            Grid.SetColumn(text, 1);
            row.Children.Add(text);
            var item = new ListViewItem { Content = row, Tag = m.Key };
            AutomationProperties.SetAutomationId(item, "library." + m.Key);
            AutomationProperties.SetName(item, m.Name);
            var parts = new RowParts(item, row, name, line, dotHost, text, m.Name);
            _rows[m.Key] = parts;
            row.SizeChanged += (_, _) => TrimName(parts);
            text.Visibility = _paneOpen ? Visibility.Visible : Visibility.Collapsed;
            item.DoubleTapped += (_, _) => OpenInNewTab(m.Key);
            var menu = new MenuFlyout();
            var openTab = new MenuFlyoutItem { Text = Ui.T(OrigamiUiString.OpenInNewTab) };
            openTab.Click += (_, _) => OpenInNewTab(m.Key);
            var show = new MenuFlyoutItem { Text = OrigamiFfiMethods.OrigamiShowInLabel(OrigamiPlatform.Windows) };
            show.Click += (_, _) => ShowMachineInExplorer(m.Key);
            var remove = new MenuFlyoutItem { Text = _commands.FirstOrDefault(c => c.Id == "machine.remove")?.Title ?? "", KeyboardAcceleratorTextOverride = "Delete" };
            AutomationProperties.SetAutomationId(remove, "row.remove");
            remove.Click += (_, _) => _ = RemoveAsync(m.Key);
            menu.Opening += (_, _) => remove.IsEnabled = CommandOn("machine.remove", _core.Status(m.Key).State, m.Key);
            menu.Items.Add(openTab); menu.Items.Add(show); menu.Items.Add(remove);
            item.ContextFlyout = menu;
            Library.Items.Add(item);
        }
        RefreshRows();
    }

    /// <summary>Each row's name, state dot and second line, as the core words them; only rows whose words changed are touched.</summary>
    void RefreshRows()
    {
        foreach (var (key, r) in _rows)
        {
            var info = _core.Row(key);
            var sig = info.Word + "|" + info.Line + "|" + info.LineTone + "|" + info.Description;
            if (_rowSigs.GetValueOrDefault(key) == sig) continue;
            _rowSigs[key] = sig;
            r.Line.Text = info.Line;
            if (info.LineTone == Tone.None) r.DotHost.Content = null;
            else if (!_rowTone.TryGetValue(key, out var was) || was != info.LineTone || r.DotHost.Content is null) r.DotHost.Content = Ui.Dot(info.LineTone);
            _rowTone[key] = info.LineTone;
            AutomationProperties.SetName(r.Item, info.Description);
            ToolTipService.SetToolTip(r.Item, info.Description);
            TrimName(r);
        }
    }

    /// <summary>The pane open shows machines and their words; closed it is a rail of pictures with the toggle at its top.</summary>
    void SetPane(bool open)
    {
        _paneOpen = open;
        LibraryColumn.Width = new GridLength(open ? 260 : 52);
        LibraryHeading.Visibility = NewMachineText.Visibility = open ? Visibility.Visible : Visibility.Collapsed;
        foreach (var r in _rows.Values) r.Text.Visibility = open ? Visibility.Visible : Visibility.Collapsed;
        NewMachineButton.HorizontalContentAlignment = open ? HorizontalAlignment.Left : HorizontalAlignment.Center;
    }

    // ---- tabs ---------------------------------------------------------------------------------

    readonly TabSet _tabs = new();
    string _tabSig = "";
    public int TabCount => _tabs.Keys.Count;
    public string? ActiveTabKey => _tabs.Active;
    public bool TabBarVisible => TabBar.Visibility == Visibility.Visible;
    public IReadOnlyList<string> RowMenuTexts(string key) =>
        Library.Items.OfType<ListViewItem>().First(i => (string?)i.Tag == key).ContextFlyout is MenuFlyout f ? f.Items.OfType<MenuFlyoutItem>().Select(i => i.Text).ToList() : new List<string>();

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

    /// <summary>Closing a tab never stops anything silently: a live machine asks the core's question, Keep running, Power off or Cancel.</summary>
    public async Task CloseTabAsync(string key, bool ask = true)
    {
        var m = _core.Machines().FirstOrDefault(x => x.Key == key);
        if (ask && _installs.GetValueOrDefault(key) is { } install && InstallRunning(install))
        {
            if (!await ConfirmStopInstallsAsync(tab: true)) return;
            await install.StopAndCloseAsync();
            m = _core.Machines().FirstOrDefault(x => x.Key == key);
        }
        if (ask && m is { State: MachineState.Running or MachineState.Starting or MachineState.Paused })
        {
            var answer = await AskAsync(new OrigamiAlertKind.CloseTab(m.Name));
            if (answer is null or { Role: ButtonRole.Cancel }) return;
            if (answer.Role == ButtonRole.Destructive && !await StopClean(m))
            {
                _ = AskAsync(new OrigamiAlertKind.PowerOffFailed(new[] { m.Name })); // the tab stays open; Force quit is on offer
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
        var sig = string.Join("|", _tabs.Keys.Select(k => k + ":" + (machines.ContainsKey(k) ? _core.Row(k).State : ShownState.Stopped))) + "@" + _tabs.Active;
        if (_tabs.Active is null && SelectedKey is { } selected && machines.ContainsKey(selected)) _tabs.Select(selected); // a selection made while rebuilding the list gets its tab too
        TabBar.Visibility = _tabs.Keys.Count > 1 ? Visibility.Visible : Visibility.Collapsed; // a strip of one tab says nothing the header does not
        if (sig == _tabSig) return;
        _tabSig = sig;
        TabStrip.Children.Clear();
        foreach (var key in _tabs.Keys)
        {
            if (!machines.TryGetValue(key, out var info)) continue;
            var active = key == _tabs.Active;
            var chassis = ChassisColor(info.Product);
            var tone = _core.Row(key).Tone;
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
                Children = { new Border { Width = 3, Height = 16, CornerRadius = new CornerRadius(2), Background = new SolidColorBrush(chassis) }, new TextBlock { Text = info.Name, VerticalAlignment = VerticalAlignment.Center, FontSize = 12 } },
            };
            if (tone != Tone.None) content.Children.Add(Ui.Dot(tone));
            content.Children.Add(close);
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

    /// <summary>The core's cards (Memory, Model, System, and Nodes where several) as fixed-size cards with a fixed icon box, centred as a group.</summary>
    string _cardsFor = "";
    public string CardsText => string.Join(" | ", Cards.Children.OfType<Border>().Select(b => ((StackPanel)b.Child).Children.OfType<TextBlock>().Select(t => t.Text).Aggregate((a, c) => a + " " + c)));

    void BuildCards(MachineSummary m)
    {
        var cards = m.Cards ?? new List<SummaryCard>();
        var sig = $"{m.Key}|{string.Join(",", cards.Select(c => c.Title + c.Value))}";
        if (sig == _cardsFor) return; // rebuilt only when the summary changes
        _cardsFor = sig;
        Cards.Children.Clear();
        foreach (var (glyph, value, label) in cards.Select(c => (c.Title == "Memory" ? "\uE950" : c.Title == "Nodes" ? "\uE80F" : c.Title == "System" ? "\uE8B7" : "\uE9D9", c.Value, c.Title)))
        {
            var icon = new Border { Width = 32, Height = 32, Child = new FontIcon { Glyph = glyph, FontSize = 20, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center } };
            var body = new StackPanel
            {
                Spacing = 4, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center,
                Children = { icon, new TextBlock { Text = value, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextAlignment = TextAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis, MaxWidth = 120 }, new TextBlock { Text = label, Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"], Foreground = Ui.Secondary, TextAlignment = TextAlignment.Center } },
            };
            Cards.Children.Add(new Border
            {
                Width = Tokens.Size.CardW, Height = Tokens.Size.CardH, CornerRadius = new CornerRadius(Tokens.Radius.Card), Child = body,
                Background = Ui.CardFill, BorderThickness = new Thickness(1), BorderBrush = Ui.CardStroke,
            });
        }
    }

    // ---- the state card: what the machine page says in place of a picture ------------------------

    /// <summary>The buttons on the state card, for the self-test.</summary>
    public IReadOnlyList<CardActionKind> CardKindsForTest => CardHost.Children.OfType<StackPanel>().SelectMany(p => p.Children.OfType<Button>()).Select(b => (CardActionKind)b.Tag).ToList();
    public string CardTitleForTest => CardHost.Children.OfType<TextBlock>().FirstOrDefault()?.Text ?? "";
    public string CardBodyForTest => string.Join(" ", CardHost.Children.OfType<TextBlock>().Skip(1).Select(t => t.Text));
    /// <summary>The detail behind Show details, once the card has it.</summary>
    public string? CardDetailsForTest => CardHost.Children.OfType<Expander>().FirstOrDefault() is { Content: StackPanel p } ? p.Children.OfType<TextBlock>().FirstOrDefault()?.Text : null;
    public bool CardShowsForTest => CardHost.Visibility == Visibility.Visible && CardHost.Children.Count > 0;
    string _cardSig = "";

    void FillCard(string key, string name, CardInfo? card, bool busy)
    {
        var failed = _upgradeFailed.GetValueOrDefault(key);
        var sig = key + "|" + busy + "|" + failed?.Message + "|" + (card is null ? "" : card.Title + card.Body + card.Details + string.Join(",", card.Actions.Select(a => a.Kind + a.Label)));
        if (sig == _cardSig && CardHost.Children.Count > 0) return; // a refresh must not rebuild a card the person has open
        _cardSig = sig;
        CardHost.Children.Clear();
        CardHost.Visibility = card is null ? Visibility.Collapsed : Visibility.Visible;
        if (card is null) return;
        var caption = (Style)Application.Current.Resources["BodyTextBlockStyle"];
        if (card.Title.Length > 0)
            CardHost.Children.Add(new TextBlock { Text = card.Title, Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"], TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap, HorizontalAlignment = HorizontalAlignment.Center });
        if (card.Body.Length > 0)
            CardHost.Children.Add(new TextBlock { Text = card.Body, Style = caption, Foreground = Ui.Secondary, TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap, MaxWidth = 440, HorizontalAlignment = HorizontalAlignment.Center });
        if (failed is not null)
        {
            var bar = new InfoBar { IsOpen = true, IsClosable = false, Severity = InfoBarSeverity.Error, Message = failed.Message, MaxWidth = 440 };
            AutomationProperties.SetAutomationId(bar, "upgrade.error");
            if (failed.Fix == ErrorFix.ChooseFirmware && _core.FixLabel(failed) is { } fix)
            {
                var pick = new Button { Content = fix };
                pick.Click += (_, _) => _ = ChooseFirmwareAsync(key);
                bar.ActionButton = pick;
            }
            CardHost.Children.Add(bar);
        }
        if (card.Details is { Length: > 0 } details)
        {
            var text = new TextBlock { Text = details, FontFamily = new FontFamily("Cascadia Mono, Consolas"), IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap };
            var copy = new Button { Content = Ui.T(OrigamiUiString.Copy) };
            copy.Click += (_, _) =>
            {
                var pkg = new Windows.ApplicationModel.DataTransfer.DataPackage();
                pkg.SetText(details);
                Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
            };
            var expander = new Expander { Header = Ui.T(OrigamiUiString.ShowDetails), MaxWidth = 440, HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Content = new StackPanel { Spacing = 8, Children = { text, copy } } };
            AutomationProperties.SetAutomationId(expander, "card.details");
            CardHost.Children.Add(expander);
        }
        if (card.Actions.Count > 0 || busy)
        {
            var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, HorizontalAlignment = HorizontalAlignment.Center };
            for (var i = 0; i < card.Actions.Count; i++)
            {
                var a = card.Actions[i];
                var b = new Button { Content = a.Label, Tag = a.Kind, IsEnabled = !busy };
                if (i == 0) b.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
                AutomationProperties.SetAutomationId(b, "card." + a.Kind);
                b.Click += (_, _) => OnCardAction(key, a.Kind);
                row.Children.Add(b);
            }
            if (busy) row.Children.Add(new ProgressRing { Width = 20, Height = 20, IsActive = true });
            CardHost.Children.Add(row);
        }
    }

    /// <summary>A button on the state card does what the same command would.</summary>
    public void OnCardAction(string key, CardActionKind kind)
    {
        switch (kind)
        {
            case CardActionKind.OpenConsole: Invoke("window.console"); break;
            case CardActionKind.PowerOff: Invoke("machine.stop"); break;
            case CardActionKind.Reconnect: case CardActionKind.TryAgain: Invoke("machine.start"); break;
            case CardActionKind.Resume: Invoke("machine.resume"); break;
            case CardActionKind.Upgrade: RunUpgrade(); break;
            case CardActionKind.ShowInFolder: ShowMachineInExplorer(key); break;
            case CardActionKind.OpenSettings: Invoke("machine.settings"); break;
            case CardActionKind.ChooseFirmware: _ = ChooseFirmwareAsync(key); break;
            case CardActionKind.ChooseDisk: _ = RunAsync(() => MachineSettingsAsync(key, "Drives")); break;
        }
    }

    /// <summary>Presses a button on the state card, through its automation pattern, for the self-test.</summary>
    public void PressCardForTest(CardActionKind kind)
    {
        var b = CardHost.Children.OfType<StackPanel>().SelectMany(p => p.Children.OfType<Button>()).First(x => (CardActionKind)x.Tag == kind);
        new Microsoft.UI.Xaml.Automation.Peers.ButtonAutomationPeer(b).Invoke();
    }

    // ---- the command bar and the shortcuts ---------------------------------------------------

    /// <summary>Commands only this app has; the core's table has no view or preferences commands.</summary>
    static readonly CommandInfo[] AppOnly =
    {
        new("machine.pointer-mode", "Toggle Follow / Capture", "Ctrl+Shift+M"),
        new("view.library", "Show library", "F9"),
        new("app.preferences", "Preferences", "Ctrl+,"),
        new("app.about", "About Origami", null),
        new("view.fit", "Fit to window", "Ctrl+9"),
        new("view.scale1x", "Scale 1x", "Ctrl+Alt+1"),
        new("view.scale2x", "Scale 2x", "Ctrl+Alt+2"),
        new("view.actual-size", "Actual size", "Ctrl+0"),
    };

    readonly List<CommandInfo> _commands = new();
    readonly Dictionary<string, string?> _shortcuts = new();
    MenuFlyoutItem _pauseItem = null!;

    /// <summary>The label of the Pause entry in the Power off flyout, which reads Resume while the machine is paused.</summary>
    public string PauseLabelForTest => _pauseItem.Text;

    /// <summary>Words and clicks for the command bar: titles and tooltips come from the core's table, so no app writes its own.</summary>
    void BuildToolbar()
    {
        _commands.AddRange(_core.Commands());
        _commands.AddRange(AppOnly);
        string Title(string id) => _commands.First(c => c.Id == id).Title;
        void Tip(DependencyObject o, string id) => ToolTipService.SetToolTip(o, Ui.Tooltip(id));

        StartButton.Label = Title("machine.start"); StartButton.Click += (_, _) => Invoke(_cancelMode ? "machine.cancel-start" : "machine.start"); Tip(StartButton, "machine.start");
        PowerLabel.Text = Title("machine.stop"); PowerSplit.Click += (_, _) => Invoke("machine.stop"); Tip(PowerSplit, "machine.stop");
        _pauseItem = new MenuFlyoutItem { Text = Title("machine.pause") };
        AutomationProperties.SetAutomationId(_pauseItem, "cmd.pause");
        _pauseItem.Click += (_, _) => Invoke("machine.pause");
        var reset = new MenuFlyoutItem { Text = Title("machine.reset") };
        AutomationProperties.SetAutomationId(reset, "cmd.reset");
        reset.Click += (_, _) => Invoke("machine.reset");
        var force = new MenuFlyoutItem { Text = Title("machine.force-quit") };
        AutomationProperties.SetAutomationId(force, "cmd.forcequit");
        force.Click += (_, _) => Invoke("machine.force-quit");
        Tip(_pauseItem, "machine.pause"); Tip(reset, "machine.reset"); Tip(force, "machine.force-quit");
        PowerMenu.Items.Add(_pauseItem); PowerMenu.Items.Add(reset); PowerMenu.Items.Add(force);

        ConsoleButton.Label = Title("window.console"); ConsoleButton.Click += (_, _) => Invoke("window.console"); Tip(ConsoleButton, "window.console");
        PointerButton.Label = Title("machine.capture-mouse"); PointerButton.Click += (_, _) => Invoke("machine.capture-mouse"); Tip(PointerButton, "machine.capture-mouse");
        SettingsButton.Label = Title("machine.settings"); SettingsButton.Click += (_, _) => Invoke("machine.settings"); Tip(SettingsButton, "machine.settings");
        CpuButton.Label = Title("view.cpu-load");
        Tip(CpuButton, "view.cpu-load");
        CpuButton.Click += (_, _) => SetCpuPanel(CpuButton.IsChecked == true);
        DiagnosticsButton.Label = Title("window.diagnostics"); DiagnosticsButton.Click += (_, _) => Invoke("window.diagnostics"); Tip(DiagnosticsButton, "window.diagnostics");
        ScreenshotButton.Label = Title("machine.screenshot"); ScreenshotButton.Click += (_, _) => Invoke("machine.screenshot"); Tip(ScreenshotButton, "machine.screenshot");
        InstallButton.Label = Title("machine.install-irix"); InstallButton.Click += (_, _) => Invoke("machine.install-irix"); Tip(InstallButton, "machine.install-irix");
        ShortcutsButton.Label = Title("view.shortcuts"); ShortcutsButton.Click += (_, _) => Invoke("view.shortcuts"); Tip(ShortcutsButton, "view.shortcuts");
        PreferencesButton.Label = Ui.T(OrigamiUiString.Preferences); PreferencesButton.Click += (_, _) => Invoke("app.preferences");
        ToolTipService.SetToolTip(PreferencesButton, Ui.T(OrigamiUiString.Preferences));
        AboutButton.Label = Ui.T(OrigamiUiString.AboutOrigami); AboutButton.Click += (_, _) => Invoke("app.about");
        ToolTipService.SetToolTip(AboutButton, Ui.T(OrigamiUiString.AboutOrigami));
        _toastTimer = _ui.CreateTimer();
        _toastTimer.IsRepeating = false;
        _toastTimer.Tick += (_, _) => Toast.Visibility = Visibility.Collapsed;
    }

    DispatcherQueueTimer _toastTimer = null!;

    /// <summary>Accelerators live on the window, not on a control: a disabled control's accelerator never fires, and Pause and Resume
    /// share one (Invoke turns Pause into Resume while paused). A bare key such as Delete belongs to the library instead.</summary>
    void BuildAccelerators()
    {
        var taken = new HashSet<(bool, bool, bool, string)>();
        foreach (var c in _commands)
        {
            _shortcuts[c.Id] = c.WindowsAccelerator;
            if (!Accelerator.TryParse(c.WindowsAccelerator, out var a) || a.VirtualKey == 0 || a.IsBare || (a.Alt && a.Key == "F4")) continue;
            if (!taken.Add((a.Ctrl, a.Shift, a.Alt, a.Key))) continue;
            var mods = (a.Ctrl ? Windows.System.VirtualKeyModifiers.Control : 0)
                     | (a.Shift ? Windows.System.VirtualKeyModifiers.Shift : 0)
                     | (a.Alt ? Windows.System.VirtualKeyModifiers.Menu : 0);
            var acc = new KeyboardAccelerator { Key = (Windows.System.VirtualKey)a.VirtualKey, Modifiers = mods };
            var cmd = c.Id;
            acc.Invoked += (_, e) => { Invoke(cmd); e.Handled = true; };
            Root.KeyboardAccelerators.Add(acc);
        }
    }

    // ---- commands ---------------------------------------------------------------------------

    /// <summary>Runs a command by table ID. The toolbar, shortcuts and tests all come here. The commands that end something ask the core's
    /// question first; <paramref name="confirmed"/> is for a caller that has already asked.</summary>
    public void Invoke(string id, bool confirmed = false)
    {
        var key = SelectedKey;
        MachineState? state = null;
        if (key != null) state = _core.Status(key).State;
        // The core gives Pause and Resume one shortcut and enables only one of them: pressing it toggles.
        if (id == "machine.pause" && state == MachineState.Paused) id = "machine.resume";
        // Diagnostics needs a machine; the shared gate would drop the command silently, so say why first.
        if (id == "window.diagnostics" && key == null) { Say(Ui.T(OrigamiUiString.SelectAMachineFirst)); return; }
        if (!CommandOn(id, state, key)) return;
        switch (id)
        {
            case "machine.start" when key != null: _ = StartAsync(key); break;
            case "machine.cancel-start" when key != null: _starts.GetValueOrDefault(key)?.Cancel(); break;
            case "machine.stop" when key != null: _ = RunConfirmedAsync(new OrigamiAlertKind.PowerOff(NameOf(key)), confirmed, () => _core.PowerOffAsync(key)); break;
            case "machine.force-quit" when key != null: _ = RunConfirmedAsync(new OrigamiAlertKind.ForceQuit(NameOf(key)), confirmed, () => _core.ForceQuitAsync(key)); break;
            case "machine.reset" when key != null: _ = RunConfirmedAsync(new OrigamiAlertKind.Reset(NameOf(key)), confirmed, () => _core.ControlAsync(key, MachineControl.Reset)); break;
            case "machine.pause" when key != null: _ = RunAsync(() => _core.ControlAsync(key, MachineControl.Pause)); break;
            case "machine.resume" when key != null: _ = RunAsync(() => _core.ControlAsync(key, MachineControl.Resume)); break;
            case "machine.release-keys" when key != null: Display.LoseFocus(); break;
            case "machine.screenshot" when key != null: _ = ScreenshotAsync(key); break;
            case "machine.settings" when key != null && !_dialogOpen: _ = RunAsync(() => MachineSettingsAsync(key)); break;
            case "machine.install-irix" when key != null: OpenInstall(key); break;
            case "machine.remove" when key != null: _ = RemoveAsync(key, confirmed); break;
            case "machine.eject-cd" when key != null: _ = RunAsync(() => EjectAsync(key)); break;
            case "app.preferences": _prefsOpen = true; Settings.Load(_settings); Refresh(); break; // load once: a refresh must not reset unsaved edits
            case "machine.capture-mouse" or "machine.pointer-mode" when key != null:
                {
                    var next = Display.Mode == PointerMode.Follow ? PointerMode.Capture : PointerMode.Follow;
                    _settings.PointerModeByMachine[key] = next;
                    _store.Save(_settings);
                    Display.Mode = next;
                    break;
                }
            case "view.library": SetPane(!_paneOpen); break;
            case "view.shortcuts" when !_dialogOpen: _ = RunAsync(ShortcutsAsync); break;
            case "window.console" when key != null: if (_paneKey == key) _pane?.FocusTerminal(); else OpenConsole(key); break;
            case "machine.new" when !_dialogOpen: _ = RunAsync(NewMachineAsync); break;
            case "window.diagnostics" when key != null: OpenDiagnostics(key); break;
            case "view.fit": LastViewCommand = id; Say(OrigamiFfiMethods.OrigamiToast(new OrigamiToastKind.PictureSize("Fit"), OrigamiPlatform.Windows).Text); break; // it always does
            case "view.scale1x": LastViewCommand = id; SizeWindowForPicture(1, "scale 1x"); break;
            case "view.scale2x": LastViewCommand = id; SizeWindowForPicture(2, "scale 2x"); break;
            case "view.actual-size": LastViewCommand = id; SizeWindowForPicture(1 / Root.XamlRoot.RasterizationScale, "actual size"); break;
            case "app.about": OpenAbout(); break;
            case "app.quit": Win.RequestClose(Hwnd); break;
        }
    }

    /// <summary>Opens the machine's Settings on a page, for the self-test.</summary>
    public void OpenSettingsForTest(string? page) { if (SelectedKey is { } k && !_dialogOpen) _ = RunAsync(() => MachineSettingsAsync(k, page)); }
    /// <summary>Rounded text containers in the library: the capsules the new rows must not have.</summary>
    public int RowCapsulesForTest => Descend(Library).OfType<Border>().Count(b => b.CornerRadius.TopLeft >= 8 && b.Child is TextBlock);
    public bool PictureWindowVisibleForTest => Display.WindowVisible;

    public PointerMode PointerModeFor(string key) => _settings.ModeFor(key);

    public void SetPointerMode(string key, PointerMode mode)
    {
        _settings.PointerModeByMachine[key] = mode;
        _store.Save(_settings);
        if (key == SelectedKey) Display.Mode = mode;
    }

    string NameOf(string key) => _core.Machines().FirstOrDefault(m => m.Key == key)?.Name ?? "";

    /// <summary>The core's gate, and Install IRIX only on a machine that runs IRIX.</summary>
    bool CommandOn(string id, MachineState? state, string? key) =>
        _core.CommandEnabled(id, state) && (id != "machine.install-irix" || _core.Machines().FirstOrDefault(m => m.Key == key)?.RunsIrix == true);

    async Task RunAsync(Func<Task> f)
    {
        try { await f(); }
        catch (Exception e) { Say(e.Message, severity: InfoBarSeverity.Error); }
    }

    /// <summary>A start that failed leaves the core's reason on the machine's card; it needs no second telling.</summary>
    async Task StartAsync(string key)
    {
        var cts = _starts[key] = new CancellationTokenSource();
        try { await _core.StartAsync(key, cts.Token); }
        catch (Exception e) { App.Log("start ended: " + e.Message); }
        _starts.Remove(key);
        Refresh();
    }

    bool _cancelMode;
    readonly Dictionary<string, CancellationTokenSource> _starts = new();

    /// <summary>Power off, Reset and Force quit lose the machine's unsaved work, so the core's question comes first, with Cancel the default.</summary>
    async Task RunConfirmedAsync(OrigamiAlertKind alert, bool confirmed, Func<Task> act)
    {
        if (!confirmed && (await AskAsync(alert))?.Role != ButtonRole.Destructive) return;
        await RunAsync(act);
    }

    // ---- dialogs: one at a time, above the screen -----------------------------------------------

    bool _dialogOpen; // one ContentDialog at a time
    readonly SemaphoreSlim _dialogs = new(1, 1);
    public Microsoft.UI.Xaml.XamlRoot XamlRootForTest => Root.XamlRoot;
    /// <summary>The dialog on show, for the self-test.</summary>
    public ContentDialog? OpenDialogForTest { get; private set; }

    /// <summary>Shows a dialog after any other has closed. The picture is a native child window that draws over all XAML, so it is hidden for as long as a dialog is up.</summary>
    public async Task<ContentDialogResult> ShowDialogAsync(ContentDialog dlg)
    {
        await _dialogs.WaitAsync();
        _dialogOpen = true;
        OpenDialogForTest = dlg;
        var hid = Display.Attached;
        if (hid) Display.Hide();
        try { return await dlg.ShowAsync(); }
        finally
        {
            _dialogOpen = false; OpenDialogForTest = null;
            if (hid) Display.Show();
            _dialogs.Release();
        }
    }

    /// <summary>The core's alert in a ContentDialog: its safe answer is the one Return picks, a destructive one is styled so, and Cancel is Escape.</summary>
    internal async Task<AlertButton?> AskAsync(OrigamiAlertKind kind)
    {
        var (alert, buttons) = Ui.Alert(kind);
        var layout = DialogLayout.Of(buttons);
        var dlg = new ContentDialog
        {
            XamlRoot = Root.XamlRoot, Title = alert.Title, CloseButtonText = layout.Close,
            Content = new TextBlock { Text = alert.Body, TextWrapping = TextWrapping.Wrap },
            DefaultButton = layout.Default switch { DialogLayout.PrimarySlot => ContentDialogButton.Primary, DialogLayout.SecondarySlot => ContentDialogButton.Secondary, _ => ContentDialogButton.Close },
        };
        if (layout.Primary is { } p) dlg.PrimaryButtonText = p;
        if (layout.Secondary is { } s) dlg.SecondaryButtonText = s;
        if (Ui.Destructive is { } bad)
        {
            if (layout.Destructive == DialogLayout.PrimarySlot) dlg.PrimaryButtonStyle = bad;
            else if (layout.Destructive == DialogLayout.SecondarySlot) dlg.SecondaryButtonStyle = bad;
        }
        var result = await ShowDialogAsync(dlg);
        var slot = result switch { ContentDialogResult.Primary => DialogLayout.PrimarySlot, ContentDialogResult.Secondary => DialogLayout.SecondarySlot, _ => DialogLayout.CloseSlot };
        return layout.Chosen(buttons, slot);
    }

    async Task MachineSettingsAsync(string key, string? page = null)
    {
        PathPicker.Hwnd = Hwnd;
        try { await MachineSettingsDialog.ShowAsync(this, _core, key, NameOf(key), page); }
        finally { Refresh(); }
    }

    async Task NewMachineAsync()
    {
        PathPicker.Hwnd = Hwnd;
        var made = await NewMachineDialog.ShowAsync(this, _core, _newMachinePreset);
        _newMachinePreset = null;
        if (made is not { } m) return;
        _prefsOpen = false;
        Reload();
        Library.SelectedItem = Library.Items.Cast<ListViewItem>().FirstOrDefault(i => (string?)i.Tag == m.Key) ?? Library.SelectedItem;
        if (m.Install) OpenInstall(m.Key);
    }

    /// <summary>A disk image dropped on the library: New machine opens with it chosen.</summary>
    string? _newMachinePreset;

    async Task ShortcutsAsync()
    {
        var body = new StackPanel { Spacing = 16, Width = 440 };
        foreach (var g in OrigamiFfiMethods.OrigamiShortcutGroups(OrigamiPlatform.Windows))
        {
            body.Children.Add(new TextBlock { Text = g.Title, Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"] });
            var grid = new Grid { RowSpacing = 6, ColumnDefinitions = { new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }, new ColumnDefinition { Width = GridLength.Auto } } };
            var row = 0;
            foreach (var i in g.Items)
            {
                grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                var label = new TextBlock { Text = i.Label, TextWrapping = TextWrapping.Wrap };
                var keys = new TextBlock { Text = i.Keys, Foreground = Ui.Secondary, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(16, 0, 0, 0) };
                Grid.SetRow(label, row); Grid.SetRow(keys, row); Grid.SetColumn(keys, 1);
                grid.Children.Add(label); grid.Children.Add(keys);
                row++;
            }
            body.Children.Add(grid);
        }
        var dlg = new ContentDialog
        {
            XamlRoot = Root.XamlRoot, Title = Ui.T(OrigamiUiString.KeyboardShortcuts), CloseButtonText = Ui.T(OrigamiUiString.Done),
            DefaultButton = ContentDialogButton.Close,
            Content = new ScrollViewer { Content = body, MaxHeight = 480 },
        };
        await ShowDialogAsync(dlg);
    }

    // ---- drag and drop -------------------------------------------------------------------------

    /// <summary>The kind of the first file in a drag, by the core's rules (extension and header).</summary>
    static async Task<(string Path, OrigamiDropKind Kind)?> DroppedAsync(Microsoft.UI.Xaml.DragEventArgs e)
    {
        if (!e.DataView.Contains(Windows.ApplicationModel.DataTransfer.StandardDataFormats.StorageItems)) return null;
        var items = await e.DataView.GetStorageItemsAsync();
        return items.FirstOrDefault(i => i.IsOfType(Windows.Storage.StorageItemTypes.File)) is { } f ? (f.Path, OrigamiFfiMethods.OrigamiClassifyDrop(f.Path)) : null;
    }

    /// <summary>A CD image over the page changes the CD, a disk image over the library starts a new machine from it; anything else is refused.</summary>
    async Task OverAsync(Microsoft.UI.Xaml.DragEventArgs e, OrigamiDropKind want)
    {
        var deferral = e.GetDeferral();
        try
        {
            var dropped = await DroppedAsync(e);
            var take = dropped is { } d && d.Kind == want && (want != OrigamiDropKind.CdImage || SelectedKey is not null);
            e.AcceptedOperation = take ? Windows.ApplicationModel.DataTransfer.DataPackageOperation.Copy : Windows.ApplicationModel.DataTransfer.DataPackageOperation.None;
            e.DragUIOverride.IsGlyphVisible = false;
            if (take)
            {
                var caption = OrigamiFfiMethods.OrigamiDropCaption(want);
                e.DragUIOverride.Caption = caption;
                DropText.Text = caption ?? "";
                DropHint.Visibility = Visibility.Visible;
            }
            else DropHint.Visibility = Visibility.Collapsed;
        }
        finally { deferral.Complete(); }
    }

    async Task DropAsync(Microsoft.UI.Xaml.DragEventArgs e, OrigamiDropKind want)
    {
        var deferral = e.GetDeferral();
        DropHint.Visibility = Visibility.Collapsed;
        try
        {
            if (await DroppedAsync(e) is not { } d || d.Kind != want) return;
            await DropFileAsync(d.Path, want);
        }
        finally { deferral.Complete(); }
    }

    /// <summary>What a dropped file does; the self-test calls it with a path, as a drag from Explorer cannot be scripted.</summary>
    internal async Task DropFileAsync(string path, OrigamiDropKind kind)
    {
        if (kind == OrigamiDropKind.DiskImage)
        {
            while (_dialogOpen) await Task.Delay(50); // a dialog still closing would swallow the command
            _newMachinePreset = path;
            Invoke("machine.new");
            return;
        }
        if (SelectedKey is not { } key) return;
        try
        {
            var s = await _core.SettingsAsync(key);
            if (s.Drives.FirstOrDefault(d => d.Kind == "cdrom") is { } cd) await _core.ChangeMediaAsync(key, cd.Name, path);
            else if (s.CdTarget is { } target) await _core.ApplyChangeAsync(key, new MachineChange.AttachCd(path, target));
        }
        catch (Exception ex) { Say(_core.ProblemOf(ex).Message, severity: InfoBarSeverity.Error); }
    }

    // ---- remove, eject --------------------------------------------------------------------------

    /// <summary>Delete…: the core's question, then the folder goes to the Recycle Bin and a toast says so. A running machine is refused by the core.</summary>
    public async Task RemoveAsync(string key, bool confirmed = false)
    {
        var name = NameOf(key);
        try
        {
            if (!confirmed)
            {
                if (!CommandOn("machine.remove", _core.Status(key).State, key)) { Say(name + " is running. Power off to remove it.", severity: InfoBarSeverity.Warning); return; }
                var left = await _core.RemovePlanAsync(key);
                if ((await AskAsync(new OrigamiAlertKind.Remove(name, left.ToArray())))?.Role != ButtonRole.Destructive) return;
            }
            await _core.RemoveAsync(key);
        }
        catch (Exception e) { Say(e.Message, severity: InfoBarSeverity.Error); return; }
        Say(OrigamiFfiMethods.OrigamiToast(new OrigamiToastKind.Removed(name), OrigamiPlatform.Windows).Text);
        Reload();
    }

    async Task EjectAsync(string key)
    {
        var cd = (await _core.SettingsAsync(key)).Drives.FirstOrDefault(d => d.Kind == "cdrom");
        if (cd is null) return;
        await _core.ChangeMediaAsync(key, cd.Name, null);
        Say(OrigamiFfiMethods.OrigamiToast(new OrigamiToastKind.DiscEjected(), OrigamiPlatform.Windows).Text);
    }

    // ---- toasts ---------------------------------------------------------------------------------

    string _lastSaid = "";

    /// <summary>A toast for a few seconds, in the status bar's row: the picture is a native window that XAML cannot draw over, and a row of its own would resize it.</summary>
    void Say(string text, string? action = null, Action? onAction = null, InfoBarSeverity severity = InfoBarSeverity.Informational)
    {
        _lastSaid = text;
        ToastText.Text = text;
        if (severity == InfoBarSeverity.Informational) ToastText.ClearValue(TextBlock.ForegroundProperty);
        else ToastText.Foreground = Ui.ToneBrush(severity == InfoBarSeverity.Error ? Tone.Critical : Tone.Caution);
        ToastActions.Children.Clear();
        if (action is not null && onAction is not null)
        {
            var b = new Button { Content = action, MinHeight = 0, Height = 22, Padding = new Thickness(8, 0, 8, 0), FontSize = 12 };
            AutomationProperties.SetAutomationId(b, "toast.action");
            b.Click += (_, _) => onAction();
            ToastActions.Children.Add(b);
        }
        Toast.Visibility = Visibility.Visible;
        _toastTimer.Stop();
        _toastTimer.Interval = TimeSpan.FromMilliseconds(Tokens.Motion.ToastMs);
        _toastTimer.Start();
    }

    /// <summary>Whether the toast offers an action such as Show in Explorer, for the self-test.</summary>
    public string? ToastActionForTest => ToastOpenForTest && ToastActions.Children.FirstOrDefault() is Button b ? b.Content as string : null;
    public bool ToastOpenForTest => Toast.Visibility == Visibility.Visible;
    /// <summary>Lay the window out now, then say whether the layout matches the window's size.</summary>
    public bool LayoutCurrentForTest { get { Root.UpdateLayout(); return LayoutMatchesWindow; } }
    bool LayoutMatchesWindow => Root.XamlRoot is { } x && Math.Abs(Root.ActualWidth - x.Size.Width) < 1 && Math.Abs(Root.ActualHeight - x.Size.Height) < 1;

    // ---- Install IRIX ------------------------------------------------------------------------

    readonly Dictionary<string, InstallWindow> _installs = new();
    public InstallWindow? InstallForTest(string key) => _installs.GetValueOrDefault(key);

    /// <summary>One assistant per machine: asking again brings the open one forward.</summary>
    void OpenInstall(string key)
    {
        if (!_installs.TryGetValue(key, out var w))
        {
            var name = _core.Machines().First(m => m.Key == key).Name;
            w = new InstallWindow(this, (UniffiCoreClient)_core, key, name, () => { SelectForTabs(key); OpenInNewTab(key); Activate(); }, () => ShowConsole(key), TellAsync);
            w.Closed += (_, _) => { _installs.Remove(key); _core.SetInstalling(key, false); RefreshGuide(); };
            w.GuideChanged += () => { _core.SetInstalling(key, w.GuideSteps is not null && !w.Closing); RefreshGuide(); };
            _installs[key] = w;
        }
        if (w.GuideSteps is null) w.Activate(); else ShowGuide(true);
    }

    void ShowConsole(string key)
    {
        SelectForTabs(key);
        if (_paneKey == key) _pane?.FocusTerminal(); else OpenConsole(key);
    }

    /// <summary>Whether an install is under way on this assistant's machine; the one place quit and tab close ask.</summary>
    static bool InstallRunning(InstallWindow w) => w.Serving;

    /// <summary>The core's prompt for quitting or closing a tab while an install runs; true to stop it and the machine. Return keeps installing.</summary>
    async Task<bool> ConfirmStopInstallsAsync(bool tab = false)
    {
        var t = uniffi.origami_ffi.OrigamiFfiMethods.OrigamiInstallTexts();
        var ask = new ContentDialog
        {
            XamlRoot = Root.XamlRoot, Title = t.QuitHeading, Content = tab ? t.TabText : t.QuitText,
            PrimaryButtonText = tab ? t.StopAndClose : t.StopAndQuit, CloseButtonText = t.Keep, DefaultButton = ContentDialogButton.Close,
        };
        if (Ui.Destructive is { } bad) ask.PrimaryButtonStyle = bad;
        return await ShowDialogAsync(ask) == ContentDialogResult.Primary;
    }

    /// <summary>A note after the assistant has closed; it waits for any other dialog on this window.</summary>
    async Task TellAsync(string text) =>
        await ShowDialogAsync(new ContentDialog { XamlRoot = Root.XamlRoot, Title = Ui.T(OrigamiUiString.InstallIrix), Content = text, CloseButtonText = Ui.T(OrigamiUiString.Done) });

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
    /// <summary>Machines whose current run has had its first fit; a key leaves when its run ends.</summary>
    readonly HashSet<string> _sized = new();
    public const double MinWidthDips = 900, MinHeightDips = 600;

    PixelRect WindowRect => new(AppWindow.Position.X, AppWindow.Position.Y, AppWindow.Size.Width, AppWindow.Size.Height);
    public PixelRect WorkArea
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

    /// <summary>Once per machine run, when its first picture arrives: grow the window to show the picture at 1 DIP per guest pixel, or the
    /// largest that fits the work area. A maximised, minimised or full-screen window is left alone.</summary>
    void SizeForPicture(string key)
    {
        if (Display.HeadWidth <= 0 || !_sized.Add(key)) return;
        if (AppWindow.Presenter is not Microsoft.UI.Windowing.OverlappedPresenter { State: Microsoft.UI.Windowing.OverlappedPresenterState.Restored }) return;
        var (pw, ph) = Display.PaneSize;
        var s = Root.XamlRoot.RasterizationScale;
        var d = ViewScale.FirstScale(WindowRect, WorkArea, s, pw, ph, Display.HeadWidth, Display.HeadHeight);
        var want = ViewScale.WindowFor(WindowRect, WorkArea, s, pw, ph, Display.HeadWidth, Display.HeadHeight, MinWidthDips, MinHeightDips, d);
        if (ViewScale.Grown(WindowRect, WorkArea, want) is { } r) { MoveResize(r); Root.UpdateLayout(); }
    }

    /// <summary>View menu: resize the window once so the picture is <paramref name="dipsPerGuest"/> DIPs per guest pixel; free resizing fills after.</summary>
    void SizeWindowForPicture(double dipsPerGuest, string name)
    {
        Say(OrigamiFfiMethods.OrigamiToast(new OrigamiToastKind.PictureSize(name), OrigamiPlatform.Windows).Text);
        if (Display.HeadWidth <= 0 || AppWindow.Presenter is not Microsoft.UI.Windowing.OverlappedPresenter { State: Microsoft.UI.Windowing.OverlappedPresenterState.Restored }) return;
        var (pw, ph) = Display.PaneSize;
        MoveResize(ViewScale.WindowFor(WindowRect, WorkArea, Root.XamlRoot.RasterizationScale, pw, ph, Display.HeadWidth, Display.HeadHeight, MinWidthDips, MinHeightDips, dipsPerGuest));
        Root.UpdateLayout();
    }

    /// <summary>Self-test: the window lies in its monitor's work area.</summary>
    public bool InsideWorkArea
    {
        get { var (r, wa) = (WindowRect, WorkArea); return r.X >= wa.X - 2 && r.Y >= wa.Y - 2 && r.X + r.W <= wa.X + wa.W + 2 && r.Y + r.H <= wa.Y + wa.H + 2; }
    }

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
    public bool ShowInExplorerVisible => ToastActionForTest is not null;

    async Task ScreenshotAsync(string key)
    {
        try
        {
            var name = _core.Machines().FirstOrDefault(x => x.Key == key)?.Name ?? "Origami";
            _lastShot = await Screenshots.SaveAsync(_core, key, name);
            var toast = OrigamiFfiMethods.OrigamiToast(new OrigamiToastKind.ScreenshotSaved(), OrigamiPlatform.Windows);
            Say(toast.Text, toast.Action, ShowInExplorer);
        }
        catch (Exception e) { Say("Screenshot failed: " + e.Message, severity: InfoBarSeverity.Error); }
    }

    void ShowInExplorer()
    {
        if (_lastShot is { } p && File.Exists(p))
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("explorer.exe", $"/select,\"{p}\"") { UseShellExecute = true });
    }

    // ---- the install guide in the pane -----------------------------------------------------------

    bool _guideShown, _guideWanted = true;
    string _guideSig = "";
    readonly HashSet<string> _guideDone = new();
    InstallWindow? GuideOwner => SelectedKey is { } k && _installs.GetValueOrDefault(k) is { GuideSteps: not null, Closing: false } w ? w : null;
    public bool GuideShownForTest => _guideShown;
    public string GuideHeaderForTest => GuideHost.Children.OfType<TextBlock>().FirstOrDefault()?.Text ?? "";
    public int GuideStepsForTest => GuideHost.Children.OfType<FrameworkElement>().Count(e => e.Tag is int);
    public IReadOnlyList<string> GuideTypedForTest => GuideHost.Children.OfType<FrameworkElement>().SelectMany(Descend).OfType<TextBlock>()
        .Where(t => t.FontFamily.Source.StartsWith("Cascadia")).Select(t => t.Text).ToList();
    static IEnumerable<DependencyObject> Descend(DependencyObject o)
    {
        yield return o;
        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(o); i++)
            foreach (var c in Descend(VisualTreeHelper.GetChild(o, i))) yield return c;
    }
    public string GuideFooterForTest => GuideHost.Children.OfType<FrameworkElement>().SelectMany(Descend).OfType<TextBlock>().FirstOrDefault(t => AutomationProperties.GetAutomationId(t) == "install.footer.server")?.Text ?? "";
    public string GuideFailureForTest => GuideHost.Children.OfType<FrameworkElement>().SelectMany(Descend).OfType<TextBlock>().FirstOrDefault(t => t.Foreground is SolidColorBrush b && b == Ui.ToneBrush(Tone.Critical))?.Text ?? "";
    public Button? GuideButtonForTest(string id) => GuideHost.Children.OfType<FrameworkElement>().SelectMany(Descend).OfType<Button>().FirstOrDefault(b => AutomationProperties.GetAutomationId(b) == id);

    /// <summary>The library or the guide in the pane. The guide is on offer while the selected machine's install runs.</summary>
    public void ShowGuide(bool on)
    {
        _guideWanted = on;
        if (on && !_paneOpen) SetPane(true);
        RefreshGuide();
    }

    /// <summary>Puts the pane in step with the install: the guide where there is one to show, else the library.</summary>
    void RefreshGuide()
    {
        var w = GuideOwner;
        var show = w is not null && _guideWanted;
        if (_guideShown != show) { _guideShown = show; _guideSig = ""; }
        Library.Visibility = show ? Visibility.Collapsed : Visibility.Visible;
        GuideScroll.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        GuideBack.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        GuideShow.Visibility = w is not null && !show ? Visibility.Visible : Visibility.Collapsed;
        LibraryHeading.Text = show ? Ui.T(OrigamiUiString.InstallIrix) : Labels.SidebarHeading;
        NewMachineButton.Visibility = show ? Visibility.Collapsed : Visibility.Visible;
        if (!show || w is null) return;
        var steps = w.GuideSteps!;
        var seen = w.GuideSeen();
        var done = w.Serial ? seen : steps.Select((s, i) => (s, i)).TakeWhile(t => _guideDone.Contains(t.i.ToString())).Count();
        var sig = $"{steps.Length}|{done}|{w.Serial}|{w.Automatic}|{w.GuideFooter}|{w.CanInstall}|{w.CanFixed}|{w.Applying}|{w.ApplyLine}|{w.ApplyFailure}|{w.ErrorText}|{w.CanFinish}|{w.MachineStopped}|{_core.Status(SelectedKey!).State}";
        if (sig == _guideSig) return;
        _guideSig = sig;
        BuildGuide(w, steps, done);
    }

    void BuildGuide(InstallWindow w, OrigamiGuideStep[] steps, int done)
    {
        var running = _core.Status(SelectedKey!).State == MachineState.Running;
        var mono = new FontFamily("Cascadia Mono, Consolas");
        var caption = (Style)Application.Current.Resources["CaptionTextBlockStyle"];
        GuideHost.Children.Clear();
        GuideHost.Children.Add(new TextBlock { Text = OrigamiFfiMethods.OrigamiGuideHeader((uint)Math.Min(done + 1, steps.Length), (uint)steps.Length), Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"] });
        if (w.Automatic)
        {
            // Origami types the package commands; the steps still show where the install is.
            GuideHost.Children.Add(new TextBlock { Text = w.ApplyHint, TextWrapping = TextWrapping.Wrap, Foreground = Ui.Secondary, Style = caption });
        }
        for (var i = 0; i < steps.Length; i++)
        {
            var step = steps[i];
            var id = i;
            if (i < done)
            {
                // Done steps fold to one line.
                GuideHost.Children.Add(new StackPanel
                {
                    Tag = id, Orientation = Orientation.Horizontal, Spacing = 8,
                    Children =
                    {
                        new FontIcon { Glyph = "\uE73E", FontSize = 12, Foreground = Ui.ToneBrush(Tone.Success) },
                        new TextBlock { Text = step.Title, Foreground = Ui.Secondary, TextTrimming = TextTrimming.CharacterEllipsis },
                        new TextBlock { Text = Ui.T(w.Serial ? OrigamiUiString.Seen : OrigamiUiString.Done), Foreground = Ui.Secondary, Style = caption, VerticalAlignment = VerticalAlignment.Center },
                    },
                });
                continue;
            }
            if (i > done)
            {
                GuideHost.Children.Add(new TextBlock { Tag = id, Text = step.Title, Foreground = Ui.Secondary, TextTrimming = TextTrimming.CharacterEllipsis, Margin = new Thickness(20, 0, 0, 0) });
                continue;
            }
            var open = new StackPanel { Spacing = 6 };
            open.Children.Add(new TextBlock { Text = step.Title, Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"], TextWrapping = TextWrapping.Wrap });
            if (step.Watch is { Length: > 0 } watch)
            {
                open.Children.Add(new TextBlock { Text = Ui.T(OrigamiUiString.WatchFor), Foreground = Ui.Secondary, Style = caption });
                open.Children.Add(new TextBlock { Text = watch, FontFamily = mono, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
                if (w.Serial) open.Children.Add(new TextBlock { Text = OrigamiFfiMethods.OrigamiWaitingFor(watch), Foreground = Ui.Secondary, Style = caption });
            }
            if (step.Typed.Length > 0)
            {
                open.Children.Add(new TextBlock { Text = Ui.T(OrigamiUiString.TypeLabel), Foreground = Ui.Secondary, Style = caption });
                foreach (var t in step.Typed)
                {
                    var line = (t.Prompt is { Length: > 0 } pr ? pr + " " : "") + t.Text;
                    open.Children.Add(new TextBlock { Text = line, FontFamily = mono, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true });
                }
                var tools = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                var typed = string.Join("\n", step.Typed.Select(t => t.Text == "Return" ? "" : t.Text));
                var copy = new Button { Content = Ui.T(OrigamiUiString.Copy) };
                AutomationProperties.SetAutomationId(copy, $"guide.copy.{i}");
                copy.Click += (_, _) => CopyToClipboard(typed);
                tools.Children.Add(copy);
                if (w.Serial && step.Send is { } send && running)
                {
                    var type = new Button { Content = Ui.T(OrigamiUiString.TypeIt) };
                    AutomationProperties.SetAutomationId(type, $"guide.typeit.{i}");
                    type.Click += (_, _) => w.TypeText(send);
                    tools.Children.Add(type);
                }
                open.Children.Add(tools);
            }
            if (step.Note is { } note)
                open.Children.Add(new TextBlock { Text = note.Text, TextWrapping = TextWrapping.Wrap, Style = caption, Foreground = note.Warning ? Ui.ToneBrush(Tone.Critical) : Ui.Secondary });
            if (!w.Serial)
            {
                var ok = new Button { Content = Ui.T(OrigamiUiString.Done), Style = (Style)Application.Current.Resources["AccentButtonStyle"] };
                AutomationProperties.SetAutomationId(ok, $"guide.done.{i}");
                ok.Click += (_, _) => { _guideDone.Add(id.ToString()); _guideSig = ""; RefreshGuide(); };
                open.Children.Add(ok);
            }
            GuideHost.Children.Add(new Border
            {
                Tag = id, Child = open, Padding = new Thickness(12), CornerRadius = new CornerRadius(Tokens.Radius.Control),
                Background = Ui.CardFill, BorderBrush = Ui.CardStroke, BorderThickness = new Thickness(1),
            });
        }
        var footer = new StackPanel { Spacing = 8, Margin = new Thickness(0, 8, 0, 0) };
        if (w.GuideFooter is { Length: > 0 } f)
        {
            var line = new TextBlock { Text = f, TextWrapping = TextWrapping.Wrap, Foreground = Ui.Secondary, Style = caption };
            AutomationProperties.SetAutomationId(line, "install.footer.server");
            footer.Children.Add(line);
        }
        if (w.ErrorText.Length > 0) footer.Children.Add(new TextBlock { Text = w.ErrorText, TextWrapping = TextWrapping.Wrap, Foreground = Ui.ToneBrush(Tone.Critical) });
        if (w.ApplyFailure.Length > 0) footer.Children.Add(new TextBlock { Text = w.ApplyFailure, TextWrapping = TextWrapping.Wrap, Foreground = Ui.ToneBrush(Tone.Critical) });
        if (w.Applying && w.ApplyLine.Length > 0) footer.Children.Add(new TextBlock { Text = w.ApplyLine, TextWrapping = TextWrapping.Wrap, Foreground = Ui.Secondary, Style = caption });
        if (w.MachineStopped)
        {
            footer.Children.Add(new TextBlock { Text = Ui.T(OrigamiUiString.TheMachineStopped), TextWrapping = TextWrapping.Wrap });
            var again = new Button { Content = Ui.T(OrigamiUiString.StartAgain) };
            AutomationProperties.SetAutomationId(again, "guide.startagain");
            again.Click += (_, _) => w.StartAgain();
            footer.Children.Add(again);
        }
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        if (w.CanInstall) buttons.Children.Add(Guide("guide.install", Ui.T(OrigamiUiString.Install), w.RunInstall, accent: true));
        if (w.Applying) buttons.Children.Add(Guide("guide.stop", Ui.T(OrigamiUiString.CancelInstall), w.StopInstall));
        if (w.CanFixed) buttons.Children.Add(Guide("guide.fixed", Ui.T(OrigamiUiString.IFixedItAtInst), w.InstallFixed));
        if (w.CanFinish) buttons.Children.Add(Guide("guide.finish", w.ContinueLabel, w.FinishInstall, accent: true));
        if (!w.Applying) buttons.Children.Add(Guide("guide.cancel", Ui.T(OrigamiUiString.CancelInstall), () => _ = w.CancelAsync()));
        footer.Children.Add(buttons);
        GuideHost.Children.Add(footer);
    }

    static Button Guide(string id, string label, Action click, bool accent = false)
    {
        var b = new Button { Content = label };
        if (accent) b.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
        AutomationProperties.SetAutomationId(b, id);
        b.Click += (_, _) => click();
        return b;
    }

    static void CopyToClipboard(string text)
    {
        var pkg = new Windows.ApplicationModel.DataTransfer.DataPackage();
        pkg.SetText(text);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
    }

    // ---- emulated CPU load -------------------------------------------------------------------

    DispatcherQueueTimer? _cpuTimer;
    int _cpuBusy;
    OsviewScene? _scene;
    public bool CpuPanelShown => CpuPanelBox.Visibility == Visibility.Visible;
    public long CpuPolls { get; private set; }
    /// <summary>Each bar in words, one per line, or the note when there are none.</summary>
    public string CpuText => string.Join("\n", CpuBars.Children.Select(c => Microsoft.UI.Xaml.Automation.AutomationProperties.GetName(c)));
    public string CpuTitleText => CpuTitle.Text;

    public void SetCpuPanel(bool on)
    {
        CpuButton.IsChecked = on;
        CpuPanelBox.Visibility = on ? Visibility.Visible : Visibility.Collapsed;
        if (!on) { _cpuTimer?.Stop(); return; } // hidden means no polling
        _cpuTimer ??= _ui.CreateTimer();
        _cpuTimer.Interval = CpuPanel.Interval;
        _cpuTimer.Tick -= CpuTick; _cpuTimer.Tick += CpuTick;
        _cpuTimer.Start();
        CpuTick(null, null);
    }

    async void CpuTick(object? s, object? e)
    {
        if (Interlocked.Exchange(ref _cpuBusy, 1) == 1) return;
        try
        {
            var key = SelectedKey;
            ShowCpu(key is null ? CpuPanel.Message(Ui.T(OrigamiUiString.SelectAMachineFirst), _scene?.Caption ?? "") : await _core.OsviewAsync(key));
            CpuPolls++;
        }
        catch (Exception ex) { ShowCpu(CpuPanel.Message(ex.Message, _scene?.Caption ?? "")); }
        finally { _cpuBusy = 0; }
    }

    static SolidColorBrush Brush(uint rgb) => new(Color.FromArgb(255, (byte)(rgb >> 16), (byte)(rgb >> 8), (byte)rgb));

    // gr_osview style: a bold italic title and coloured legend over a black-framed bar of segments, with a heavy ruler marked in tenths.
    void ShowCpu(OsviewScene v)
    {
        _scene = v;
        CpuPanelBox.Background = Brush(v.Background);
        CpuTitle.Text = v.Caption;
        CpuBars.Children.Clear();
        if (v.Bars.Count == 0)
        {
            var note = new TextBlock { Text = CpuPanel.Empty(v), FontSize = 12, Foreground = Brush(0x000000) };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(note, note.Text);
            CpuBars.Children.Add(note);
            return;
        }
        foreach (var b in v.Bars)
        {
            var head = new TextBlock { FontSize = 12, FontWeight = Microsoft.UI.Text.FontWeights.Bold, FontStyle = Windows.UI.Text.FontStyle.Italic };
            head.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run { Text = b.Title + ":", Foreground = Brush(0x000000) });
            foreach (var k in b.Legend)
                head.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run { Text = " " + k.Label, Foreground = Brush(k.Rgb) });
            var segs = new Grid { Height = 20 };
            var ticks = new Grid { Height = 20 };
            for (var i = 0; i < b.Segments.Count; i++)
            {
                segs.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Math.Max(b.Segments[i].Fraction, 0), GridUnitType.Star) });
                var r = new Microsoft.UI.Xaml.Shapes.Rectangle { Fill = Brush(b.Segments[i].Rgb) };
                Grid.SetColumn(r, i);
                segs.Children.Add(r);
            }
            for (var t = 0; t < v.Ticks; t++)
            {
                ticks.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                if (t == v.Ticks - 1) break;
                var mark = new Microsoft.UI.Xaml.Shapes.Rectangle { Width = 1, Height = 10, HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Bottom, Fill = Brush(0x000000) };
                Grid.SetColumn(mark, t);
                ticks.Children.Add(mark);
            }
            var face = new Grid();
            face.Children.Add(segs);
            face.Children.Add(ticks);
            var frame = new Border { Background = Brush(v.BarBackground), BorderBrush = Brush(v.BarBackground), BorderThickness = new Thickness(2, 2, 2, 5), Child = face };
            var item = new StackPanel { Spacing = 2 };
            item.Children.Add(head);
            item.Children.Add(frame);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, CpuPanel.Summary(b));
            CpuBars.Children.Add(item);
        }
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

    bool _wasCaptured;

    void RefreshPointerUi()
    {
        PointerButton.IsChecked = Display.Mode == PointerMode.Capture;
        // Said as a toast below the picture, which a native window keeps XAML from drawing over.
        if (Display.Captured && !_wasCaptured) Say(OrigamiFfiMethods.OrigamiCaptureHint(OrigamiPlatform.Windows));
        _wasCaptured = Display.Captured;
        RefreshStatusBar();
    }

    /// <summary>The bar under the picture: its size and the pointer, from the core, and only while the machine runs.</summary>
    void RefreshStatusBar()
    {
        var key = SelectedKey;
        var bar = key is null || _prefsOpen ? null : _core.StatusBar(key, Display.Mode != PointerMode.Capture, Display.Captured);
        PictureSize.Text = bar?.Picture ?? "";
        PointerStatus.Text = bar?.Pointer ?? "";
        // Its row is there from the moment the picture shows, so the pane the window is sized from
        // does not lose 24 pixels when the core's first line arrives.
        StatusBar.Visibility = bar is { Picture: not null } or { Pointer: not null } || Display.Visibility == Visibility.Visible ? Visibility.Visible : Visibility.Collapsed;
    }

    public bool StatusBarShownForTest => StatusBar.Visibility == Visibility.Visible;
    public string StatusBarTextForTest => PictureSize.Text + "|" + PointerStatus.Text;

    bool _previewStart;
    Tone? _headerTone;

    void Refresh()
    {
        if (_previewStart) return; // the self-test is showing the starting pane by hand
        SyncLibrary();
        RebuildTabs();
        RefreshRows();
        RefreshGuide();
        foreach (var m in _core.Machines())
            if (m.State is MachineState.Stopped or MachineState.Exited) _sized.Remove(m.Key);

        // A row left over from a library that was just replaced is not a machine to show.
        var key = SelectedKey is { } picked && _core.Machines().Any(m => m.Key == picked) ? picked : null;
        EmptyPanel.Visibility = Visibility.Collapsed;
        // The cube holds a child window and a GPU surface, so it lives only while the starting pane shows.
        var starting = key != null && !_prefsOpen && _core.Status(key).State == MachineState.Starting;
        if (!starting) StartCube.Close();
        if (key is not null || _prefsOpen) EmptyCube.Close();
        HeroScroll.Visibility = StartingPanel.Visibility = Display.Visibility = Tint.Visibility = Visibility.Collapsed;
        if (_pane is not null) _pane.Visibility = Visibility.Collapsed;
        Settings.Visibility = _prefsOpen ? Visibility.Visible : Visibility.Collapsed;
        PageHeader.Visibility = key is not null && !_prefsOpen ? Visibility.Visible : Visibility.Collapsed;
        if (key is null)
        {
            var empty = OrigamiFfiMethods.OrigamiEmptyState(_core.Machines().Count > 0 ? OrigamiEmptyKind.NoneSelected : OrigamiEmptyKind.NoMachines);
            EmptyPanel.Visibility = _prefsOpen ? Visibility.Collapsed : Visibility.Visible;
            EmptyTitle.Text = empty.Title;
            EmptyHint.Text = empty.Sentence;
            EmptyButton.Visibility = empty.Button is null ? Visibility.Collapsed : Visibility.Visible;
            if (empty.Button is { } label) EmptyButton.Content = label;
            RefreshStatusBar();
            if (!_prefsOpen) ShowEmptyCube();
            return;
        }

        var info = _core.Machines().First(m => m.Key == key);
        var st = _core.Status(key);
        var row = _core.Row(key);
        var shown = row.State;
        HeaderName.Text = info.Name;
        AutomationProperties.SetName(HeaderName, info.Name);
        HeaderState.Text = row.Word;
        if (_headerTone != row.Tone || HeaderDot.Content is null != (row.Tone == Tone.None))
        {
            _headerTone = row.Tone;
            HeaderDot.Content = row.Tone == Tone.None ? null : Ui.Dot(row.Tone);
        }
        UpdateCommands(st.State, key, info);
        BuildCards(info);
        StageText.Text = st.Note ?? "";

        if (_prefsOpen) { Display.Hide(); RefreshStatusBar(); return; }
        if (shown is ShownState.NeedsUpgrade or ShownState.CantOpen)
        {
            Detach();
            ShowTint(ChassisOf(info));
            ShowMachinePage(key, info, st, shown, art: true);
        }
        else
            switch (st.State)
            {
                case MachineState.Stopped:
                case MachineState.Exited:
                case MachineState.LeftRunning: // the page, with its card or Start attaching to the runner that is still up
                    Detach();
                    ShowTint(ChassisOf(info));
                    ShowMachinePage(key, info, st, shown, art: true);
                    break;
                case MachineState.Stopping:
                    Detach();
                    ShowTint(ChassisOf(info));
                    ShowMachinePage(key, info, st, shown, art: false);
                    break;
                case MachineState.Starting:
                    Detach();
                    StartingPanel.Visibility = Visibility.Visible;
                    ShowStartCube(info);
                    break;
                case var _ when shown == ShownState.NoPicture: // running, nothing drawn yet: the card offers the console
                    Detach();
                    ShowTint(ChassisOf(info));
                    ShowMachinePage(key, info, st, shown, art: false);
                    break;
                case var _ when !info.HasDisplay: // serial only: the console is the machine's view
                    Display.Detach(); _attachedKey = _modeKey = null;
                    ShowConsolePane(key);
                    break;
                default:
                    ClosePane();
                    Display.Visibility = Visibility.Visible;
                    if (_dialogOpen) Display.Hide(); else Display.Show();
                    if (_modeKey != key) { _modeKey = key; Display.Mode = _settings.ModeFor(key); }
                    if (!Display.Attached || _attachedKey != key)
                    {
                        // The panel must be laid out before the child window is placed over it.
                        _ui.TryEnqueue(DispatcherQueuePriority.Low, () => { if (Display.Attach(_core, key)) { _attachedKey = key; SizeForPicture(key); if (_dialogOpen) Display.Hide(); } });
                    }
                    break;
            }
        RefreshPointerUi();
    }

    /// <summary>Shows only the commands that fit the machine's state: Start while it is down, the Power off button while it is up.</summary>
    void UpdateCommands(MachineState state, string key, MachineSummary info)
    {
        bool On(string id) => CommandOn(id, state, key);
        string Title(string id) => _commands.First(c => c.Id == id).Title;
        var canStop = On("machine.stop");
        // While it starts the button cancels the start, in the core's words.
        _cancelMode = state == MachineState.Starting;
        var startId = _cancelMode ? "machine.cancel-start" : "machine.start";
        StartButton.Label = Title(startId);
        ToolTipService.SetToolTip(StartButton, Ui.Tooltip(startId));
        StartButton.Visibility = canStop ? Visibility.Collapsed : Visibility.Visible;
        StartButton.IsEnabled = On(startId);
        PowerHost.Visibility = canStop ? Visibility.Visible : Visibility.Collapsed;
        var paused = state == MachineState.Paused;
        _pauseItem.Text = Title(paused ? "machine.resume" : "machine.pause");
        _pauseItem.IsEnabled = On(paused ? "machine.resume" : "machine.pause");
        foreach (var item in PowerMenu.Items.OfType<MenuFlyoutItem>().Skip(1))
            item.IsEnabled = On(AutomationProperties.GetAutomationId(item) == "cmd.reset" ? "machine.reset" : "machine.force-quit");
        ConsoleButton.IsEnabled = On("window.console");
        PointerButton.IsEnabled = On("machine.capture-mouse");
        SettingsButton.IsEnabled = On("machine.settings");
        DiagnosticsButton.IsEnabled = On("window.diagnostics");
        ScreenshotButton.IsEnabled = On("machine.screenshot");
        InstallButton.Visibility = info.RunsIrix ? Visibility.Visible : Visibility.Collapsed;
        InstallButton.IsEnabled = On("machine.install-irix");
    }

    /// <summary>The machine's page in place of a picture: its art, the card for its state, Start, and what the machine is.</summary>
    void ShowMachinePage(string key, MachineSummary info, MachineStatus st, ShownState shown, bool art)
    {
        HeroScroll.Visibility = Visibility.Visible;
        HeroImage.Source = art && StillOf(info) is { } hs ? Art(hs.Id, "hero@2x.png") : null;
        HeroImage.Visibility = HeroImage.Source is null ? Visibility.Collapsed : Visibility.Visible;
        var card = _core.Card(key, info.Name, shown);
        // A machine that ended on its own has no card; what the core knows goes behind Show details.
        if (card is null && shown == ShownState.Exited && (st.LastError is { Length: > 0 } || st.Reason.Length > 0))
            card = new CardInfo("", "", Array.Empty<CardAction>(), st.LastError is { Length: > 0 } e ? e : st.Reason);
        FillCard(key, info.Name, card, _upgrading.Contains(key) || shown == ShownState.Saving);
        HeroStart.Visibility = shown is ShownState.Stopped or ShownState.Exited ? Visibility.Visible : Visibility.Collapsed;
        HeroStart.IsEnabled = _core.CommandEnabled("machine.start", st.State);
        Cards.Visibility = shown is ShownState.Stopped or ShownState.Exited or ShownState.LeftRunning ? Visibility.Visible : Visibility.Collapsed;
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
        _previewStart = true;
        _prefsOpen = false;
        Display.Hide(); // the screen's child window is not hidden by collapsing its pane
        HeroScroll.Visibility = Display.Visibility = Visibility.Collapsed;
        StartingPanel.Visibility = Visibility.Visible;
        ShowStartCube(_core.Machines().First(x => x.Key == SelectedKey));
    }
    public CubeHost StartCubeForTest => StartCube;
    public void Restore() { _previewStart = false; Refresh(); }

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
            catch (Exception e) { Say("Console: " + e.Message, severity: InfoBarSeverity.Error); return; }
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
        if (_installs.Values.Any(InstallRunning) && !_installQuitApproved)
        {
            args.Cancel = true;
            _quitting = true;
            try
            {
                if (!await ConfirmStopInstallsAsync()) return;
                _installQuitApproved = true;
                foreach (var w in _installs.Values.Where(InstallRunning).ToList()) await w.StopAndCloseAsync();
            }
            finally { _quitting = false; }
            Close();
            return;
        }
        var up = Up(); // live state, so only machines that are up right now are named
        if (up.Count == 0 || !_settings.ConfirmQuitWithRunningMachines) { _recheck?.Stop(); Display.Detach(); return; }
        args.Cancel = true;
        _quitting = true;
        try
        {
            var answer = await AskAsync(new OrigamiAlertKind.QuitRunning(up.Select(m => m.Name).ToArray()));
            if (answer is null or { Role: ButtonRole.Cancel }) return;
            if (answer.Role == ButtonRole.Destructive && !await PowerOffAll(up)) return;
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
            XamlRoot = Root.XamlRoot, Title = Ui.T(OrigamiUiString.PoweringOff),
            Content = new StackPanel { Spacing = 12, Children = { text, new ProgressBar { IsIndeterminate = true } } },
        };
        var shown = ShowDialogAsync(panel);
        var failed = new List<MachineSummary>();
        try
        {
            foreach (var m in todo)
            {
                text.Text = OrigamiFfiMethods.OrigamiPowerOffProgress(m.Name, false);
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
        var answer = await AskAsync(new OrigamiAlertKind.PowerOffFailed(failed.Select(m => m.Name).ToArray()));
        if (answer is null or { Role: ButtonRole.Cancel }) return false;
        foreach (var m in failed) await RunAsync(() => _core.ForceQuitAsync(m.Key));
        return true;
    }
}
