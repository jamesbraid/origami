using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Origami.Logic;
using uniffi.origami_ffi;

namespace Origami.App;

/// <summary>The Install IRIX assistant for one machine: widgets over the core's <see cref="OrigamiInstallFlow"/>, which owns every choice and rule.</summary>
public sealed class InstallWindow : Window
{
    readonly UniffiCoreClient _core;
    readonly MainWindow _host;
    readonly string _key;
    readonly Action _showMachine, _openConsole;
    readonly Func<string, Task> _tell;
    readonly OrigamiInstallTexts _t = OrigamiFfiMethods.OrigamiInstallTexts();
    readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _timer;
    readonly Grid _root = new();
    readonly StackPanel _steps = new() { Spacing = 6, Padding = new Thickness(16) };
    readonly StackPanel _body = new() { Spacing = 10, Padding = new Thickness(20, 16, 20, 16) };
    readonly TextBlock _error = new() { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed, Foreground = Ui.ToneBrush(Tone.Critical), Margin = new Thickness(20, 12, 20, 0) };
    readonly StackPanel _stopped = new() { Orientation = Orientation.Horizontal, Spacing = 12, Visibility = Visibility.Collapsed, Margin = new Thickness(20, 12, 20, 0) };
    readonly TextBlock _reason = new() { TextWrapping = TextWrapping.Wrap, Foreground = Ui.Secondary, Visibility = Visibility.Collapsed };
    readonly TextBlock _server = new() { TextWrapping = TextWrapping.Wrap, VerticalAlignment = VerticalAlignment.Center };
    readonly Button _restart, _startOver, _cancel, _back, _continue;

    OrigamiInstallFlow? _flow;
    OrigamiInstallGuide? _guide;
    OrigamiOperation? _serve, _check, _apply;
    OrigamiMediaFolder? _found;
    OrigamiInstallPlan? _plan;
    string _checkFailure = "", _checkInfo = "", _mediaError = "", _applyError = "";
    bool _serial, _noDisk, _busy, _closed, _closeApproved, _asking, _runStarted, _starting, _full;
    // Controls of the step on show that change after it was built.
    TextBlock? _mediaFile, _mediaVerdict, _checkStatus, _checkNote;
    ProgressBar? _checkBar;
    Button? _checkRun, _checkStop, _mediaChoose;
    CheckBox? _checkFull;

    /// <summary>The question on show, for the self-test.</summary>
    public ContentDialog? OpenDialog { get; private set; }

