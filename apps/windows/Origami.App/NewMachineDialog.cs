using uniffi.origami_ffi;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>The New machine dialog: Machine, Disk and Advanced groups, headers above their controls, each error under its own field. A failed create stays on the dialog with the core's reason.</summary>
public static class NewMachineDialog
{
    static TextBlock Heading(string t) => new() { Text = t, Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"], Margin = new Thickness(0, 8, 0, 0) };

    static TextBlock Caption(string id, string text = "") => Note(id, text, Ui.Secondary);

    static TextBlock Note(string id, string text, Microsoft.UI.Xaml.Media.Brush brush)
    {
        var t = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, Foreground = brush, Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"], Visibility = text.Length > 0 ? Visibility.Visible : Visibility.Collapsed };
        AutomationProperties.SetAutomationId(t, id);
        return t;
    }

    static void Show(TextBlock t, string? text) { t.Text = text ?? ""; t.Visibility = string.IsNullOrEmpty(text) ? Visibility.Collapsed : Visibility.Visible; }

    /// <summary>Shows the dialog; returns the new machine's key and whether to open Install IRIX on it, or null when cancelled. <paramref name="image"/> is a disk image dropped on the library, chosen at the start.</summary>
    public static async Task<(string Key, bool Install)?> ShowAsync(MainWindow host, ICoreClient core, string? image = null)
    {
        IReadOnlyList<Offering> offerings;
        try { offerings = await Task.Run(core.Catalogue); }
        catch (Exception e) { await Message(host, "Cannot list the machines", e.Message); return null; }
        if (offerings.Count == 0) { await Message(host, "Cannot list the machines", "No engine offers a machine."); return null; }

        var models = NewMachine.Models(offerings);
        var model = new ComboBox { Header = Labels.NewMachine.Model, HorizontalAlignment = HorizontalAlignment.Stretch, ItemsSource = models.Select(NewMachine.Label).ToList(), SelectedIndex = core.DefaultOffering(models) };
        var engine = new ComboBox { Header = Labels.NewMachine.Engine, HorizontalAlignment = HorizontalAlignment.Stretch };
        IReadOnlyList<string> engines = Array.Empty<string>();
        void SyncEngines()
        {
            engines = model.SelectedIndex < 0 ? Array.Empty<string>() : NewMachine.EnginesFor(offerings, models[model.SelectedIndex].Id);
            engine.ItemsSource = engines.Select(core.EngineName).ToList();
            engine.SelectedIndex = engines.Count > 0 ? 0 : -1;
            engine.IsEnabled = engines.Count > 1; // always shown, as on Linux and macOS; one choice is not changeable
        }
        var name = new TextBox { Header = Labels.NewMachine.Name };
        var nameError = Note("new.name.error", "", Ui.ToneBrush(Tone.Critical));

        // The disk: the core's choices in its order with their captions, the first being the default.
        var labels = core.Labels();
        var disk = new RadioButtons();
        List<OrigamiDiskOption> options = new();
        var release = new ComboBox { Header = labels.Release, HorizontalAlignment = HorizontalAlignment.Stretch, Visibility = Visibility.Collapsed };
        var releaseCaption = Caption("new.release.caption");
        IReadOnlyList<ReadyDisk> ready = Array.Empty<ReadyDisk>();
        var picked = false; // a choice the person made is kept when the ready-made disks arrive
        var filling = false;
        OrigamiDiskChoice? Chosen() => disk.SelectedIndex >= 0 && disk.SelectedIndex < options.Count ? options[disk.SelectedIndex].Choice : null;
        void FillDisks()
        {
            var keep = Chosen();
            var runsIrix = model.SelectedIndex >= 0 && models[model.SelectedIndex].RunsIrix;
            options = OrigamiFfiMethods.OrigamiDiskOptions(ready.Count > 0).Where(o => runsIrix || o.Choice != OrigamiDiskChoice.Install).ToList();
            filling = true;
            disk.Items.Clear();
            foreach (var o in options)
            {
                var title = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { new TextBlock { Text = o.Title } } };
                if (o.Recommended)
                    title.Children.Add(new TextBlock { Text = Ui.T(OrigamiUiString.Recommended), Foreground = Ui.Res("AccentTextFillColorPrimaryBrush"), Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"], VerticalAlignment = VerticalAlignment.Center });
                disk.Items.Add(new StackPanel { Children = { title, new TextBlock { Text = o.Caption, TextWrapping = TextWrapping.Wrap, Foreground = Ui.Secondary, Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"] } } });
            }
            var at = picked && keep is { } k ? options.FindIndex(o => o.Choice == k) : 0;
            disk.SelectedIndex = at >= 0 ? at : 0;
            filling = false;
            ShowDiskParts();
        }
        var asked = 0;
        async void SyncReady()
        {
            var turn = ++asked;
            var id = model.SelectedIndex < 0 ? null : models[model.SelectedIndex].Id;
            IReadOnlyList<ReadyDisk> found = Array.Empty<ReadyDisk>();
            if (id is not null) { try { found = await core.ReadyDisksAsync(id); } catch (Exception) { } }
            if (turn != asked) return; // a later model choice has asked again
            ready = found;
            release.ItemsSource = found.Select(d => d.Label).ToList();
            release.SelectedIndex = found.Count > 0 ? 0 : -1;
            FillDisks();
        }
        var size = new NumberBox { Header = Ui.T(OrigamiUiString.DiskSize), Value = labels.DefaultGb, Minimum = 1, Maximum = 2048, Width = 160 };
        var sizeRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Visibility = Visibility.Collapsed,
            Children = { size, new TextBlock { Text = Ui.T(OrigamiUiString.Gb), VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, 0, 6) } } };
        var picture = new ImagePanel(core, null);
        picture.Root.Visibility = Visibility.Collapsed;

        // Advanced: the machine ID, prefilled with the next free one, and an Ethernet address to use instead of the one it makes.
        var id = new TextBox { Header = Ui.T(OrigamiUiString.MachineId) };
        var idCaption = Caption("new.id.caption");
        var idError = Note("new.id.error", "", Ui.ToneBrush(Tone.Critical));
        var address = new RadioButtons { Header = Ui.T(OrigamiUiString.EthernetAddress) };
        var fromId = new RadioButton { Content = Ui.T(OrigamiUiString.FromTheMachineId), GroupName = "mac", IsChecked = true };
        var custom = new RadioButton { Content = Ui.T(OrigamiUiString.Custom), GroupName = "mac" };
        address.Items.Add(fromId); address.Items.Add(custom);
        var addressShown = Caption("new.mac");
        var typed = new TextBox { PlaceholderText = "08:00:69:__:__:__", IsEnabled = false };
        var addressError = Note("new.mac.error", "", Ui.ToneBrush(Tone.Critical));
        var advanced = new Expander { Header = Labels.NewMachine.Advanced, HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Content = new StackPanel { Spacing = 8, Children = { id, idCaption, idError, address, addressShown, typed, addressError } } };
        AutomationProperties.SetAutomationId(id, "new.id");
        AutomationProperties.SetAutomationId(custom, "new.mac.custom");
        AutomationProperties.SetAutomationId(typed, "new.mac.entry");

        var bar = new ProgressBar { Minimum = 0, Maximum = 1, Visibility = Visibility.Collapsed };
        var line = new TextBlock { Visibility = Visibility.Collapsed };
        var stop = new Button { Content = Ui.T(OrigamiUiString.Cancel), Visibility = Visibility.Collapsed };
        var copy = new CancellationTokenSource();
        stop.Click += (_, _) => copy.Cancel();
        var diskError = Note("new.disk.error", "", Ui.ToneBrush(Tone.Critical));
        AutomationProperties.SetAutomationId(disk, "new.disk");
        AutomationProperties.SetAutomationId(size, "new.size");
        AutomationProperties.SetAutomationId(release, "new.release");
        AutomationProperties.SetAutomationId(stop, "new.cancel");
        AutomationProperties.SetAutomationId(model, "new.model");
        AutomationProperties.SetAutomationId(engine, "new.engine");
        AutomationProperties.SetAutomationId(name, "new.name");
        var stack = new StackPanel
        {
            Spacing = 10, Width = 420,
            Children = { Heading(Ui.T(OrigamiUiString.Machine)), engine, model, name, nameError, Heading(Ui.T(OrigamiUiString.Disk)), disk, release, releaseCaption, sizeRow, picture.Root, diskError, advanced, bar, line, stop },
        };
        // Everything scrolls inside the dialog, so an error under a field further down is reached, never cut off.
        var scroll = new ScrollViewer { Content = stack, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, Padding = new Thickness(0, 0, 12, 0),
            MaxHeight = Math.Max(320, host.AppWindow.Size.Height / host.XamlRootForTest.RasterizationScale - 220) };
        var dlg = new ContentDialog
        {
            XamlRoot = host.XamlRootForTest,
            Title = Ui.T(OrigamiUiString.NewMachine),
            Content = scroll,
            PrimaryButtonText = Ui.T(OrigamiUiString.Create),
            CloseButtonText = Ui.T(OrigamiUiString.Cancel),
            DefaultButton = ContentDialogButton.Primary,
            IsPrimaryButtonEnabled = false,
        };
        // A list filled before it is on screen can draw with nothing selected; say it again once it is.
        disk.Loaded += (_, _) => { var at = disk.SelectedIndex; filling = true; disk.SelectedIndex = -1; disk.SelectedIndex = at; filling = false; };
        string? identityProblem = null;
        void Sync() => dlg.IsPrimaryButtonEnabled = NewMachine.CanCreate(name.Text, model.SelectedIndex) && identityProblem is null
            && (Chosen() != OrigamiDiskChoice.Image || picture.Choice.Ready) && (Chosen() != OrigamiDiskChoice.Ready || release.SelectedIndex >= 0);
        int round = 0;
        bool writing = false;
        string? TypedAddress() => custom.IsChecked == true && typed.Text.Trim() is { Length: > 0 } a ? a : null;
        async void RefreshIdentity()
        {
            if (model.SelectedIndex < 0) return;
            var mine = ++round;
            var wanted = id.Text;
            IdentityPreview p;
            try { p = await core.PreviewIdentityAsync(models[model.SelectedIndex].Id, engine.SelectedIndex >= 0 ? engines[engine.SelectedIndex] : null, wanted, TypedAddress()); }
            catch (Exception e) { if (mine == round) { identityProblem = e.Message; Show(idError, e.Message); Sync(); } return; }
            if (mine != round) return;
            // The next free ID fills an empty field, never one the person has started typing.
            if (wanted.Trim().Length == 0 && id.Text.Trim().Length == 0) { writing = true; id.Text = p.EmuId.ToString(); writing = false; }
            if (!p.FromId) { custom.IsChecked = true; fromId.IsEnabled = false; } else fromId.IsEnabled = true;
            Show(idCaption, OrigamiFfiMethods.OrigamiMachineIdCaption(p.EmuId));
            Show(addressShown, p.Mac);
            identityProblem = p.Problem;
            Show(idError, p.Problem is not null && p.Field != IdentityField.Address ? p.Problem : null);
            Show(addressError, p.Problem is not null && p.Field == IdentityField.Address ? p.Problem : null);
            Sync();
        }
        void SyncReadyCaption()
        {
            var d = release.SelectedIndex >= 0 && release.SelectedIndex < ready.Count ? ready[release.SelectedIndex] : null;
            releaseCaption.Text = d is null ? "" : OrigamiFfiMethods.OrigamiReadyCaption(d.Bytes, d.Cached);
            releaseCaption.Visibility = d is null || Chosen() != OrigamiDiskChoice.Ready ? Visibility.Collapsed : Visibility.Visible;
        }
        void ShowDiskParts()
        {
            var choice = Chosen();
            release.Visibility = choice == OrigamiDiskChoice.Ready ? Visibility.Visible : Visibility.Collapsed;
            sizeRow.Visibility = choice == OrigamiDiskChoice.Install ? Visibility.Visible : Visibility.Collapsed;
            picture.Root.Visibility = choice == OrigamiDiskChoice.Image ? Visibility.Visible : Visibility.Collapsed;
            Show(diskError, null);
            SyncReadyCaption();
            Sync();
        }
        id.TextChanged += (_, _) => { if (!writing) RefreshIdentity(); };
        typed.TextChanged += (_, _) => RefreshIdentity();
        custom.Checked += (_, _) => { typed.IsEnabled = true; RefreshIdentity(); };
        fromId.Checked += (_, _) => { typed.IsEnabled = false; RefreshIdentity(); };
        engine.SelectionChanged += (_, _) => RefreshIdentity();
        name.TextChanged += (_, _) => { Show(nameError, null); Sync(); };
        picture.Changed += Sync;
        disk.SelectionChanged += (_, _) => { if (!filling) { picked = true; ShowDiskParts(); } };
        model.SelectionChanged += (_, _) => { SyncEngines(); SyncReady(); FillDisks(); Sync(); RefreshIdentity(); };
        release.SelectionChanged += (_, _) => { SyncReadyCaption(); Sync(); };
        SyncEngines();
        FillDisks();
        SyncReady();
        RefreshIdentity();
        if (image is not null)
        {
            var at = options.FindIndex(o => o.Choice == OrigamiDiskChoice.Image);
            if (at >= 0) { disk.SelectedIndex = at; await picture.SetAsync(image); }
        }

        string? created = null;
        var openInstall = false;
        dlg.PrimaryButtonClick += async (_, e) =>
        {
            var deferral = e.GetDeferral();
            dlg.IsPrimaryButtonEnabled = false; // a second click would create the same name twice
            try
            {
                var choice = Chosen();
                SystemDisk? system = choice switch
                {
                    OrigamiDiskChoice.Install => new SystemDisk.Empty((uint)size.Value),
                    OrigamiDiskChoice.Image => picture.Choice.System,
                    OrigamiDiskChoice.Ready => new SystemDisk.Ready(ready[release.SelectedIndex].Name),
                    _ => null,
                };
                var progress = new Progress<CopyProgress>(p =>
                {
                    bar.Visibility = line.Visibility = stop.Visibility = Visibility.Visible;
                    bar.Value = p.Fraction;
                    line.Text = p.Text;
                });
                openInstall = choice == OrigamiDiskChoice.Install;
                created = await core.CreateMachineAsync(name.Text.Trim(), models[model.SelectedIndex].Id, engines[engine.SelectedIndex], copy.Token, system, progress, new IdentityRequest(id.Text, TypedAddress()));
            }
            catch (OperationCanceledException)
            {
                // A cancelled download made nothing: stay on the dialog with a fresh token.
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
                var problem = core.ProblemOf(ex);
                // The error goes under the field it is about: a disk's under the disk, anything else under the name.
                var under = problem.Fix == ErrorFix.ChooseDisk ? diskError : nameError;
                Show(under, problem.Message);
                under.StartBringIntoView();
            }
            finally { deferral.Complete(); }
        };
        await host.ShowDialogAsync(dlg);
        return created is { } key ? (key, openInstall) : null;
    }

    static async Task Message(MainWindow host, string title, string text) =>
        await host.ShowDialogAsync(new ContentDialog { XamlRoot = host.XamlRootForTest, Title = title, Content = text, CloseButtonText = Ui.T(OrigamiUiString.Done) });
}
