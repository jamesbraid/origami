using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>One machine's settings, as the Linux dialog has them. Edits apply at once and the sheet is rebuilt from the file; a running machine shows them locked.</summary>
public static class MachineSettingsDialog
{
    public static async Task ShowAsync(XamlRoot root, ICoreClient core, string key, string name, Func<IntPtr> hwnd)
    {
        var body = new StackPanel { Spacing = 14, Width = 460 };
        var dlg = new ContentDialog { XamlRoot = root, Title = name + " Settings", Content = new ScrollViewer { Content = body, MaxHeight = 520 }, CloseButtonText = "Close" };
        var note = new TextBlock { TextWrapping = TextWrapping.Wrap, Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.IndianRed), Visibility = Visibility.Collapsed };
        AutomationProperties.SetAutomationId(note, "settings.error");
        List<(string Text, string? Copy)> notes = new(); // advice an edit returned, shown once on the next rebuild
        bool bridgedOpen = false; // Bridged chosen but not applied: the core needs its device with the mode
        ImagePanel? adding = null; // an image being added, shown inline: a ContentDialog cannot open another

        async void ReadBadge(string drive, TextBlock badge)
        {
            try { badge.Text = await core.DriveInfoAsync(key, drive); }
            catch { badge.Visibility = Visibility.Collapsed; } // a running machine's disk that was not read before
        }