    public InstallWindow(MainWindow host, UniffiCoreClient core, string key, string name, Action showMachine, Action openConsole, Func<string, Task> tell)
    {
        _host = host; _core = core; _key = key; _showMachine = showMachine; _openConsole = openConsole; _tell = tell;
        Title = "Install IRIX: " + name;
        AppIcon.Apply(this);
        AutomationProperties.SetAutomationId(_error, "install.error");
        AutomationProperties.SetAutomationId(_server, "install.footer.server");
        AutomationProperties.SetAutomationId(_reason, "install.continue.reason");
        _stopped.Children.Add(new TextBlock { Text = Ui.T(OrigamiUiString.TheMachineStopped), VerticalAlignment = VerticalAlignment.Center });
        _stopped.Children.Add(Btn(Ui.T(OrigamiUiString.StartAgain), "install.startagain", () => _ = BusyAsync(() => _core.StartAsync(_key))));

        _restart = Btn("Restart", "install.restart", () => _ = RestartServerAsync());
        _startOver = Btn("Start over", "install.startover", () => _ = StartOverAsync());
        _cancel = Btn(Ui.T(OrigamiUiString.Cancel), "install.cancel", () => _ = TryCloseAsync());
        _back = Btn("Back", "install.back", () => { if (_busy) return; _flow!.Back(); ShowStep(); });
        _continue = Btn("Continue", "install.continue", () => _ = ContinueAsync());
        _continue.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
        _restart.Visibility = _startOver.Visibility = Visibility.Collapsed;

        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { _restart, _startOver, _cancel, _back, _continue } };
        var footer = new Grid { Padding = new Thickness(16, 10, 16, 10), ColumnSpacing = 12, ColumnDefinitions = { new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }, new ColumnDefinition { Width = GridLength.Auto } } };
        footer.Children.Add(new StackPanel { Spacing = 4, VerticalAlignment = VerticalAlignment.Center, Children = { _server, _reason } });
        Grid.SetColumn(buttons, 1);
        footer.Children.Add(buttons);

        var right = new Grid { RowDefinitions = { new RowDefinition { Height = GridLength.Auto }, new RowDefinition { Height = GridLength.Auto }, new RowDefinition { Height = new GridLength(1, GridUnitType.Star) } } };
        var scroll = new ScrollViewer { Content = _body };
        Grid.SetRow(_stopped, 1); Grid.SetRow(scroll, 2);
        right.Children.Add(_error); right.Children.Add(_stopped); right.Children.Add(scroll);
        Grid.SetColumn(right, 1);

        _root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        _root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        _root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(220) });
        _root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetRow(footer, 1); Grid.SetColumnSpan(footer, 2);
        _root.Children.Add(_steps); _root.Children.Add(right); _root.Children.Add(footer);
        Content = _root;
        AppWindow.Resize(new Windows.Graphics.SizeInt32(960, 680));

        _timer = DispatcherQueue.CreateTimer();
        _timer.Interval = TimeSpan.FromSeconds(1);
        _timer.Tick += (_, _) => Poll();
        _timer.Start();
        AppWindow.Closing += OnClosing;
        Closed += (_, _) => { _closed = true; _timer.Stop(); _check?.Cancel(); GuideChanged?.Invoke(); };
        _ = BeginAsync();
    }

    // ---- hooks for the self-test and the main window ------------------------------------------

    internal OrigamiInstallStep? StepForTest => _flow?.Step();
    internal List<string> StepNamesForTest => _flow?.StepTitles().Select(t => $"{t.Number}. {t.Title}").ToList() ?? new();
    /// <summary>The numbered title of a step, as the core lists it.</summary>
    string TitleOf(OrigamiInstallStep s) => _flow!.StepTitles().First(t => t.Step == s).Title;
    /// <summary>Why Continue is off, as the page says it.</summary>
    public string ReasonForTest => _reason.Text;
    public string ServerLineForTest => _server.Text;
    public bool Serving => _serve?.State() is OrigamiOperationState.Running || _apply is not null;

    public T? Find<T>(string id) where T : FrameworkElement => FindWhere<T>(t => AutomationProperties.GetAutomationId(t) == id);

    public T? FindWhere<T>(Func<T, bool> match) where T : FrameworkElement
    {
        T? In(DependencyObject o)
        {
            if (o is T t && match(t)) return t;
            for (var i = 0; i < VisualTreeHelper.GetChildrenCount(o); i++) if (In(VisualTreeHelper.GetChild(o, i)) is { } hit) return hit;
            return null;
        }
        return In(_root);
    }

    /// <summary>Quit or tab close after the person chose to stop: end the server, force the machine down, close without asking.</summary>
    public async Task StopAndCloseAsync()
    {
        _serve?.Cancel(); _apply?.Cancel();
        try { await _core.ForceQuitAsync(_key); } catch (Exception e) { App.Log("install stop: " + e.Message); }
        _closeApproved = true;
        Close();
    }

    // ---- starting ----------------------------------------------------------------------------

    async Task BeginAsync()
    {
        try
        {
            var (plan, serial, pending) = await Task.Run(() => (_core.Raw.InstallPlan(_key), _core.Open(_key).BootConsole() == OrigamiBootConsole.Serial, _core.Raw.InstallRestorePending(_key)));
            _serial = serial;
            _flow = new OrigamiInstallFlow(plan);
            // A machine left on the installation network offers to set it back on close.
            if (pending) _flow.SetPrepared();
            _flow.SetSerial(serial);
            _plan = plan;
        }
        catch (Exception e) { ShowError(e.Message); return; }
        ShowStep();
    }

    // ---- the guide: once the install starts it lives in the main window's pane ----------------

    /// <summary>The steps from the PROM menu to the halt, once the core has read them.</summary>
    internal OrigamiGuideStep[]? GuideSteps => _guide?.Steps;
    public bool Serial => _serial;
    public bool Automatic => _flow?.Automatic() ?? false;
    /// <summary>The line at the foot: the install files, in the core's words.</summary>
    public string GuideFooter => _flow is { } f ? Footer(f, f.Step()) : "";
    public string ApplyHint => _flow?.ApplyHint() ?? "";
    public bool CanInstall => _flow?.CanInstall(Up()) ?? false;
    public bool CanFixed => _flow?.CanFixed() ?? false;
    public bool Applying => _flow?.Apply() is OrigamiApplyState.Running;
    public string ApplyLine => Applying ? _apply?.Progress() ?? "" : "";
    public string ApplyFailure => _flow?.Apply() is OrigamiApplyState.Failed ? _applyError : "";
    public bool CanFinish => _flow is { } f && f.Step() == OrigamiInstallStep.Finish && f.CanContinue(Up());
    public string ContinueLabel => _flow?.ContinueLabel() ?? "";
    /// <summary>How many leading guide steps the console shows done; asks the core, which reads the console from where it left off.</summary>
    internal int GuideSeen() => _guide is { } g && _serial ? (int)_core.Open(_key).GuideSeen(g.Steps) : 0;
    public bool Busy => _busy;
    /// <summary>What went wrong while the guide runs, for the pane to show.</summary>
    public string ErrorText => _error.Visibility == Visibility.Visible ? _error.Text : "";
    /// <summary>The machine went down in the middle of the install: the pane offers Start again.</summary>
    public bool MachineStopped => _stopped.Visibility == Visibility.Visible;
    public void StartAgain() => _ = BusyAsync(() => _core.StartAsync(_key));
    public bool Closing => _closed;
    /// <summary>Raised when the guide appears, changes what it offers or goes away; the main window rebuilds its pane.</summary>
    public event Action? GuideChanged;

    /// <summary>The window's own pages end where the run begins: the machine starts, the guide moves to the pane, and this window steps aside.</summary>
    void EnterGuide()
    {
        var flow = _flow!;
        if (!_runStarted) { _runStarted = true; _ = EnterRunAsync(); }
        // The guide lists every step to the halt, so the flow's own pages in between are passed over.
        var last = flow.Automatic() ? OrigamiInstallStep.Packages : OrigamiInstallStep.Finish;
        while (flow.Step() < last) flow.Next();
        AppWindow.Hide();
        GuideChanged?.Invoke();
        Poll();
    }

    public void RunInstall() => _ = RunApplyAsync();
    public void StopInstall() => _apply?.Cancel();
    public void InstallFixed() { _flow!.Fixed(); GuideChanged?.Invoke(); }
    public void FinishInstall() => _ = ContinueAsync();
    public Task CancelAsync() => TryCloseAsync();
    public void TypeText(string send) => TypeIt(send);

    // ---- the frame ---------------------------------------------------------------------------

    static Button Btn(string text, string id, Action click)
    {
        var b = new Button { Content = text };
        AutomationProperties.SetAutomationId(b, id);
        b.Click += (_, _) => click();
        return b;
    }

    static TextBlock Text(string t, bool dim = false) => new() { Text = t, TextWrapping = TextWrapping.Wrap, Opacity = dim ? 0.7 : 1 };
    static TextBlock Heading(string t) => new() { Text = t, FontSize = 20, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };

    /// <summary>A checkbox that reports each change, whether a click or the automation tree made it.</summary>
    static CheckBox Tick(string id, UIElement content, bool on, Action<bool> changed, bool enabled = true)
    {
        var c = new CheckBox { Content = content, IsChecked = on, IsEnabled = enabled };
        AutomationProperties.SetAutomationId(c, id);
        c.Checked += (_, _) => changed(true);
        c.Unchecked += (_, _) => changed(false);
        return c;
    }

    void ShowError(string text)
    {
        _error.Text = text;
        _error.Visibility = Visibility.Visible;
    }

    bool Up() => _core.Status(_key).State is MachineState.Running or MachineState.Starting or MachineState.Paused or MachineState.Stopping or MachineState.LeftRunning;

    void ShowStep()
    {
        if (_flow is not { } flow || _closed) return;
        _error.Visibility = Visibility.Collapsed;
        var step = flow.Step();
        _steps.Children.Clear();
        foreach (var t in flow.StepTitles())
        {
            var now = t.State == OrigamiStepState.Current;
            _steps.Children.Add(new TextBlock { Text = $"{t.Number}. {t.Title}", FontWeight = now ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal, Opacity = t.State == OrigamiStepState.Later ? 0.7 : 1 });
        }
        _body.Children.Clear();
        _mediaFile = _mediaVerdict = _checkStatus = _checkNote = null; _checkBar = null; _checkRun = _checkStop = _mediaChoose = null; _checkFull = null;
        if (step >= OrigamiInstallStep.Prepare) { EnterGuide(); return; }
        switch (step)
        {
            case OrigamiInstallStep.Release: BuildRelease(); break;
            case OrigamiInstallStep.Media: BuildMedia(); break;
            case OrigamiInstallStep.Software: _ = BuildSoftwareAsync(); break;
            case OrigamiInstallStep.Method: BuildMethod(); break;
            case OrigamiInstallStep.Disk: _ = BuildDiskAsync(); break;
            case OrigamiInstallStep.Check: BuildCheck(); break;
        }
        Poll();
    }

    /// <summary>Everything that follows the machine and the media server: once a second and after each change.</summary>
    void Poll()
    {
        if (_flow is not { } flow || _closed) return;
        var st = _core.Status(_key).State;
        var step = flow.Step();
        var inRun = step is >= OrigamiInstallStep.Prepare and <= OrigamiInstallStep.Packages;
        _stopped.Visibility = inRun && _runStarted && !_starting && !Up() && !(flow.Automatic() && flow.Apply() is OrigamiApplyState.Done) ? Visibility.Visible : Visibility.Collapsed;

        var failed = _serve?.State() as OrigamiOperationState.Failed;
        _restart.Visibility = failed is not null ? Visibility.Visible : Visibility.Collapsed;
        _server.Text = failed?.Message ?? Footer(flow, step);
        if (failed is not null) _server.Foreground = Ui.ToneBrush(Tone.Critical); else _server.ClearValue(TextBlock.ForegroundProperty);

        var checking = flow.Check() == OrigamiCheckState.Running;
        _startOver.Visibility = flow.Resumed() && step <= OrigamiInstallStep.Check ? Visibility.Visible : Visibility.Collapsed;
        _startOver.IsEnabled = !_busy && !checking;
        _back.IsEnabled = !_busy && flow.CanBack() && !checking;
        _continue.Content = flow.ContinueLabel();
        _continue.IsEnabled = !_busy && flow.CanContinue(Up());
        _reason.Text = flow.ContinueReason(Up()) ?? "";
        _reason.Visibility = _reason.Text.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        if (step == OrigamiInstallStep.Check) UpdateCheck();
        GuideChanged?.Invoke();
    }

    /// <summary>The line at the foot while the install files are served, in the core's words and with no address.</summary>
    string Footer(OrigamiInstallFlow flow, OrigamiInstallStep step) =>
        _plan is { } p && (_serve?.State() is OrigamiOperationState.Running || (flow.Resumed() && step <= OrigamiInstallStep.Check)) ? OrigamiFfiMethods.OrigamiInstallFooter(p) : "";

    async Task<bool> BusyAsync(Func<Task> work)
    {
        _busy = true; Poll();
        try { await work(); return true; }
        catch (Exception e) { ShowError(e.Message); return false; }
        finally { _busy = false; Poll(); }
    }

    async Task ContinueAsync()
    {
        if (_flow is not { } flow || _busy) return;
        if (flow.Step() == OrigamiInstallStep.Finish) { await FinishAsync(); return; }
        if (flow.Step() == OrigamiInstallStep.Disk && _noDisk)
        {
            var mib = (ulong)flow.NewDiskGb() * 1024;
            if (!await BusyAsync(async () =>
            {
                await _core.ApplyChangeAsync(_key, new MachineChange.CreateDisk(mib));
                flow.SetDisk(await ReadDiskAsync());
                flow.SetErase(true);
            })) return;
        }
        flow.Next();
        ShowStep();
    }

    async Task StartOverAsync()
    {
        if (_flow is not { } flow) return;
        _serve?.Cancel(); _serve = null; _guide = null; _runStarted = false;
        AppWindow.Show();
        if (!await BusyAsync(() => Task.Run(() => _core.Raw.InstallReset(_key)))) return;
        flow.Restart();
        _plan = null;
        ShowStep();
    }

    // ---- Release -----------------------------------------------------------------------------

    void BuildRelease()
    {
        var flow = _flow!;
        _body.Children.Add(Heading(TitleOf(OrigamiInstallStep.Release)));
        var list = new StackPanel { Spacing = 6 };
        _body.Children.Add(list);
        void Fill()
        {
            list.Children.Clear();
            foreach (var r in flow.Releases())
            {
                var radio = new RadioButton { Content = r.Label, GroupName = "release", IsChecked = r.Release == flow.Release() };
                AutomationProperties.SetAutomationId(radio, "install.release." + r.Release);
                radio.Checked += (_, _) =>
                {
                    if (flow.Release() == r.Release) return;
                    flow.SelectRelease(r.Release);
                    DispatcherQueue.TryEnqueue(Fill); // the profiles under it change
                };
                list.Children.Add(radio);
                if (r.Release != flow.Release()) continue;
                var profiles = new StackPanel { Margin = new Thickness(28, 0, 0, 8), Spacing = 4 };
                foreach (var p in r.Profiles)
                {
                    var pr = new RadioButton { GroupName = "profile", IsChecked = p.Name == flow.Profile(), Content = new StackPanel { Children = { Text(p.Label), Text(p.Detail, dim: true) } } };
                    AutomationProperties.SetAutomationId(pr, "install.profile." + p.Name);
                    pr.Checked += (_, _) => flow.SelectProfile(p.Name);
                    profiles.Children.Add(pr);
                }
                list.Children.Add(profiles);
            }
        }
        Fill();
    }

    // ---- Media -------------------------------------------------------------------------------

    void BuildMedia()
    {
        var flow = _flow!;
        _body.Children.Add(Heading(Ui.T(OrigamiUiString.WhereShouldTheDiscsComeFrom)));
        var download = new RadioButton { Content = Ui.T(OrigamiUiString.DownloadThem), GroupName = "media", IsChecked = !flow.UseFolder() };
        var folder = new RadioButton { Content = Ui.T(OrigamiUiString.UseDiscsIAlreadyHave), GroupName = "media", IsChecked = flow.UseFolder() };
        AutomationProperties.SetAutomationId(download, "install.media.download");
        AutomationProperties.SetAutomationId(folder, "install.media.folder");
        var note = Text(Ui.T(OrigamiUiString.NeedsAnInternetConnection), dim: true);
        note.Margin = new Thickness(28, 0, 0, 8);
        _mediaChoose = Btn(Ui.T(OrigamiUiString.ChooseFolder), "install.media.choose", () => _ = ChooseFolderAsync());
        _mediaFile = new TextBlock { TextWrapping = TextWrapping.Wrap };
        _mediaVerdict = new TextBlock { TextWrapping = TextWrapping.Wrap };
        AutomationProperties.SetAutomationId(_mediaFile, "install.media.path");
        AutomationProperties.SetAutomationId(_mediaVerdict, "install.media.verdict");
        var below = new StackPanel { Margin = new Thickness(28, 0, 0, 0), Spacing = 6, Children = { _mediaChoose, _mediaFile, _mediaVerdict } };
        download.Checked += (_, _) => { flow.SetUseFolder(false); UpdateMedia(); Poll(); };
        folder.Checked += (_, _) => { flow.SetUseFolder(true); UpdateMedia(); Poll(); };
        _body.Children.Add(download); _body.Children.Add(note); _body.Children.Add(folder); _body.Children.Add(below);
        UpdateMedia();
        // The release may have changed since the folder was last looked at.
        if (flow.Folder() is { } f) _ = CheckFolderAsync(f);
    }

    void UpdateMedia()
    {
        if (_flow is not { } flow || _mediaVerdict is null || _mediaFile is null || _mediaChoose is null) return;
        _mediaChoose.Visibility = _mediaFile.Visibility = _mediaVerdict.Visibility = flow.UseFolder() ? Visibility.Visible : Visibility.Collapsed;
        _mediaFile.Text = flow.Folder() ?? "";
        if (_mediaError.Length > 0) { _mediaVerdict.Text = _mediaError; _mediaVerdict.Foreground = Ui.ToneBrush(Tone.Critical); return; }
        _mediaVerdict.Text = _found?.Verdict ?? (flow.Folder() is null ? "" : "Looking…");
        // Green for all the discs, amber for some, red for none.
        if (_found is { } m) _mediaVerdict.Foreground = Ui.ToneBrush(m.Complete ? Tone.Success : m.Found > 0 ? Tone.Caution : Tone.Critical);
    }

    async Task ChooseFolderAsync()
    {
        PathPicker.Hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        if (await PathPicker.PickFolderAsync() is not { } path) return;
        _found = null; _mediaError = "";
        await CheckFolderAsync(path);
    }

    async Task CheckFolderAsync(string path)
    {
        var flow = _flow!;
        try
        {
            var found = await Task.Run(() => OrigamiFfiMethods.OrigamiInstallMediaFolder(path, flow.Release(), flow.Profile()));
            flow.SetFolder(path, found);
            _found = found; _mediaError = "";
        }
        catch (Exception e) { _mediaError = e.Message; }
        if (_closed) return;
        UpdateMedia(); Poll();
    }

    // ---- Software ----------------------------------------------------------------------------

    async Task BuildSoftwareAsync()
    {
        var flow = _flow!;
        _body.Children.Add(Heading(TitleOf(OrigamiInstallStep.Software)));
        var list = new StackPanel { Spacing = 8 };
        _body.Children.Add(list);
        try { flow.SetChoices(await Task.Run(() => _core.Raw.InstallAddonChoices(_key, flow.Profile()))); }
        catch (Exception e) { ShowError(e.Message); return; }
        if (flow.Step() != OrigamiInstallStep.Software || _closed) return;
        FillSoftware(list);
    }

    void FillSoftware(StackPanel list)
    {
        var flow = _flow!;
        list.Children.Clear();
        foreach (var a in flow.Addons())
        {
            var c = a.Choice;
            var label = new StackPanel { Children = { Text(c.Label) } };
            if (c.Note is { Length: > 0 } n) label.Children.Add(Text(n, dim: true));
            list.Children.Add(Tick("install.addon." + c.Name, label, a.On, v => flow.Tick(c.Name, v), !c.Locked));
        }
        var extras = flow.Extras();
        for (var i = 0; i < extras.Length; i++)
        {
            var at = (uint)i;
            var row = new Grid { ColumnDefinitions = { new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }, new ColumnDefinition { Width = GridLength.Auto } } };
            row.Children.Add(Text(extras[i].Source + (extras[i].Install.Length > 0 ? ": " + string.Join(", ", extras[i].Install) : "")));
            var remove = Btn(Ui.T(OrigamiUiString.Remove), "install.package.remove", () => { flow.RemoveExtra(at); FillSoftware(list); });
            Grid.SetColumn(remove, 1);
            row.Children.Add(remove);
            list.Children.Add(row);
        }
        list.Children.Add(Btn(Ui.T(OrigamiUiString.AddAPackage2), "install.package.add", () => _ = AddPackageAsync(list)));
    }

    async Task AddPackageAsync(StackPanel list)
    {
        if (OpenDialog is not null) return;
        var source = new TextBox { Header = Ui.T(OrigamiUiString.SourceAFileOrAn), PlaceholderText = "https://" };
        var lines = new TextBox { Header = Ui.T(OrigamiUiString.PackagesToInstallOnePerLine), AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MinHeight = 90 };
        AutomationProperties.SetAutomationId(source, "install.package.source");
        AutomationProperties.SetAutomationId(lines, "install.package.lines");
        var choose = Btn(Ui.T(OrigamiUiString.ChooseFile), "install.package.choose", () => _ = ChooseFileAsync(source));
        var dlg = new ContentDialog
        {
            XamlRoot = _root.XamlRoot, Title = Ui.T(OrigamiUiString.AddAPackage),
            Content = new StackPanel { Spacing = 10, Width = 420, Children = { source, choose, lines } },
            PrimaryButtonText = Ui.T(OrigamiUiString.Add), CloseButtonText = Ui.T(OrigamiUiString.Cancel), DefaultButton = ContentDialogButton.Primary, IsPrimaryButtonEnabled = false,
        };
        source.TextChanged += (_, _) => dlg.IsPrimaryButtonEnabled = source.Text.Trim().Length > 0;
        OpenDialog = dlg;
        ContentDialogResult r;
        try { r = await dlg.ShowAsync(); }
        finally { OpenDialog = null; }
        if (r != ContentDialogResult.Primary || _flow is not { } flow || !flow.AddExtra(source.Text.Trim(), lines.Text)) return;
        FillSoftware(list);
    }

    async Task ChooseFileAsync(TextBox into)
    {
        PathPicker.Hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        if (await PathPicker.PickFileAsync() is { } p) into.Text = p;
    }

    // ---- Disk --------------------------------------------------------------------------------

    async Task<OrigamiDiskState> ReadDiskAsync() =>
        await Task.Run(() => _core.Raw.InstallDisk(_key)) is { } d ? new OrigamiDiskState.Present(d) : new OrigamiDiskState.None();

    async Task BuildDiskAsync()
    {
        var flow = _flow!;
        _body.Children.Add(Heading(TitleOf(OrigamiInstallStep.Disk)));
        OrigamiDiskState disk;
        try { disk = await ReadDiskAsync(); }
        catch (Exception e) { ShowError(e.Message); return; }
        if (flow.Step() != OrigamiInstallStep.Disk || _closed) return;
        flow.SetDisk(disk);
        _noDisk = disk is OrigamiDiskState.None;
        if (disk is OrigamiDiskState.Present p)
        {
            var text = Text(p.Disk.Text);
            AutomationProperties.SetAutomationId(text, "install.disk.image");
            _body.Children.Add(text);
            _body.Children.Add(Tick("install.disk.erase", Text("Erase this disk and install onto it"), flow.Erase(), v => { flow.SetErase(v); Poll(); }));
            _body.Children.Add(Text("The disk formatter erases everything on it.", dim: true));
        }
        else
        {
            _body.Children.Add(Text("This machine has no disk."));
            var size = new NumberBox { Header = "Create a disk (GB)", Value = flow.NewDiskGb(), Minimum = 1, Maximum = 2048, Width = 160, HorizontalAlignment = HorizontalAlignment.Left };
            AutomationProperties.SetAutomationId(size, "install.disk.size");
            size.ValueChanged += (_, _) => { if (!double.IsNaN(size.Value)) flow.SetNewDiskGb((uint)size.Value); Poll(); };
            _body.Children.Add(size);
            _body.Children.Add(Text("The disk is made when you continue.", dim: true));
        }
        Poll();
    }

    // ---- Check -------------------------------------------------------------------------------

    void BuildCheck()
    {
        _body.Children.Add(Heading(TitleOf(OrigamiInstallStep.Check)));
        _body.Children.Add(Text(_t.CheckIntro));
        _checkRun = Btn(Ui.T(OrigamiUiString.CheckNow), "install.check.run", () => _ = RunCheckAsync());
        _checkStop = Btn(Ui.T(OrigamiUiString.Cancel), "install.check.cancel", () => _check?.Cancel());
        _checkFull = Tick("install.check.full", Text("Check thoroughly (also reads the downloaded discs)"), _full, v => _full = v);
        _checkStatus = new TextBlock { TextWrapping = TextWrapping.Wrap };
        _checkNote = Text("", dim: true);
        _checkBar = new ProgressBar { IsIndeterminate = true };
        AutomationProperties.SetAutomationId(_checkStatus, "install.check.status");
        AutomationProperties.SetAutomationId(_checkNote, "install.check.info");
        _body.Children.Add(_checkFull);
        _body.Children.Add(new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { _checkRun, _checkStop } });
        _body.Children.Add(_checkBar); _body.Children.Add(_checkStatus); _body.Children.Add(_checkNote);
        UpdateCheck();
    }

    void UpdateCheck()
    {
        if (_flow is not { } flow || _checkStatus is null || _checkRun is null || _checkStop is null || _checkBar is null || _checkNote is null || _checkFull is null) return;
        var state = flow.Check();
        var running = state == OrigamiCheckState.Running;
        _checkBar.Visibility = _checkStop.Visibility = running ? Visibility.Visible : Visibility.Collapsed;
        _checkRun.IsEnabled = _checkFull.IsEnabled = !running;
        _checkRun.Content = Ui.T(state == OrigamiCheckState.Failed ? OrigamiUiString.CheckAgain : OrigamiUiString.CheckNow);
        _checkStatus.Text = state == OrigamiCheckState.Failed ? _checkFailure : flow.CheckStatus() ?? "";
        _checkStatus.Foreground = state == OrigamiCheckState.Failed ? Ui.ToneBrush(Tone.Critical) : state == OrigamiCheckState.Passed ? Ui.ToneBrush(Tone.Success) : _checkNote.Foreground;
        _checkNote.Text = _checkInfo;
        _checkNote.Visibility = _checkInfo.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    async Task RunCheckAsync()
    {
        if (_flow is not { } flow || flow.Check() == OrigamiCheckState.Running) return;
        flow.SetCheck(OrigamiCheckState.Running); _checkFailure = ""; _checkInfo = "";
        Poll();
        try
        {
            // An assistant opened on an existing plan checks it as it is.
            if (!flow.Resumed())
            {
                var setup = flow.Setup();
                var report = await Task.Run(() => _core.Raw.InstallPrepare(_key, setup));
                flow.SetPrepared();
                _plan = await Task.Run(() => _core.Raw.InstallPlan(_key));
                _checkInfo = string.Join(" ", report.Where(l => l.Kind is OrigamiLineKind.Warning or OrigamiLineKind.Advice).Select(l => l.Text));
            }
            var op = _check = await Task.Run(() => _core.Raw.InstallCheck(_key, _full));
            switch (await Task.Run(() => op.Wait(null)))
            {
                case OrigamiOperationState.Succeeded: flow.SetCheck(OrigamiCheckState.Passed); break;
                case OrigamiOperationState.Failed f: flow.SetCheck(OrigamiCheckState.Failed); _checkFailure = f.Message; break;
                default: flow.SetCheck(OrigamiCheckState.Ready); break;
            }
        }
        catch (Exception e) { flow.SetCheck(OrigamiCheckState.Failed); _checkFailure = e.Message; }
        _check = null;
        if (!_closed) Poll();
    }

    // ---- Prepare the disk to Finish: at the machine --------------------------------------------

    /// <summary>Entering Prepare: the media server first, then the machine.</summary>
    async Task EnterRunAsync()
    {
        _starting = true; // the machine is not up yet, and that is not a stop
        try
        {
            _guide = await Task.Run(() => _core.Raw.InstallGuide(_key));
            if (!_closed && _flow!.Step() >= OrigamiInstallStep.Prepare) ShowStep();
            _serve = await Task.Run(() => _core.Raw.InstallServe(_key));
            await _core.StartAsync(_key);
        }
        catch (Exception e) { if (!_closed) ShowError(e.Message); }
        _starting = false;
        if (!_closed) Poll();
    }

    async Task RestartServerAsync()
    {
        try { _serve = await Task.Run(() => _core.Raw.InstallServe(_key)); }
        catch (Exception e) { ShowError(e.Message); }
        Poll();
    }

    // ---- Method, and the automatic Install packages -------------------------------------------------

    void BuildMethod()
    {
        var flow = _flow!;
        _body.Children.Add(Heading(TitleOf(OrigamiInstallStep.Method)));
        _body.Children.Add(Text("How should the install run?"));
        foreach (var (m, id, name, note) in new[]
        {
            (OrigamiInstallMethod.Automatic, "automatic", "Automatic", "Origami types the package commands for you, logs in when they finish and shuts IRIX down."),
            (OrigamiInstallMethod.Guided, "guided", "Guided", "You type each command in the console. Origami shows each step."),
        })
        {
            var radio = new RadioButton { GroupName = "method", IsChecked = flow.Method() == m, Content = new StackPanel { Children = { Text(name), Text(note, dim: true) } } };
            AutomationProperties.SetAutomationId(radio, "install.method." + id);
            radio.Checked += (_, _) => flow.SetMethod(m);
            _body.Children.Add(radio);
        }
    }

    async Task RunApplyAsync()
    {
        if (_flow is not { } flow) return;
        flow.SetApply(new OrigamiApplyState.Running());
        _applyError = "";
        Poll();
        var kind = OrigamiInstallFailure.Setup; // a refusal typed nothing, so Install stays on offer
        try
        {
            var op = _apply = await Task.Run(() => _core.Raw.InstallApply(_key));
            switch (await Task.Run(() => op.Wait(null)))
            {
                case OrigamiOperationState.Succeeded: flow.SetApply(new OrigamiApplyState.Done()); break;
                case OrigamiOperationState.Failed f:
                    _applyError = f.Message;
                    flow.SetApply(new OrigamiApplyState.Failed(op.InstallFailure() ?? kind));
                    break;
                default: flow.SetApply(new OrigamiApplyState.Failed(op.InstallFailure() ?? OrigamiInstallFailure.Cancelled)); break;
            }
        }
        catch (Exception e)
        {
            _applyError = e.Message;
            flow.SetApply(new OrigamiApplyState.Failed(kind));
        }
        _apply = null;
        if (!_closed) Poll();
    }

    static void Copy(string text)
    {
        var pkg = new Windows.ApplicationModel.DataTransfer.DataPackage();
        pkg.SetText(text);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
    }

    void TypeIt(string send)
    {
        try { _core.ConsoleSend(_key, System.Text.Encoding.UTF8.GetBytes(send)); }
        catch (Exception e) { ShowError(e.Message); }
    }

    async Task FinishAsync()
    {
        var notes = "";
        if (!await BusyAsync(async () => notes = await RestoreNetworkAsync())) return;
        _serve?.Cancel();
        _closeApproved = true;
        Close();
        await _tell(_t.Finished + notes);
    }

    /// <summary>Puts the network back as the core says; its Advice and Warning lines come back as text.</summary>
    async Task<string> RestoreNetworkAsync()
    {
        var lines = await Task.Run(() => _core.Raw.InstallRestoreNetwork(_key));
        return string.Concat(lines.Where(l => l.Kind is OrigamiLineKind.Advice or OrigamiLineKind.Warning).Select(l => "\n" + l.Text));
    }

    // ---- closing -----------------------------------------------------------------------------

    async void OnClosing(Microsoft.UI.Windowing.AppWindow sender, Microsoft.UI.Windowing.AppWindowClosingEventArgs args)
    {
        if (_closeApproved) return;
        args.Cancel = true;
        await TryCloseAsync();
    }

    /// <summary>Close the window after the core's questions; Cancel and the title bar both come here.</summary>
    async Task TryCloseAsync()
    {
        if (_closeApproved || _asking || OpenDialog is not null) return;
        _asking = true;
        try
        {
            if (_flow is not null && !await ConfirmCloseAsync()) return;
            _check?.Cancel();
            _closeApproved = true;
            Close();
        }
        catch (Exception e) { App.Log("install window close failed: " + e.Message); _closeApproved = true; Close(); }
        finally { _asking = false; }
    }

    /// <summary>The core's two close questions in its order; the server stops only after the machine question is answered. False keeps the window.</summary>
    async Task<bool> ConfirmCloseAsync()
    {
        var flow = _flow!;
        var action = flow.CloseMachineQuestion(Up()) is { } q ? await Ask(q) : OrigamiCloseAction.LeaveRunning;
        if (action == OrigamiCloseAction.Stay) return false;
        _serve?.Cancel(); _apply?.Cancel();
        try
        {
            if (action == OrigamiCloseAction.StopMachine)
            {
                await _core.ForceQuitAsync(_key);
                for (var i = 0; i < 40 && Up(); i++) await Task.Delay(500); // the network question needs the machine stopped
            }
        }
        catch (Exception e) { ShowError(e.Message); return false; }
        // The machine may have stopped just now, so ask the core about the network as it is.
        var priv = false;
        try { priv = (await _core.SettingsAsync(_key)).Network.Mode == "private"; } catch { }
        if (flow.CloseNetworkQuestion(Up(), priv) is { } net && await Ask(net) == OrigamiCloseAction.SetBack)
        {
            try { await RestoreNetworkAsync(); }
            catch (Exception e) { ShowError(e.Message); return false; }
        }
        return true;
    }

    /// <summary>Shows the core's question with its buttons: Return picks its default button, the last is what Escape picks.</summary>
    async Task<OrigamiCloseAction> Ask(OrigamiCloseQuestion q)
    {
        var b = q.Buttons;
        // Once the guide is in the pane this window is hidden, so the question goes on the main window.
        var shown = AppWindow.IsVisible;
        var dlg = new ContentDialog
        {
            XamlRoot = shown ? _root.XamlRoot : _host.XamlRootForTest, Title = q.Heading, Content = Text(q.Text),
            PrimaryButtonText = b[0].Label, SecondaryButtonText = b.Length > 2 ? b[1].Label : "", CloseButtonText = b[^1].Label,
            DefaultButton = q.DefaultButton == 0 ? ContentDialogButton.Primary : q.DefaultButton == b.Length - 1 ? ContentDialogButton.Close : ContentDialogButton.Secondary,
        };
        OpenDialog = dlg;
        try
        {
            return (shown ? await dlg.ShowAsync() : await _host.ShowDialogAsync(dlg)) switch
            {
                ContentDialogResult.Primary => b[0].Action,
                ContentDialogResult.Secondary => b[1].Action,
                _ => b[^1].Action,
            };
        }
        finally { OpenDialog = null; }
    }
}
