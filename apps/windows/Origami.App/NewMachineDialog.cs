using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>The New Machine sheet: a model, a name, Create. A failed create stays on the sheet with the core's reason.</summary>
public static class NewMachineDialog
{
    /// <summary>Shows the sheet; returns the new machine's key, or null when cancelled.</summary>
    public static async Task<string?> ShowAsync(XamlRoot root, ICoreClient core)
    {
        IReadOnlyList<Offering> offerings;
        try { offerings = await Task.Run(core.Catalogue); }
        catch (Exception e) { await Message(root, "Cannot list the machines", e.Message); return null; }
        if (offerings.Count == 0) { await Message(root, "Cannot list the machines", "No engine offers a machine."); return null; }

        var models = NewMachine.Models(offerings);
        var model = new ComboBox { Header = "Model", HorizontalAlignment = HorizontalAlignment.Stretch, ItemsSource = models.Select(NewMachine.Label).ToList(), SelectedIndex = NewMachine.DefaultIndex(models) };
        var engine = new ComboBox { Header = "Engine", HorizontalAlignment = HorizontalAlignment.Stretch };
        IReadOnlyList<string> engines = Array.Empty<string>();
        void SyncEngines()
        {
            engines = model.SelectedIndex < 0 ? Array.Empty<string>() : NewMachine.EnginesFor(offerings, models[model.SelectedIndex].Id);
            engine.ItemsSource = engines.ToList();
            engine.SelectedIndex = engines.Count > 0 ? 0 : -1;
            engine.IsEnabled = engines.Count > 1; // always shown, as on Linux and macOS; one choice is not changeable
        }
        var name = new TextBox { Header = "Name" };
        var error = new TextBlock { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed, Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.IndianRed) };
        AutomationProperties.SetAutomationId(model, "new.model");
        AutomationProperties.SetAutomationId(engine, "new.engine");
        AutomationProperties.SetAutomationId(name, "new.name");
        AutomationProperties.SetAutomationId(error, "new.error");
        var dlg = new ContentDialog
        {
            XamlRoot = root,
            Title = "New Machine",
            Content = new StackPanel { Spacing = 12, Width = 380, Children = { model, engine, name, error } },
            PrimaryButtonText = "Create",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            IsPrimaryButtonEnabled = false,
        };
        void Sync() => dlg.IsPrimaryButtonEnabled = NewMachine.CanCreate(name.Text, model.SelectedIndex);
        name.TextChanged += (_, _) => Sync();
        model.SelectionChanged += (_, _) => { SyncEngines(); Sync(); };
        SyncEngines();

        string? created = null;
        dlg.PrimaryButtonClick += async (_, e) =>
        {
            var deferral = e.GetDeferral();
            dlg.IsPrimaryButtonEnabled = false; // a second click would create the same name twice
            try
            {
                created = await core.CreateMachineAsync(name.Text.Trim(), models[model.SelectedIndex].Id, engines[engine.SelectedIndex]);
            }
            catch (Exception ex)
            {
                e.Cancel = true;
                dlg.IsPrimaryButtonEnabled = true;
                error.Text = ex.Message;
                error.Visibility = Visibility.Visible;
            }
            finally { deferral.Complete(); }
        };
        await dlg.ShowAsync();
        return created;
    }

    static async Task Message(XamlRoot root, string title, string text) =>
        await new ContentDialog { XamlRoot = root, Title = title, Content = text, CloseButtonText = "OK" }.ShowAsync();
}