        async Task Rebuild()
        {
            MachineSettings s; MachineState state;
            try { s = await core.SettingsAsync(key); state = core.Status(key).State; }
            catch (Exception e) { body.Children.Clear(); body.Children.Add(Text("This machine could not be read: " + e.Message)); return; }
            var ok = MachineSettingsRules.Editable(state);
            // Every edit goes through here: apply off the UI thread, show the core's refusal, else rebuild from the file.
            async void Apply(MachineChange c)
            {
                try { notes = MachineSettingsRules.Notes(await core.ApplyChangeAsync(key, c)); await Rebuild(); }
                catch (Exception e) { note.Text = e.Message; note.Visibility = Visibility.Visible; await Rebuild(); }
            }
            body.Children.Clear();
            if (!ok) body.Children.Add(Text("This machine is running. Hardware settings are locked until it is powered off."));
            body.Children.Add(note);
            foreach (var (text, copy) in notes)
            {
                if (copy is null) body.Children.Add(Text(text));
                else body.Children.Add(Row(text, "Copy", "settings.notecopy", true, () => CopyText(copy)));
            }
            notes = new();

            body.Children.Add(Heading("Processor and memory"));
            body.Children.Add(Text($"{s.Processor} ({s.ProcessorPart}), {s.Nodes} node{(s.Nodes == 1 ? "" : "s")}"));
            body.Children.Add(Combo("settings.cpus", "Processors per node", s.CpusAllowed.Select(c => c.ToString()).ToList(), s.CpusAllowed.ToList().IndexOf(s.CpusPerNode), ok,
                i => Apply(new MachineChange.SetCpus(s.CpusAllowed[i]))));
            body.Children.Add(Combo("settings.memory", "Memory per node", s.MemoryAllowed.Select(m => m + " MiB").ToList(), s.MemoryAllowed.ToList().IndexOf(s.MemoryMiB), ok,
                i => Apply(new MachineChange.SetMemory(s.MemoryAllowed[i]))));

            body.Children.Add(Heading("Startup and firmware"));
            body.Children.Add(Combo("settings.console", "Console serial line", s.ConsoleLines.Select(l => l.Label).ToList(),
                s.ConsoleLines.ToList().FindIndex(l => l.Name == s.ConsolePort), ok, i => Apply(MachineSettingsRules.SetConsoleTo(s.ConsoleLines[i]))));
            body.Children.Add(Text("The Console window and a headless machine's tab use this line. On a machine with graphics, the PROM may print on the screen instead."));

            body.Children.Add(Heading("Graphics"));
            if (s.Pipes.Count == 0) body.Children.Add(Text("None"));
            foreach (var p in s.Pipes) body.Children.Add(Text($"Pipe in slot {p.Slot}: {p.Product} ({p.Family})"));

            body.Children.Add(Heading("Drives"));
            if (s.Drives.Count == 0) body.Children.Add(Text("None"));
            async void Media(string drive, string? image)
            {
                try { await core.ChangeMediaAsync(key, drive, image); await Rebuild(); }
                catch (Exception e) { note.Text = e.Message; note.Visibility = Visibility.Visible; }
            }
            foreach (var d in s.Drives)
            {
                body.Children.Add(Text($"{d.Name} ({d.Kind}, bus {d.Bus} target {d.Target}): {d.Image}{(d.ReadOnly ? " (read-only)" : "")}"));
                var tools = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                if (d.Kind == "disk")
                {
                    var badge = new TextBlock { Text = "Reading…", VerticalAlignment = VerticalAlignment.Center, Opacity = 0.8 };
                    AutomationProperties.SetAutomationId(badge, "settings.badge." + d.Name);
                    tools.Children.Add(badge);
                    ReadBadge(d.Name, badge);
                }
                if (d.Kind == "cdrom")
                {
                    // A CD changes while the machine runs, unlike the rest of the sheet.
                    tools.Children.Add(Btn("Change CD…", "settings.cd.change." + d.Name, true, async () =>
                    {
                        if (await PathPicker.PickFileAsync() is { } p) Media(d.Name, p);
                    }));
                    tools.Children.Add(Btn("Eject", "settings.cd.eject." + d.Name, true, () => { Media(d.Name, null); return Task.CompletedTask; }));
                }
                tools.Children.Add(Btn("Detach", "settings.detach." + d.Name, ok, () => { Apply(new MachineChange.DetachDrive(d.Name)); return Task.CompletedTask; }));
                body.Children.Add(tools);
            }
            int? diskTarget = ok ? s.DiskTarget : null, cdTarget = ok ? s.CdTarget : null;
            var adds = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            adds.Children.Add(Btn("Add a disk image…", "settings.addimage", diskTarget is not null, async () =>
            {
                adding = new ImagePanel(core, key);
                await adding.ChooseAsync();
                if (adding.Choice.Path is null) adding = null;
                await Rebuild();
            }));
            if (!s.Drives.Any(d => d.Kind == "cdrom"))
                adds.Children.Add(Btn("Insert a CD-ROM image…", "settings.insertcd", cdTarget is not null, async () =>
                {
                    if (await PathPicker.PickFileAsync() is { } p) Apply(new MachineChange.AttachCd(p, cdTarget!.Value));
                }));
            body.Children.Add(adds);
            if (adding is { } panel)
            {
                var bar = new ProgressBar { Minimum = 0, Maximum = 1, Visibility = Visibility.Collapsed };
                var line = new TextBlock { Visibility = Visibility.Collapsed };
                var copy = new CancellationTokenSource();
                var stop = new Button { Content = "Cancel", Visibility = Visibility.Collapsed };
                stop.Click += (_, _) => copy.Cancel();
                AutomationProperties.SetAutomationId(stop, "settings.copycancel");
                var go = Btn("Add Disk", "settings.adddisk", panel.Choice.Ready && diskTarget is not null, async () =>
                {
                    var progress = new Progress<CopyProgress>(p => { bar.Visibility = line.Visibility = stop.Visibility = Visibility.Visible; bar.Value = p.Fraction; line.Text = p.Text; });
                    try { await core.ImportDriveAsync(key, panel.Choice.Path!, diskTarget!.Value, panel.Choice.Overlay, panel.Choice.Copy, progress, copy.Token); adding = null; }
                    catch (OperationCanceledException) { adding = null; }
                    catch (Exception e) { note.Text = e.Message; note.Visibility = Visibility.Visible; }
                    await Rebuild();
                });
                panel.Changed += () => go.IsEnabled = panel.Choice.Ready && diskTarget is not null;
                body.Children.Add(Heading("Add a disk image"));
                body.Children.Add(panel.Root);
                body.Children.Add(bar); body.Children.Add(line); body.Children.Add(stop);
                body.Children.Add(go);
            }

            body.Children.Add(Heading("Network"));
            var choices = core.NetworkModes();
            var setup = core.HostBridgedSetup();
            static bool IsHost(string mode) => mode is "tap" or "vmnet";
            var at = choices.ToList().FindIndex(c => c.Mode == s.Network.Mode);
            var bridgedAt = choices.ToList().FindIndex(c => IsHost(c.Mode));
            if (at < 0) body.Children.Add(Text("Currently " + s.Network.Mode));
            // A chosen Bridged shows as selected while its fields are open, before anything is saved.
            var shown = bridgedOpen ? bridgedAt : at;
            body.Children.Add(Combo("settings.network", "Mode", choices.Select(c => c.Label).ToList(), shown, ok, i =>
            {
                bridgedOpen = IsHost(choices[i].Mode);
                if (bridgedOpen || i == at) { _ = Rebuild(); return; }
                Apply(new MachineChange.SetNetwork(choices[i].Mode));
            }));
            if (shown >= 0) body.Children.Add(Text(choices[shown].Detail));
            if (shown == bridgedAt && shown >= 0)
            {
                var device = new TextBox { Header = setup.Field, PlaceholderText = setup.Example, IsEnabled = ok, Text = s.Network.Device };
                var share = new TextBox { Header = "Address for shared folders", PlaceholderText = "ADDRESS/PREFIX", IsEnabled = ok, Text = s.Network.ShareAddress ?? "" };
                AutomationProperties.SetAutomationId(device, "settings.bridge.device");
                AutomationProperties.SetAutomationId(share, "settings.bridge.share");
                body.Children.Add(device);
                body.Children.Add(share);
                var help = new StackPanel { Spacing = 6, Children = { Text(setup.Steps) } };
                if (setup.Commands is { } commands) help.Children.Add(Btn("Copy", "settings.bridge.copy", true, () => { CopyText(commands); return Task.CompletedTask; }));
                body.Children.Add(new Expander { Header = "How to set this up", Content = help });
                body.Children.Add(Btn("Apply", "settings.bridge.apply", ok, () =>
                {
                    Apply(new MachineChange.SetNetwork(choices[bridgedAt].Mode, device.Text, share.Text));
                    return Task.CompletedTask;
                }));
                foreach (var w in s.Network.Warnings) body.Children.Add(Text(w));
            }
            body.Children.Add(Row("Ethernet address: " + (s.Network.Mac ?? "None"), "Copy", "settings.mac.copy", s.Network.Mac is not null, () => CopyText(s.Network.Mac!)));
            body.Children.Add(Text(s.Network.MacNote));
            if (s.Network.Mode is "user" or "private")
            {
                var subnet = new TextBox { Header = "Private subnet (a /24)", IsEnabled = ok, Text = s.Network.Subnet };
                AutomationProperties.SetAutomationId(subnet, "settings.subnet");
                var advanced = new StackPanel { Spacing = 6, Children = { subnet } };
                advanced.Children.Add(Btn("Set", "settings.subnet.apply", ok, () =>
                {
                    Apply(new MachineChange.SetNetwork(s.Network.Mode, "", null, subnet.Text));
                    return Task.CompletedTask;
                }));
                body.Children.Add(new Expander { Header = "Advanced", Content = advanced });
            }

            body.Children.Add(Heading("Port forwards"));
            foreach (var f in s.Forwards)
                body.Children.Add(Row($"{f.Name}: {f.Protocol} host {f.HostPort} to guest {f.GuestPort}", "Remove", "settings.unforward." + f.Name, ok, () => Apply(new MachineChange.RemoveForward(f.Name))));
            var fname = new TextBox { PlaceholderText = "Name", IsEnabled = ok, Width = 120 };
            var proto = new ComboBox { ItemsSource = new[] { "tcp", "udp" }, SelectedIndex = 0, IsEnabled = ok };
            var host = new NumberBox { Header = "Host port", Value = 2222, Minimum = 1, Maximum = 65535, IsEnabled = ok };
            var guest = new NumberBox { Header = "Guest port", Value = 22, Minimum = 1, Maximum = 65535, IsEnabled = ok };
            var add = new Button { Content = "Add forward", IsEnabled = false };
            void SyncAdd() => add.IsEnabled = ok && MachineSettingsRules.CanAddForward(fname.Text, (int)host.Value, (int)guest.Value);
            fname.TextChanged += (_, _) => SyncAdd();
            host.ValueChanged += (_, _) => SyncAdd();
            guest.ValueChanged += (_, _) => SyncAdd();
            add.Click += (_, _) => Apply(new MachineChange.AddForward(new Forward(fname.Text.Trim(), (string)proto.SelectedItem, (int)host.Value, (int)guest.Value)));
            AutomationProperties.SetAutomationId(add, "settings.addforward");
            body.Children.Add(new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { fname, proto, host, guest } });
            body.Children.Add(add);

