using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>One machine's state as the core reports it (from the client's snapshot, so reads never wait on QEMU), with a button that copies it.</summary>
public sealed class DiagnosticsWindow : Window
{
    readonly ICoreClient _core;
    readonly string _key;
    readonly Func<(long Wakes, long Presents)> _screen;
    readonly TextBlock _title = new() { FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center };
    readonly Border _pillBox = new() { CornerRadius = new CornerRadius(9), Padding = new Thickness(8, 1, 8, 1), VerticalAlignment = VerticalAlignment.Center };
    readonly TextBlock _pill = new() { Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.White), FontSize = 12 };
    readonly TextBlock _report = new() { FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Cascadia Mono, Consolas"), IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap };
    readonly DispatcherQueueTimer _timer;

    public string ReportText => _report.Text;

    public DiagnosticsWindow(ICoreClient core, string key, Func<(long, long)> screenCounts)
    {
        _core = core; _key = key; _screen = screenCounts;
        Title = "Diagnostics";
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(_report, "diagnostics.report");
        _pillBox.Child = _pill;
        var copy = new Button { Content = "Copy Report" };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(copy, "diagnostics.copy");
        copy.Click += (_, _) => Copy();
        var bar = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12, Padding = new Thickness(16, 12, 16, 8), Children = { _title, _pillBox, copy } };
        var grid = new Grid();
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        Grid.SetRow(bar, 0);
        var scroll = new ScrollViewer { Content = _report, Padding = new Thickness(16, 4, 16, 16) };
        Grid.SetRow(scroll, 1);
        grid.Children.Add(bar); grid.Children.Add(scroll);
        Content = grid;
        AppWindow.Resize(new Windows.Graphics.SizeInt32(720, 460));
        _timer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        _timer.Interval = TimeSpan.FromSeconds(1);
        _timer.Tick += (_, _) => Refresh();
        _timer.Start();
        Closed += (_, _) => _timer.Stop();
        Refresh();
    }

    public void Refresh()
    {
        try
        {
            var d = _core.Diagnostics(_key);
            var (wakes, presents) = _screen();
            _title.Text = d.Name;
            _pill.Text = Pill.Text(d.State);
            _pillBox.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(Tones.For(Pill.Tone(d.State)));
            _report.Text = DiagnosticsReport.Format(d, wakes, presents);
        }
        catch (Exception e) { _report.Text = "Could not read the machine: " + e.Message; }
    }

    public void Copy()
    {
        var pkg = new Windows.ApplicationModel.DataTransfer.DataPackage();
        pkg.SetText(_report.Text);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
    }
}
