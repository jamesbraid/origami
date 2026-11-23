using uniffi.origami_ffi;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>The Acknowledgements and Licences sheets About opens, from the core's credits document.</summary>
public sealed class CreditsWindow : Window
{
    public enum Page { Acknowledgements, Licences }

    public Page Which { get; }
    /// <summary>Every title and line the sheet shows, for the self-test to read.</summary>
    public string Text { get; private set; } = "";
    /// <summary>The licence groups shown, after the search.</summary>
    public IReadOnlyList<string> ShownGroups => _shown;

    readonly CreditsDocument _doc;
    readonly StackPanel _list = new() { Spacing = 8 };
    readonly TextBox _search = new() { PlaceholderText = Ui.T(OrigamiUiString.SearchComponentsAndLicences) };
    readonly List<Expander> _groups = new();
    List<string> _shown = new();

    public CreditsWindow(Page which, CreditsDocument doc)
    {
        Which = which; _doc = doc;
        Title = which == Page.Acknowledgements ? "Acknowledgements" : "Licences";
        AppIcon.Apply(this);
        Content = which == Page.Acknowledgements ? Acknowledgements() : Licences();
        AppWindow.Resize(new Windows.Graphics.SizeInt32(560, 720));
    }

    static TextBlock Line(string text, double size = 12, double opacity = 0.75) => new()
    {
        Text = text, FontSize = size, Opacity = opacity, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true,
    };

    static HyperlinkButton Link(string label, string url) => new()
    {
        Content = label, Padding = new Thickness(0), FontSize = 12,
        NavigateUri = Uri.TryCreate(url, UriKind.Absolute, out var u) ? u : null,
        IsEnabled = Uri.TryCreate(url, UriKind.Absolute, out _),
    };

    /// <summary>One credited thing: its name, a line about it and links.</summary>
    UIElement Row(string title, string detail, params (string Label, string Url)[] links)
    {
        Text += title + "\n" + detail + "\n";
        var row = new StackPanel { Spacing = 2, Margin = new Thickness(0, 0, 0, 10) };
        row.Children.Add(new TextBlock { Text = title, FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
        if (detail.Length > 0) row.Children.Add(Line(detail));
        var bar = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 14 };
        foreach (var (label, url) in links.Where(l => l.Url.Length > 0)) { bar.Children.Add(Link(label, url)); Text += label + " " + url + "\n"; }
        if (bar.Children.Count > 0) row.Children.Add(bar);
        return row;
    }

    static TextBlock Heading(string text) => new()
    {
        Text = text, FontSize = 16, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 14, 0, 6),
    };

    UIElement Acknowledgements()
    {
        var c = _doc;
        var panel = new StackPanel { Padding = new Thickness(24, 8, 24, 24) };
        panel.Children.Add(Heading("Built on"));
        foreach (var p in c.BuiltOn)
        {
            panel.Children.Add(Row(p.Name, $"{p.By}. {p.Note}", ("Website", p.Url)));
            if (p.Name.StartsWith("QEMU")) panel.Children.Add(Row(c.Qemu.Label, "The exact source of the bundled build.", ("Source", c.Qemu.Url)));
        }
        panel.Children.Add(Heading("Thanks"));
        foreach (var t in c.Thanks) panel.Children.Add(Row(t.Name, t.Reason, ("Website", t.Url)));
        panel.Children.Add(Heading("Photographs"));
        foreach (var p in c.Photographs)
            panel.Children.Add(Row(p.Title, $"{p.Author}, {p.Year}. {p.Changes}.", (p.Licence, p.LicenceUrl), ("Source", p.Source)));
        panel.Children.Add(Heading("Artwork"));
        foreach (var a in c.Artwork) panel.Children.Add(Row(a.Name, a.From, ("Source", a.Url)));
        panel.Children.Add(Line(c.Trademark));
        Text += c.Trademark + "\n";
        return new ScrollViewer { Content = panel };
    }

    UIElement Licences()
    {
        var c = _doc;
        var top = new StackPanel { Spacing = 6, Padding = new Thickness(24, 16, 24, 8) };
        top.Children.Add(_search);
        top.Children.Add(Line(c.Qemu.Note));
        top.Children.Add(Link($"Source of {c.Qemu.Label}", c.Qemu.Url));
        Text += c.Qemu.Note + "\n";
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(_search, "licences.search");
        _search.TextChanged += (_, _) => Rebuild();
        var grid = new Grid();
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var scroll = new ScrollViewer { Content = _list, Padding = new Thickness(24, 0, 24, 24) };
        Grid.SetRow(scroll, 1);
        grid.Children.Add(top); grid.Children.Add(scroll);
        Rebuild();
        return grid;
    }

    /// <summary>Types into the search box, as a person would.</summary>
    public void Search(string query) => _search.Text = query;

    /// <summary>Opens the nth licence group, as a click on its header does.</summary>
    public void ExpandGroup(int index) { if (index < _groups.Count) _groups[index].IsExpanded = true; }

    void Rebuild()
    {
        _list.Children.Clear();
        _groups.Clear();
        var query = _search.Text.Trim();
        var shown = _doc.Filter(query);
        _shown = shown.Select(l => l.Name).ToList();
        foreach (var licence in shown)
        {
            var inner = new StackPanel { Spacing = 4 };
            foreach (var entry in licence.Entries) inner.Children.Add(Entry(entry));
            var header = new StackPanel();
            header.Children.Add(new TextBlock { Text = licence.Name, FontWeight = FontWeights.SemiBold });
            header.Children.Add(Line(licence.Count, 12, 0.6));
            var group = new Expander
            {
                Header = header, Content = inner, IsExpanded = query.Length > 0,
                HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
            };
            _groups.Add(group);
            _list.Children.Add(group);
        }
    }

    /// <summary>One entry; its text is laid out when it first opens, since some are hundreds of lines.</summary>
    static Expander Entry(CreditsDocument.LicenceEntry entry)
    {
        var expander = new Expander
        {
            Header = new TextBlock { Text = entry.Title, TextWrapping = TextWrapping.Wrap },
            HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch,
        };
        var built = false;
        expander.Expanding += (_, _) =>
        {
            if (built) return;
            built = true;
            var body = new StackPanel { Spacing = 8 };
            if (entry.Components.Count > 1) body.Children.Add(Line(string.Join(", ", entry.Components)));
            body.Children.Add(new TextBlock
            {
                Text = entry.Text, FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Cascadia Mono, Consolas"), FontSize = 12,
                TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true,
            });
            expander.Content = body;
        };
        return expander;
    }
}
