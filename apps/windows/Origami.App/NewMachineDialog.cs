using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>The New Machine sheet: a model, a name, Create. A failed create stays on the sheet with the core's reason.</summary>
public static class NewMachineDialog
{
    /// <summary>Shows the sheet; returns the new machine's key and whether to open Install IRIX on it, or null when cancelled.</summary>
    public static async Task<(string Key, bool Install)?> ShowAsync(XamlRoot root, ICoreClient core)
    {
        IReadOnlyList<Offering> offerings;
        try { offerings = await Task.Run(core.Catalogue); }
        catch (Exception e) { await Message(root, "Cannot list the machines", e.Message); return null; }
        if (offerings.Count == 0) { await Message(root, "Cannot list the machines", "No engine offers a machine."); return null; }

        var models = NewMachine.Models(offerings);
        var model = new ComboBox { Header = Labels.NewMachine.Model, HorizontalAlignment = HorizontalAlignment.Stretch, ItemsSource = models.Select(NewMachine.Label).ToList(), SelectedIndex = NewMachine.DefaultIndex(models) };
        var engine = new ComboBox { Header = Labels.NewMachine.Engine, HorizontalAlignment = HorizontalAlignment.Stretch };
        IReadOnlyList<string> engines = Array.Empty<string>();
        void SyncEngines()
        {
            engines = model.SelectedIndex < 0 ? Array.Empty<string>() : NewMachine.EnginesFor(offerings, models[model.SelectedIndex].Id);
            engine.ItemsSource = engines.ToList();
            engine.SelectedIndex = engines.Count > 0 ? 0 : -1;
            engine.IsEnabled = engines.Count > 1; // always shown, as on Linux and macOS; one choice is not changeable
        }
        var name = new TextBox { Header = Labels.NewMachine.Name };
        // Install from discs joins this list later; a ready-made disk is added while one fits the model.
        var labels = core.Labels();
        var disk = new ComboBox { Header = Labels.NewMachine.Disk, HorizontalAlignment = HorizontalAlignment.Stretch, ItemsSource = labels.Choices.ToList(), SelectedIndex = DiskChoices.None };
        var release = new ComboBox { Header = labels.Release, HorizontalAlignment = HorizontalAlignment.Stretch, Visibility = Visibility.Collapsed };
        IReadOnlyList<ReadyDisk> ready = Array.Empty<ReadyDisk>();
        var asked = 0;
        async void SyncReady()
        {
            var turn = ++asked;
            var id = model.SelectedIndex < 0 ? null : models[model.SelectedIndex].Id;
            IReadOnlyList<ReadyDisk> found = Array.Empty<ReadyDisk>();
            if (id is not null) { try { found = await core.ReadyDisksAsync(id); } catch (Exception) { } }
            if (turn != asked) return; // a later model choice has asked again
            ready = found;
            var choices = labels.Choices.ToList();
            if (found.Count > 0) choices.Add(labels.Ready);
            var keep = disk.SelectedIndex >= choices.Count ? DiskChoices.None : disk.SelectedIndex;
            disk.ItemsSource = choices; // setting the list clears the selection
            disk.SelectedIndex = keep;
            release.ItemsSource = found.Select(d => d.Label).ToList();
            release.SelectedIndex = found.Count > 0 ? 0 : -1;
        }
        var size = new NumberBox { Header = "Disk size (GB)", Value = core.Labels().DefaultGb, Minimum = 1, Maximum = 2048, Visibility = Visibility.Collapsed };
        var install = new CheckBox { Content = "Install IRIX after creating", Visibility = Visibility.Collapsed };
        AutomationProperties.SetAutomationId(install, "new.install");
        var image = new ImagePanel(core, null);
        image.Root.Visibility = Visibility.Collapsed;
        // Advanced: the machine ID, prefilled with the next free one, and an Ethernet address to use instead of the one it makes.
        var id = new TextBox { Header = "Machine ID (1 to 2999)" };
        var addressShown = new TextBlock();
        var custom = new CheckBox { Content = "Use this address instead" };
        var typed = new TextBox { Header = "Address", PlaceholderText = "08:00:69:__:__:__", Visibility = Visibility.Collapsed };
        var problem = new TextBlock { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed, Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.IndianRed) };
        var advanced = new Expander { Header = Labels.NewMachine.Advanced, HorizontalAlignment = HorizontalAlignment.Stretch, Content = new StackPanel { Spacing = 8, Children = { id, addressShown, custom, typed } } };
        AutomationProperties.SetAutomationId(id, "new.id");
        AutomationProperties.SetAutomationId(addressShown, "new.mac");
        AutomationProperties.SetAutomationId(custom, "new.mac.custom");
        AutomationProperties.SetAutomationId(typed, "new.mac.entry");
        AutomationProperties.SetAutomationId(problem, "new.identity.problem");
        var bar = new ProgressBar { Minimum = 0, Maximum = 1, Visibility = Visibility.Collapsed };
        var line = new TextBlock { Visibility = Visibility.Collapsed };
        var stop = new Button { Content = "Cancel", Visibility = Visibility.Collapsed };
        var copy = new CancellationTokenSource();
        stop.Click += (_, _) => copy.Cancel();
        AutomationProperties.SetAutomationId(disk, "new.disk");
        AutomationProperties.SetAutomationId(size, "new.size");
        AutomationProperties.SetAutomationId(release, "new.release");
        AutomationProperties.SetAutomationId(stop, "new.cancel");
        var error = new TextBlock { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed, Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.IndianRed) };
        AutomationProperties.SetAutomationId(model, "new.model");
        AutomationProperties.SetAutomationId(engine, "new.engine");
        AutomationProperties.SetAutomationId(name, "new.name");
        AutomationProperties.SetAutomationId(error, "new.error");
        var dlg = new ContentDialog
        {
            XamlRoot = root,
            Title = "New Machine",
            Content = new StackPanel { Spacing = 12, Width = 380, Children = { engine, model, name, disk, release, size, install, image.Root, advanced, problem, bar, line, stop, error } },
            PrimaryButtonText = "Create",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            IsPrimaryButtonEnabled = false,
        };
        string? identityProblem = null;
        void Sync() => dlg.IsPrimaryButtonEnabled = NewMachine.CanCreate(name.Text, model.SelectedIndex) && identityProblem is null && (disk.SelectedIndex != DiskChoices.Image || image.Choice.Ready) && (disk.SelectedIndex != DiskChoices.Ready || release.SelectedIndex >= 0);
        int round = 0;
        bool writing = false;
        string? TypedAddress() => custom.IsChecked == true && typed.Text.Trim() is { Length: > 0 } a ? a : null;
        async void RefreshIdentity()
        {
            if (model.SelectedIndex < 0) return;
            var mine = ++round;
            var asked = id.Text;
            IdentityPreview p;
            try { p = await core.PreviewIdentityAsync(models[model.SelectedIndex].Id, engine.SelectedIndex >= 0 ? engines[engine.SelectedIndex] : null, asked, TypedAddress()); }
            catch (Exception e) { if (mine == round) { identityProblem = e.Message; problem.Text = e.Message; problem.Visibility = Visibility.Visible; Sync(); } return; }
            if (mine != round) return;
            // The next free ID fills an empty field, never one the person has started typing.
            if (asked.Trim().Length == 0 && id.Text.Trim().Length == 0) { writing = true; id.Text = p.EmuId.ToString(); writing = false; }
            if (!p.FromId) custom.IsChecked = true;
            custom.IsEnabled = p.FromId;
            addressShown.Text = "Ethernet address: " + (p.Mac.Length > 0 ? p.Mac : "Set by the machine");
            identityProblem = p.Problem;
            problem.Text = identityProblem ?? "";
            problem.Visibility = identityProblem is null ? Visibility.Collapsed : Visibility.Visible;
            Sync();
        }
        id.TextChanged += (_, _) => { if (!writing) RefreshIdentity(); };
        typed.TextChanged += (_, _) => RefreshIdentity();
        custom.Checked += (_, _) => { typed.Visibility = Visibility.Visible; RefreshIdentity(); };
        custom.Unchecked += (_, _) => { typed.Visibility = Visibility.Collapsed; RefreshIdentity(); };
        engine.SelectionChanged += (_, _) => RefreshIdentity();
        name.TextChanged += (_, _) => Sync();
        image.Changed += Sync;
        disk.SelectionChanged += (_, _) =>
        {
            release.Visibility = disk.SelectedIndex == DiskChoices.Ready ? Visibility.Visible : Visibility.Collapsed;
            size.Visibility = disk.SelectedIndex == DiskChoices.Empty ? Visibility.Visible : Visibility.Collapsed;
            SyncInstall();
            image.Root.Visibility = disk.SelectedIndex == DiskChoices.Image ? Visibility.Visible : Visibility.Collapsed;
            Sync();
        };
        model.SelectionChanged += (_, _) => { SyncEngines(); SyncReady(); SyncInstall(); Sync(); RefreshIdentity(); };
        release.SelectionChanged += (_, _) => Sync();
        void SyncInstall() => install.Visibility = disk.SelectedIndex == DiskChoices.Empty && model.SelectedIndex >= 0 && models[model.SelectedIndex].RunsIrix ? Visibility.Visible : Visibility.Collapsed;
        SyncEngines();
        SyncReady();
        RefreshIdentity();

        string? created = null;
        var openInstall = false;
        dlg.PrimaryButtonClick += async (_, e) =>
        {
            var deferral = e.GetDeferral();
            dlg.IsPrimaryButtonEnabled = false; // a second click would create the same name twice
            try
            {
                SystemDisk? system = disk.SelectedIndex switch
                {
                    DiskChoices.Empty => new SystemDisk.Empty((uint)size.Value),
                    DiskChoices.Image => image.Choice.System,
                    DiskChoices.Ready => new SystemDisk.Ready(ready[release.SelectedIndex].Name),
                    _ => null,
                };
                var progress = new Progress<CopyProgress>(p =>
                {
                    bar.Visibility = line.Visibility = stop.Visibility = Visibility.Visible;
                    bar.Value = p.Fraction;
                    line.Text = p.Text;
                });
                openInstall = disk.SelectedIndex == DiskChoices.Empty && install.IsChecked == true;
                created = await core.CreateMachineAsync(name.Text.Trim(), models[model.SelectedIndex].Id, engines[engine.SelectedIndex], copy.Token, system, progress, new IdentityRequest(id.Text, TypedAddress()));
            }
            catch (OperationCanceledException)
            {
                // A cancelled download made nothing: stay on the sheet with a fresh token.
                e.Cancel = true;
                copy = new CancellationTokenSource();
                bar.Visibility = line.Visibility = stop.Visibility = Visibility.Collapsed;
                dlg.IsPrimaryButtonEnabled = true;
            }
            catch (Exception ex)
            {
                e.Cancel = true;
                bar.Visibility = line.Visibility = stop.Visibility = Visibility.Collapsed;
                dlg.IsPrimaryButtonEnabled = true;
                error.Text = ex.Message;
                error.Visibility = Visibility.Visible;
            }
            finally { deferral.Complete(); }
        };
        await dlg.ShowAsync();
        return created is { } key ? (key, openInstall) : null;
    }

    static async Task Message(XamlRoot root, string title, string text) =>
        await new ContentDialog { XamlRoot = root, Title = title, Content = text, CloseButtonText = "OK" }.ShowAsync();
}