            body.Children.Add(Heading("Shared folders"));
            foreach (var sh in s.Shares)
                body.Children.Add(Row($"{sh.Name}: {sh.HostDir}", "Stop sharing", "settings.unshare." + sh.Name, ok, () => Apply(new MachineChange.RemoveShare(sh.Name))));
            var pick = new Button { Content = "Share a folder…", IsEnabled = ok };
            AutomationProperties.SetAutomationId(pick, "settings.share");
            pick.Click += async (_, _) =>
            {
                var picker = new Windows.Storage.Pickers.FolderPicker();
                picker.FileTypeFilter.Add("*");
                WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd());
                if (await picker.PickSingleFolderAsync() is { } folder)
                    Apply(new MachineChange.AddShare(new Share(MachineSettingsRules.ShareNameFor(folder.Path), folder.Path)));
            };
            body.Children.Add(pick);
        }

        await Rebuild();
        await dlg.ShowAsync();
    }

    static void CopyText(string text)
    {
        var pkg = new Windows.ApplicationModel.DataTransfer.DataPackage();
        pkg.SetText(text);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
    }

    static Button Btn(string text, string id, bool enabled, Func<Task> click)
    {
        var b = new Button { Content = text, IsEnabled = enabled };
        AutomationProperties.SetAutomationId(b, id);
        b.Click += async (_, _) => await click();
        return b;
    }

    static TextBlock Text(string t) => new() { Text = t, TextWrapping = TextWrapping.Wrap };
    static TextBlock Heading(string t) => new() { Text = t, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Margin = new Thickness(0, 6, 0, 0) };

    static ComboBox Combo(string id, string header, List<string> items, int selected, bool enabled, Action<int> changed)
    {
        var c = new ComboBox { Header = header, ItemsSource = items, SelectedIndex = selected, IsEnabled = enabled, HorizontalAlignment = HorizontalAlignment.Stretch };
        AutomationProperties.SetAutomationId(c, id);
        // Rebuilding sets the selection first, so only a later change is an edit.
        c.SelectionChanged += (_, _) => { if (c.SelectedIndex >= 0 && c.SelectedIndex != selected) changed(c.SelectedIndex); };
        return c;
    }

    static Grid Row(string text, string button, string id, bool enabled, Action click)
    {
        var b = new Button { Content = button, IsEnabled = enabled };
        AutomationProperties.SetAutomationId(b, id);
        b.Click += (_, _) => click();
        var g = new Grid { ColumnDefinitions = { new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }, new ColumnDefinition { Width = GridLength.Auto } } };
        g.Children.Add(Text(text));
        Grid.SetColumn(b, 1);
        g.Children.Add(b);
        return g;
    }
}
