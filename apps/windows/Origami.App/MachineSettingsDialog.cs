using uniffi.origami_ffi;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>One machine's settings as pages: a navigation list of eight, each laid out as Fluent settings rows. Edits apply at once and the page is rebuilt
/// from the file; a running machine shows its hardware pages locked, with the core's line and a Power off button.</summary>
public static class MachineSettingsDialog
{
    /// <summary>The pages in the order every app lists them; the words are the core's.</summary>
    public static IReadOnlyList<(string Id, string Title, string Glyph)> Pages() => new[]
    {
        ("general", Ui.T(OrigamiUiString.General), ""),
        ("processor", Labels.Settings.ProcessorMemory, ""),
        ("graphics", Labels.Settings.Graphics, ""),
        ("drives", Labels.Settings.Drives, ""),
        ("network", Labels.Settings.Network, ""),
        ("shares", Labels.Settings.SharedFolders, ""),
        ("startup", Labels.Settings.StartupFirmware, ""),
        ("input", Ui.T(OrigamiUiString.Input), ""),
    };

    /// <summary>The open dialog's page list, for the self-test.</summary>
    public static NavigationView? Current { get; private set; }

    public static async Task ShowAsync(MainWindow host, ICoreClient core, string key, string name, string? page = null)
    {
        var body = new StackPanel { Spacing = 8 };
        var locked = new Grid { ColumnSpacing = 12, Visibility = Visibility.Collapsed, Padding = new Thickness(24, 8, 24, 8) };
        locked.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        locked.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var lockText = new TextBlock { Text = Ui.T(OrigamiUiString.PowerOffToChangeHardwareSettings), VerticalAlignment = VerticalAlignment.Center, TextWrapping = TextWrapping.Wrap, Foreground = Ui.Secondary };
        var lockOff = new Button { Content = Ui.T(OrigamiUiString.PowerOff) };
        AutomationProperties.SetAutomationId(lockText, "settings.locked");
        AutomationProperties.SetAutomationId(lockOff, "settings.locked.poweroff");
        Grid.SetColumn(lockOff, 1);
        locked.Children.Add(lockText); locked.Children.Add(lockOff);
        var note = new InfoBar { IsOpen = false, IsClosable = true, Severity = InfoBarSeverity.Error };
        AutomationProperties.SetAutomationId(note, "settings.error");

        var nav = new NavigationView
        {
            PaneDisplayMode = NavigationViewPaneDisplayMode.Left, IsBackButtonVisible = NavigationViewBackButtonVisible.Collapsed,
            IsSettingsVisible = false, IsPaneToggleButtonVisible = false, OpenPaneLength = 220, Height = 440, Width = 720,
        };
        foreach (var (id, title, glyph) in Pages())
        {
            var item = new NavigationViewItem { Content = title, Tag = id, Icon = new FontIcon { Glyph = glyph } };
            AutomationProperties.SetAutomationId(item, "settings.page." + id);
            nav.MenuItems.Add(item);
        }
        var scroll = new ScrollViewer { Content = body, Padding = new Thickness(24, 8, 24, 8), VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        var shell = new Grid { RowDefinitions = { new RowDefinition { Height = new GridLength(1, GridUnitType.Star) }, new RowDefinition { Height = GridLength.Auto } } };
        shell.Children.Add(scroll);
        Grid.SetRow(locked, 1);
        shell.Children.Add(locked);
        nav.Content = shell;
        var dlg = new ContentDialog { XamlRoot = host.XamlRootForTest, Title = name, Content = nav, CloseButtonText = Ui.T(OrigamiUiString.Done), DefaultButton = ContentDialogButton.Close };
        dlg.Resources["ContentDialogMaxWidth"] = 760.0;

        List<(string Text, string? Copy)> notes = new(); // advice an edit returned, shown once on the next rebuild
        (string Dir, string Name)? sharing = null; // a folder picked and waiting for its name
        bool bridgedOpen = false; // Bridged chosen but not applied: the core needs its device with the mode
        ImagePanel? adding = null; // an image being added, shown inline: a ContentDialog cannot open another

        string PageNow() => (nav.SelectedItem as NavigationViewItem)?.Tag as string ?? "general";
        void ShowNote(string text) { note.Message = text; note.IsOpen = true; }

        async Task Rebuild()
        {
            MachineSettings s; MachineState state;
            try { s = await core.SettingsAsync(key); state = core.Status(key).State; }
            catch (Exception e) { body.Children.Clear(); body.Children.Add(Text(core.ProblemOf(e).Message)); return; }
            var ok = MachineSettingsRules.Editable(state);
            // Every edit goes through here: apply off the UI thread, show the core's refusal, else rebuild from the file.
            async void Apply(MachineChange c)
            {
                try { notes = MachineSettingsRules.Notes(await core.ApplyChangeAsync(key, c)); await Rebuild(); }
                catch (Exception e) { ShowNote(core.ProblemOf(e).Message); await Rebuild(); }
            }
            body.Children.Clear();
            var page = PageNow();
            locked.Visibility = !ok && page is "processor" or "drives" or "network" or "shares" or "startup" ? Visibility.Visible : Visibility.Collapsed;
            body.Children.Add(note);
            foreach (var (text, copy) in notes)
            {
                if (copy is null) body.Children.Add(Text(text));
                else body.Children.Add(Row(text, null, Button(Ui.T(OrigamiUiString.Copy), "settings.notecopy", true, () => { CopyText(copy); return Task.CompletedTask; })));
            }
            notes = new();

            switch (page)
            {
                case "general":
                    body.Children.Add(Row(Ui.T(OrigamiUiString.Name), null, Value(name)));
                    foreach (var c in core.Machines().FirstOrDefault(m => m.Key == key)?.Cards ?? new List<SummaryCard>())
                        body.Children.Add(Row(c.Title, null, Value(c.Value)));
                    break;
                case "processor":
                    body.Children.Add(Row(Ui.T(OrigamiUiString.Processor), null, Value($"{s.Processor} {s.ProcessorPart}")));
                    if (s.Nodes > 1) body.Children.Add(Row(Ui.T(OrigamiUiString.Nodes), null, Value(s.Nodes.ToString())));
                    body.Children.Add(Row(Ui.T(OrigamiUiString.ProcessorsPerNode), null, Combo("settings.cpus", s.CpusAllowed.Select(c => c.ToString()).ToList(), s.CpusAllowed.ToList().IndexOf(s.CpusPerNode), ok,
                        i => Apply(new MachineChange.SetCpus(s.CpusAllowed[i])))));
                    body.Children.Add(Row(Ui.T(OrigamiUiString.MemoryPerNode), null, Combo("settings.memory", s.MemoryAllowed.Select(m => m + " MiB").ToList(), s.MemoryAllowed.ToList().IndexOf(s.MemoryMiB), ok,
                        i => Apply(new MachineChange.SetMemory(s.MemoryAllowed[i])))));
                    break;
                case "graphics":
                    foreach (var p in s.Pipes) body.Children.Add(Row($"{Ui.T(OrigamiUiString.Graphics)} {p.Slot}", null, Value($"{p.Product} ({p.Family})")));
                    break;
                case "drives":
                    await DrivesPage(s, ok, Apply);
                    break;
                case "network":
                    NetworkPage(s, ok, Apply);
                    break;
                case "shares":
                    SharesPage(s, ok, Apply);
                    break;
                case "startup":
                    {
                        body.Children.Add(Row(Ui.T(OrigamiUiString.ConsoleSerialLine), Ui.T(OrigamiUiString.TheConsoleWindowAndA), Combo("settings.console", s.ConsoleLines.Select(l => l.Label).ToList(),
                            s.ConsoleLines.ToList().FindIndex(l => l.Name == s.ConsolePort), ok, i => Apply(MachineSettingsRules.SetConsoleTo(s.ConsoleLines[i])))));
                        var row = Row(Ui.T(OrigamiUiString.StartupMessagesOn), null, Value(await core.StartupValueAsync(key), "settings.startup"));
                        body.Children.Add(row);
                        break;
                    }
                case "input":
                    {
                        var modes = new RadioButtons { Header = Ui.T(OrigamiUiString.Pointer) };
                        modes.Items.Add(Ui.T(OrigamiUiString.FollowMyMouse));
                        modes.Items.Add(Ui.T(OrigamiUiString.CaptureOnClick));
                        modes.SelectedIndex = host.PointerModeFor(key) == PointerMode.Capture ? 1 : 0;
                        AutomationProperties.SetAutomationId(modes, "settings.pointer");
                        modes.SelectionChanged += (_, _) => host.SetPointerMode(key, modes.SelectedIndex == 1 ? PointerMode.Capture : PointerMode.Follow);
                        body.Children.Add(Card(modes));
                        var release = OrigamiFfiMethods.OrigamiShortcutGroups(OrigamiPlatform.Windows).First().Items.First();
                        body.Children.Add(Row(release.Label, null, Value(release.Keys)));
                        break;
                    }
            }
        }

        // ---- the pages with more than rows ----

        async Task DrivesPage(MachineSettings s, bool ok, Action<MachineChange> apply)
        {
            IReadOnlyList<DriveRow> rows = Array.Empty<DriveRow>();
            try { rows = await core.DriveRowsAsync(key); } catch (Exception e) { ShowNote(core.ProblemOf(e).Message); }
            async void Media(string drive, string? image)
            {
                try { await core.ChangeMediaAsync(key, drive, image); await Rebuild(); }
                catch (Exception e) { ShowNote(core.ProblemOf(e).Message); }
            }
            foreach (var r in rows)
            {
                var kind = s.Drives.FirstOrDefault(d => d.Name == r.Name)?.Kind;
                var tools = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                if (kind == "cdrom")
                {
                    // A CD changes while the machine runs, unlike the rest of the page.
                    tools.Children.Add(Button(Ui.T(OrigamiUiString.ChangeCd), "settings.cd.change." + r.Name, true, async () => { if (await PathPicker.PickFileAsync() is { } p) Media(r.Name, p); }));
                    tools.Children.Add(Button(Ui.T(OrigamiUiString.Eject), "settings.cd.eject." + r.Name, true, () => { Media(r.Name, null); return Task.CompletedTask; }));
                }
                tools.Children.Add(Button(Ui.T(OrigamiUiString.Detach), "settings.detach." + r.Name, ok, () => { apply(new MachineChange.DetachDrive(r.Name)); return Task.CompletedTask; }));
                body.Children.Add(Row(r.Title, r.Caption, tools));
            }
            int? diskTarget = ok ? s.DiskTarget : null, cdTarget = ok ? s.CdTarget : null;
            var adds = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            adds.Children.Add(Button(Ui.T(OrigamiUiString.AddADiskImage), "settings.addimage", diskTarget is not null, async () =>
            {
                var picked = new ImagePanel(core, key);
                await picked.ChooseAsync();
                adding = picked.Choice.Path is null ? null : picked;
                await Rebuild();
            }));
            if (!s.Drives.Any(d => d.Kind == "cdrom"))
                adds.Children.Add(Button(Ui.T(OrigamiUiString.InsertACdImage), "settings.insertcd", cdTarget is not null, async () =>
                {
                    if (await PathPicker.PickFileAsync() is { } p) apply(new MachineChange.AttachCd(p, cdTarget!.Value));
                }));
            body.Children.Add(adds);
            if (adding is { } panel)
            {
                var bar = new ProgressBar { Minimum = 0, Maximum = 1, Visibility = Visibility.Collapsed };
                var line = new TextBlock { Visibility = Visibility.Collapsed };
                var copy = new CancellationTokenSource();
                var stop = new Button { Content = Ui.T(OrigamiUiString.Cancel), Visibility = Visibility.Collapsed };
                stop.Click += (_, _) => copy.Cancel();
                AutomationProperties.SetAutomationId(stop, "settings.copycancel");
                var go = Button("Add disk", "settings.adddisk", panel.Choice.Ready && diskTarget is not null, async () =>
                {
                    var progress = new Progress<CopyProgress>(p => { bar.Visibility = line.Visibility = stop.Visibility = Visibility.Visible; bar.Value = p.Fraction; line.Text = p.Text; });
                    try { await core.ImportDriveAsync(key, panel.Choice.Path!, diskTarget!.Value, panel.Choice.Overlay, panel.Choice.Copy, progress, copy.Token); adding = null; }
                    catch (OperationCanceledException) { adding = null; }
                    catch (Exception e) { ShowNote(core.ProblemOf(e).Message); }
                    await Rebuild();
                });
                panel.Changed += () => go.IsEnabled = panel.Choice.Ready && diskTarget is not null;
                body.Children.Add(Heading("Add a disk image"));
                body.Children.Add(panel.Root);
                body.Children.Add(bar); body.Children.Add(line); body.Children.Add(stop);
                body.Children.Add(go);
            }
        }

        void NetworkPage(MachineSettings s, bool ok, Action<MachineChange> apply)
        {
            var choices = core.NetworkModes();
            var setup = core.HostBridgedSetup();
            static bool IsHost(string mode) => mode is "tap" or "vmnet";
            var at = choices.ToList().FindIndex(c => c.Mode == s.Network.Mode);
            var bridgedAt = choices.ToList().FindIndex(c => IsHost(c.Mode));
            // A chosen Bridged shows as selected while its fields are open, before anything is saved.
            var shown = bridgedOpen ? bridgedAt : at;
            var modes = new RadioButtons { Header = Ui.T(OrigamiUiString.Mode), IsEnabled = ok };
            foreach (var c in choices)
                modes.Items.Add(new StackPanel { Children = { new TextBlock { Text = c.Label }, new TextBlock { Text = c.Detail, TextWrapping = TextWrapping.Wrap, Foreground = Ui.Secondary, Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"] } } });
            modes.SelectedIndex = shown;
            AutomationProperties.SetAutomationId(modes, "settings.network");
            // Rebuilding sets the selection first, so only a later change is an edit.
            modes.SelectionChanged += (_, _) =>
            {
                var i = modes.SelectedIndex;
                if (i < 0 || i == shown) return;
                bridgedOpen = IsHost(choices[i].Mode);
                if (bridgedOpen || i == at) { _ = Rebuild(); return; }
                apply(new MachineChange.SetNetwork(choices[i].Mode));
            };
            body.Children.Add(Card(modes));
            if (shown == bridgedAt && shown >= 0)
            {
                var device = new TextBox { Header = setup.Field, PlaceholderText = setup.Example, IsEnabled = ok, Text = s.Network.Device };
                var share = new TextBox { Header = Ui.T(OrigamiUiString.AddressForSharedFolders), PlaceholderText = "ADDRESS/PREFIX", IsEnabled = ok, Text = s.Network.ShareAddress ?? "" };
                AutomationProperties.SetAutomationId(device, "settings.bridge.device");
                AutomationProperties.SetAutomationId(share, "settings.bridge.share");
                var help = new StackPanel { Spacing = 6, Children = { Text(setup.Steps) } };
                if (setup.Commands is { } commands) help.Children.Add(Button(Ui.T(OrigamiUiString.Copy), "settings.bridge.copy", true, () => { CopyText(commands); return Task.CompletedTask; }));
                // One edit sets the mode, the device and the share address, so the fields apply when the person leaves them.
                string applied = s.Network.Device + "|" + (s.Network.ShareAddress ?? "");
                void Commit()
                {
                    if (!ok || device.Text.Trim().Length == 0 || device.Text + "|" + share.Text == applied) return;
                    applied = device.Text + "|" + share.Text;
                    apply(new MachineChange.SetNetwork(choices[bridgedAt].Mode, device.Text, share.Text));
                }
                foreach (var box in new[] { device, share })
                {
                    box.LostFocus += (_, _) => Commit();
                    box.KeyDown += (_, e) => { if (e.Key == Windows.System.VirtualKey.Enter) { Commit(); e.Handled = true; } };
                }
                var fields = new StackPanel { Spacing = 8, Children = { device, share, new Expander { Header = Ui.T(OrigamiUiString.HowToSetThisUp), Content = help, HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch } } };
                body.Children.Add(Card(fields));
                foreach (var w in s.Network.Warnings) body.Children.Add(Text(w));
            }
            var mac = s.Network.Mac;
            var copyMac = mac is null ? null : Button(Ui.T(OrigamiUiString.Copy), "settings.mac.copy", true, () => { CopyText(mac); return Task.CompletedTask; });
            var macTools = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { Value(mac ?? "", "settings.mac") } };
            if (copyMac is not null) macTools.Children.Add(copyMac);
            body.Children.Add(Row(Ui.T(OrigamiUiString.EthernetAddress), s.Network.MacNote, macTools));
            if (s.Network.Mode is "user" or "private")
            {
                var subnet = new TextBox { Header = "Private subnet (a /24)", IsEnabled = ok, Text = s.Network.Subnet };
                AutomationProperties.SetAutomationId(subnet, "settings.subnet");
                var advanced = new StackPanel { Spacing = 6, Children = { subnet, Button("Set", "settings.subnet.apply", ok, () => { apply(new MachineChange.SetNetwork(s.Network.Mode, "", null, subnet.Text)); return Task.CompletedTask; }) } };
                body.Children.Add(new Expander { Header = Ui.T(OrigamiUiString.Advanced), Content = advanced, HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch });
            }

            body.Children.Add(Heading(Ui.T(OrigamiUiString.PortForwards)));
            foreach (var f in s.Forwards)
                body.Children.Add(Row($"{f.Name}: {f.Protocol} host {f.HostPort} to guest {f.GuestPort}", null, Button(Ui.T(OrigamiUiString.Remove), "settings.unforward." + f.Name, ok, () => { apply(new MachineChange.RemoveForward(f.Name)); return Task.CompletedTask; })));
            var fname = new TextBox { PlaceholderText = Ui.T(OrigamiUiString.Name), IsEnabled = ok, Width = 120 };
            var proto = new ComboBox { ItemsSource = new[] { "tcp", "udp" }, SelectedIndex = 0, IsEnabled = ok };
            var host_ = new NumberBox { Header = Ui.T(OrigamiUiString.HostPort), Value = 2222, Minimum = 1, Maximum = 65535, IsEnabled = ok };
            var guest = new NumberBox { Header = Ui.T(OrigamiUiString.GuestPort), Value = 22, Minimum = 1, Maximum = 65535, IsEnabled = ok };
            var add = new Button { Content = Ui.T(OrigamiUiString.AddAPortForward), IsEnabled = false };
            void SyncAdd() => add.IsEnabled = ok && MachineSettingsRules.CanAddForward(fname.Text, (int)host_.Value, (int)guest.Value);
            fname.TextChanged += (_, _) => SyncAdd();
            host_.ValueChanged += (_, _) => SyncAdd();
            guest.ValueChanged += (_, _) => SyncAdd();
            add.Click += (_, _) => apply(new MachineChange.AddForward(new Forward(fname.Text.Trim(), (string)proto.SelectedItem, (int)host_.Value, (int)guest.Value)));
            AutomationProperties.SetAutomationId(add, "settings.addforward");
            body.Children.Add(Card(new StackPanel { Spacing = 8, Children = { new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { fname, proto, host_, guest } }, add } }));
        }

        void SharesPage(MachineSettings s, bool ok, Action<MachineChange> apply)
        {
            var empty = OrigamiFfiMethods.OrigamiEmptyState(OrigamiEmptyKind.NoSharedFolders);
            if (s.Shares.Count == 0 && sharing is null)
            {
                body.Children.Add(new TextBlock { Text = empty.Title, Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"], HorizontalAlignment = HorizontalAlignment.Center, Margin = new Thickness(0, 24, 0, 0) });
                body.Children.Add(new TextBlock { Text = empty.Sentence, Foreground = Ui.Secondary, HorizontalAlignment = HorizontalAlignment.Center });
            }
            foreach (var sh in s.Shares)
            {
                body.Children.Add(Row(sh.Name, sh.HostDir, Button(Ui.T(OrigamiUiString.StopSharing), "settings.unshare." + sh.Name, ok, () => { apply(new MachineChange.RemoveShare(sh.Name)); return Task.CompletedTask; })));
                if (sh.MountProblem is { } reason)
                {
                    body.Children.Add(Text(reason));
                    if (sh.NeedsAddress) body.Children.Add(Button(Ui.T(OrigamiUiString.SetTheAddressInNetwork), "settings.share.network", true, () => { nav.SelectedItem = nav.MenuItems.OfType<NavigationViewItem>().First(i => (string)i.Tag == "network"); return Task.CompletedTask; }));
                }
                else if (sh.MountLines is { Count: > 0 } lines)
                {
                    var mount = string.Join("\n", lines);
                    var code = new TextBlock { Text = mount, FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Cascadia Mono, Consolas"), IsTextSelectionEnabled = true };
                    AutomationProperties.SetAutomationId(code, "settings.mount." + sh.Name);
                    body.Children.Add(Card(new StackPanel { Spacing = 6, Children = { code, Button(Ui.T(OrigamiUiString.Copy), "settings.mount.copy." + sh.Name, true, () => { CopyText(mount); return Task.CompletedTask; }) } }));
                }
            }
            var pick = new Button { Content = empty.Button, IsEnabled = ok };
            AutomationProperties.SetAutomationId(pick, "settings.share");
            pick.Click += async (_, _) =>
            {
                if (await PathPicker.PickFolderAsync() is { } folder) { sharing = (folder, core.SuggestShareName(folder)); await Rebuild(); }
            };
            body.Children.Add(pick);
            body.Children.Add(Text(Ui.T(OrigamiUiString.RunTheseAsRootIn)));
            if (sharing is { } draft)
            {
                // The name is asked inline: a ContentDialog cannot open another.
                var nameBox = new TextBox { Header = Ui.T(OrigamiUiString.NameInIrix), Text = draft.Name };
                var why = new TextBlock { TextWrapping = TextWrapping.Wrap, Foreground = Ui.ToneBrush(Tone.Critical) };
                var addShare = Button(Ui.T(OrigamiUiString.Add), "settings.share.add", ok, () =>
                {
                    sharing = null;
                    apply(new MachineChange.AddShare(new Share(nameBox.Text, draft.Dir)));
                    return Task.CompletedTask;
                });
                void Check() { var p = core.CheckShareName(nameBox.Text); why.Text = p ?? ""; why.Visibility = p is null ? Visibility.Collapsed : Visibility.Visible; addShare.IsEnabled = ok && p is null; }
                AutomationProperties.SetAutomationId(nameBox, "settings.share.name");
                AutomationProperties.SetAutomationId(why, "settings.share.problem");
                nameBox.TextChanged += (_, _) => Check();
                Check();
                body.Children.Add(Heading(Ui.T(OrigamiUiString.ShareAFolder)));
                body.Children.Add(Card(new StackPanel { Spacing = 8, Children = { nameBox, Text(draft.Dir), why, new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { addShare, Button(Ui.T(OrigamiUiString.Cancel), "settings.share.cancel", true, async () => { sharing = null; await Rebuild(); }) } } } }));
            }
        }

        nav.SelectionChanged += async (_, _) => { adding = null; sharing = null; note.IsOpen = false; await Rebuild(); };
        lockOff.Click += (_, _) => { dlg.Hide(); host.DispatcherQueue.TryEnqueue(() => host.Invoke("machine.stop")); };
        var want = page ?? "general";
        nav.SelectedItem = nav.MenuItems.OfType<NavigationViewItem>().FirstOrDefault(i => (string)i.Tag == want) ?? (NavigationViewItem)nav.MenuItems[0];
        await Rebuild();
        Current = nav;
        try { await host.ShowDialogAsync(dlg); }
        finally { Current = null; }
    }

    // ---- Fluent settings rows ----

    /// <summary>A card: the control on the card fill with the system card border, as a Settings app group is drawn.</summary>
    static Border Card(UIElement content) => new()
    {
        Child = content, Padding = new Thickness(16, 12, 16, 12), CornerRadius = new CornerRadius(Tokens.Radius.Control),
        Background = Ui.CardFill, BorderBrush = Ui.CardStroke, BorderThickness = new Thickness(1),
    };

    /// <summary>A settings row: the title and an optional caption on the left, the control on the right.</summary>
    static Border Row(string title, string? caption, FrameworkElement control)
    {
        var text = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center, Children = { new TextBlock { Text = title, TextWrapping = TextWrapping.Wrap } } };
        if (!string.IsNullOrEmpty(caption))
            text.Children.Add(new TextBlock { Text = caption, TextWrapping = TextWrapping.Wrap, Foreground = Ui.Secondary, Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"] });
        var g = new Grid { ColumnSpacing = 16, ColumnDefinitions = { new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }, new ColumnDefinition { Width = GridLength.Auto } } };
        g.Children.Add(text);
        Grid.SetColumn(control, 1);
        control.VerticalAlignment = VerticalAlignment.Center;
        g.Children.Add(control);
        return Card(g);
    }

    static TextBlock Value(string text, string? id = null)
    {
        var t = new TextBlock { Text = text, Foreground = Ui.Secondary, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap, MaxWidth = 300, TextAlignment = TextAlignment.Right, VerticalAlignment = VerticalAlignment.Center };
        if (id is not null) AutomationProperties.SetAutomationId(t, id);
        return t;
    }

    static void CopyText(string text)
    {
        var pkg = new Windows.ApplicationModel.DataTransfer.DataPackage();
        pkg.SetText(text);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
    }

    static Button Button(string text, string id, bool enabled, Func<Task> click)
    {
        var b = new Button { Content = text, IsEnabled = enabled };
        AutomationProperties.SetAutomationId(b, id);
        b.Click += async (_, _) => await click();
        return b;
    }

    static TextBlock Text(string t) => new() { Text = t, TextWrapping = TextWrapping.Wrap };

    static TextBlock Heading(string t) => new() { Text = t, Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"], Margin = new Thickness(0, 12, 0, 0) };

    static ComboBox Combo(string id, List<string> items, int selected, bool enabled, Action<int> changed)
    {
        var c = new ComboBox { ItemsSource = items, SelectedIndex = selected, IsEnabled = enabled, MinWidth = 160 };
        AutomationProperties.SetAutomationId(c, id);
        // Rebuilding sets the selection first, so only a later change is an edit.
        c.SelectionChanged += (_, _) => { if (c.SelectedIndex >= 0 && c.SelectedIndex != selected) changed(c.SelectedIndex); };
        return c;
    }
}
