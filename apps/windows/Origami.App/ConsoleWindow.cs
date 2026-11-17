using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

/// <summary>The machine's serial console as plain text with a line to type into.</summary>
public sealed class ConsoleWindow : Window
{
    readonly ICoreClient _core;
    readonly string _key;
    readonly TextBlock _text = new() { FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Cascadia Mono, Consolas"), TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
    readonly ScrollViewer _scroll;
    readonly TextBox _input = new() { PlaceholderText = "Type to the console, Enter sends" };
    readonly DispatcherQueueTimer _timer;
    ulong _next;

    public ConsoleWindow(ICoreClient core, string key)
    {
        _core = core; _key = key;
        Title = "Console";
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(_text, "console.text");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_text, "Console output");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetLiveSetting(_text, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        _scroll = new ScrollViewer { Content = _text, Padding = new Thickness(8) };
        var grid = new Grid();
        grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetRow(_input, 1);
        grid.Children.Add(_scroll); grid.Children.Add(_input);
        Content = grid;
        _input.KeyDown += (_, e) =>
        {
            if (e.Key != Windows.System.VirtualKey.Enter) return;
            _core.ConsoleSend(_key, System.Text.Encoding.ASCII.GetBytes(_input.Text + "\r"));
            _input.Text = ""; e.Handled = true;
        };
        // One reader following the shared ring by offset; polls cheaply until the core pushes events.
        _timer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        _timer.Interval = TimeSpan.FromMilliseconds(200);
        _timer.Tick += (_, _) => Pump();
        _timer.Start();
        Closed += (_, _) => _timer.Stop();
        Pump();
    }

    void Pump()
    {
        var bytes = _core.ConsoleRead(_key, _next, out _next);
        if (bytes.Length == 0) return;
        var t = _text.Text + System.Text.Encoding.ASCII.GetString(bytes);
        _text.Text = t.Length > 65536 ? t[^65536..] : t; // keep the last 64 KB
        _scroll.ChangeView(null, _scroll.ScrollableHeight, null);
    }
}
