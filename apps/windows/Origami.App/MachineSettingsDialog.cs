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

        async Task Rebuild()
        {
            MachineSettings s; MachineState state;
            try { s = await core.SettingsAsync(key); state = core.Status(key).State; }
            catch (Exception e) { body.Children.Clear(); body.Children.Add(Text("This machine could not be read: " + e.Message)); return; }
            var ok = MachineSettingsRules.Editable(state);
            // Every edit goes through here: apply off the UI thread, show the core's refusal, else rebuild from the file.
            async void Apply(MachineChange c)
            {
                try { await core.ApplyChangeAsync(key, c); await Rebuild(); }
                catch (Exception e) { note.Text = e.Message; note.Visibility = Visibility.Visible; await Rebuild(); }
            }
            body.Children.Clear();
            if (!ok) body.Children.Add(Text("This machine is running. Hardware settings are locked until it is powered off."));
            body.Children.Add(note);

            body.Children.Add(Heading("Processor and memory"));
            body.Children.Add(Text($"{s.Processor} ({s.ProcessorPart}), {s.Nodes} node{(s.Nodes == 1 ? "" : "s")}"));
            body.Children.Add(Combo("settings.cpus", "Processors per node", s.CpusAllowed.Select(c => c.ToString()).ToList(), s.CpusAllowed.ToList().IndexOf(s.CpusPerNode), ok,
                i => Apply(new MachineChange.SetCpus(s.CpusAllowed[i]))));
            body.Children.Add(Combo("settings.memory", "Memory per node", s.MemoryAllowed.Select(m => m + " MiB").ToList(), s.MemoryAllowed.ToList().IndexOf(s.MemoryMiB), ok,
                i => Apply(new MachineChange.SetMemory(s.MemoryAllowed[i]))));

            body.Children.Add(Heading("Graphics"));
            if (s.Pipes.Count == 0) body.Children.Add(Text("None"));
            foreach (var p in s.Pipes) body.Children.Add(Text($"Pipe in slot {p.Slot}: {p.Product} ({p.Family})"));

            body.Children.Add(Heading("Drives"));
            if (s.Drives.Count == 0) body.Children.Add(Text("None"));
            foreach (var d in s.Drives)
                body.Children.Add(Row($"{d.Name} ({d.Kind}, bus {d.Bus} target {d.Target}): {d.Image}{(d.ReadOnly ? " (read-only)" : "")}", "Detach", "settings.detach." + d.Name, ok, () => Apply(new MachineChange.DetachDrive(d.Name))));

            body.Children.Add(Heading("Network"));
            var modes = MachineSettingsRules.NetworkModes;
            var at = Array.FindIndex(modes, m => m.Mode == s.NetworkMode);
            // tap and vmnet need an endpoint this sheet does not ask for.
            if (at < 0) body.Children.Add(Text("Currently " + s.NetworkMode));
            body.Children.Add(Combo("settings.network", "Mode", modes.Select(m => m.Label).ToList(), at, ok && at >= 0, i => Apply(new MachineChange.SetNetwork(modes[i].Mode))));

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
