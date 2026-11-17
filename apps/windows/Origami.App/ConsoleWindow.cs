using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Origami.Logic;
using Windows.System;
using Windows.UI;

namespace Origami.App;

/// <summary>
/// The machine's console as the core's terminal grid: colours, a cursor, selection, copy, paste and scrollback. A timer asks the
/// terminal what changed and redraws only those rows. Type with the window focused; Ctrl+C copies a selection (else it is sent),
/// Ctrl+Shift+C and Ctrl+V / Ctrl+Shift+V copy and paste, Shift+PageUp/PageDown and the wheel scroll back.
/// Shown in its own window, or in the main window's pane for a machine with no display.
/// </summary>
public sealed class ConsoleView : Grid
{
    const double FontSize_ = 14;
    static readonly FontFamily Mono = new("Cascadia Mono, Consolas");
    const uint DefaultFg = 0xCCCCCC, DefaultBg = 0x0C0C0C;

    readonly ICoreClient _core;
    readonly string _key;
    readonly ITerminal _term;
    readonly Grid _host = new() { Background = new SolidColorBrush(Hex(DefaultBg)), IsTabStop = true };
    readonly Canvas _canvas = new();
    readonly Canvas _overlay = new() { IsHitTestVisible = false };
    readonly Rectangle _cursor = new() { Fill = new SolidColorBrush(Color.FromArgb(160, 0xCC, 0xCC, 0xCC)), IsHitTestVisible = false };
    readonly TextBlock _scrollNote = new() { FontSize = 12, Opacity = 0.8, VerticalAlignment = VerticalAlignment.Center };
    readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _timer;
    readonly TermSelection _sel = new();
    readonly List<Canvas> _rows = new();
    double _cw, _ch;
    ulong _seq;
    bool _dragging;

    public string TextForTest => string.Join("\n", Enumerable.Range(0, _term.Size.Rows).Select(r => string.Concat(_term.Row(r).Where(c => !c.Continuation).Select(c => c.Text.Length == 0 ? " " : c.Text)).TrimEnd()));
    public string SelectedText => _sel.Text(r => _term.Row(r), _term.Size.Cols);
    public int Rows => _term.Size.Rows;
    public int Cols => _term.Size.Cols;
    public int ColourCells { get; private set; }
    public long RowsDrawn { get; private set; }

    static Color Hex(uint rgb) => Color.FromArgb(255, (byte)(rgb >> 16), (byte)(rgb >> 8), (byte)rgb);

    public ConsoleView(ICoreClient core, string key)
    {
        _core = core; _key = key;
        Measure();
        _term = core.OpenTerminal(key, 24, 80);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(_host, "console.terminal");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_host, "Console");

        _host.Children.Add(_canvas); _host.Children.Add(_overlay);
        _canvas.Children.Add(_cursor);

        var copy = new Button { Content = "Copy" }; var paste = new Button { Content = "Paste" };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(copy, "console.copy");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(paste, "console.paste");
        copy.Click += (_, _) => Copy(); paste.Click += async (_, _) => await PasteAsync();
        var bar = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Padding = new Thickness(8, 4, 8, 4), Children = { copy, paste, _scrollNote } };
        RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetRow(bar, 1);
        Children.Add(_host); Children.Add(bar);

        _host.SizeChanged += (_, e) => Fit(e.NewSize.Width, e.NewSize.Height);
        _host.KeyDown += OnKeyDown;
        _host.CharacterReceived += OnChar;
        _host.PointerPressed += OnPress;
        _host.PointerMoved += OnMove;
        _host.PointerReleased += (_, _) => _dragging = false;
        _host.PointerWheelChanged += (_, e) => { Scroll(e.GetCurrentPoint(_host).Properties.MouseWheelDelta > 0 ? 3 : -3); e.Handled = true; };
        _host.GotFocus += (_, _) => _cursor.Opacity = 1; _host.LostFocus += (_, _) => _cursor.Opacity = 0.4;
        _timer = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread().CreateTimer();
        _timer.Interval = TimeSpan.FromMilliseconds(33);
        _timer.Tick += (_, _) => Tick();
        _timer.Start();
    }

    public void Dispose() { _timer.Stop(); _term.Dispose(); }
    public void FocusTerminal() => _host.Focus(FocusState.Programmatic);

    void Measure()
    {
        var t = new TextBlock { Text = "MMMMMMMMMM", FontFamily = Mono, FontSize = FontSize_ };
        t.Measure(new Windows.Foundation.Size(double.PositiveInfinity, double.PositiveInfinity));
        _cw = t.DesiredSize.Width / 10; _ch = t.DesiredSize.Height;
    }

    // ---- layout and drawing -------------------------------------------------------------------

    void Fit(double w, double h)
    {
        int cols = Math.Max(10, (int)(w / _cw)), rows = Math.Max(2, (int)(h / _ch));
        if ((rows, cols) != _term.Size) _term.Resize(rows, cols);
        foreach (var r in _rows) _canvas.Children.Remove(r);
        _rows.Clear();
        for (int i = 0; i < rows; i++) { var c = new Canvas { Width = cols * _cw, Height = _ch }; Canvas.SetTop(c, i * _ch); _canvas.Children.Add(c); _rows.Add(c); }
        _canvas.Children.Remove(_cursor); _canvas.Children.Add(_cursor);
        _cursor.Width = _cw; _cursor.Height = _ch;
        _seq = 0; Tick();
    }

    void Tick()
    {
        try
        {
            var d = _term.ChangedSince(_seq);
            if (d.Sequence != _seq || d.Full)
            {
                _seq = d.Sequence;
                if (d.Full) for (int r = 0; r < _rows.Count; r++) DrawRow(r);
                else foreach (var r in d.Rows) if (r < _rows.Count) DrawRow(r);
            }
            var (cr, cc, vis) = _term.Cursor;
            _cursor.Visibility = vis && _term.ScrollOffset == 0 ? Visibility.Visible : Visibility.Collapsed;
            Canvas.SetLeft(_cursor, cc * _cw); Canvas.SetTop(_cursor, cr * _ch);
            var off = _term.ScrollOffset;
            _scrollNote.Text = off > 0 ? $"Scrolled back {off} lines; End or typing returns" : "";
        }
        catch (Exception e) { App.Log("console redraw: " + e.Message); }
    }

    void DrawRow(int r)
    {
        var host = _rows[r];
        RowsDrawn++;
        host.Children.Clear();
        var cells = _term.Row(r);
        var text = new TextBlock { FontFamily = Mono, FontSize = FontSize_, IsHitTestVisible = false };
        int x = 0, coloured = 0;
        // Group runs of cells that share attributes; backgrounds that differ from the default become rectangles behind the text.
        int i = 0;
        while (i < cells.Length)
        {
            var c = cells[i];
            int j = i + 1;
            while (j < cells.Length && Same(cells[j], c)) j++;
            var (fg, bg) = Colours(c);
            var span = string.Concat(cells.Skip(i).Take(j - i).Where(k => !k.Continuation).Select(k => k.Text.Length == 0 ? " " : k.Text));
            if (bg != DefaultBg) { var rect = new Rectangle { Width = (j - i) * _cw, Height = _ch, Fill = new SolidColorBrush(Hex(bg)) }; Canvas.SetLeft(rect, i * _cw); host.Children.Add(rect); }
            if (fg != DefaultFg || bg != DefaultBg) coloured += j - i;
            var run = new Run { Text = span, Foreground = new SolidColorBrush(Hex(fg)) };
            if (c.Bold) run.FontWeight = Microsoft.UI.Text.FontWeights.Bold;
            if (c.Italic) run.FontStyle = Windows.UI.Text.FontStyle.Italic;
            var inl = c.Underline ? (Inline)new Underline { Inlines = { run } } : run;
            text.Inlines.Add(inl);
            i = j; x = j;
        }
        ColourCells = Math.Max(ColourCells, coloured);
        host.Children.Add(text);
        DrawSelection();
    }

    static bool Same(TermCell a, TermCell b) =>
        a.Fg == b.Fg && a.Bg == b.Bg && a.Bold == b.Bold && a.Dim == b.Dim && a.Italic == b.Italic && a.Underline == b.Underline && a.Inverse == b.Inverse;

    static (uint Fg, uint Bg) Colours(TermCell c)
    {
        var fg = TermPalette.Rgb(c.Fg, DefaultFg); var bg = TermPalette.Rgb(c.Bg, DefaultBg);
        if (c.Inverse) (fg, bg) = (bg, fg);
        if (c.Dim) fg = ((fg >> 1) & 0x7F7F7F) + ((bg >> 1) & 0x7F7F7F);
        return (fg, bg);
    }

    void DrawSelection()
    {
        _overlay.Children.Clear();
        if (_sel.Range() is not { } r) return;
        for (int y = r.From.R; y <= r.To.R && y < _rows.Count; y++)
        {
            int from = y == r.From.R ? r.From.C : 0, to = y == r.To.R ? r.To.C : _term.Size.Cols - 1;
            var rect = new Rectangle { Width = (to - from + 1) * _cw, Height = _ch, Fill = new SolidColorBrush(Color.FromArgb(90, 0x4A, 0x52, 0xC4)) };
            Canvas.SetLeft(rect, from * _cw); Canvas.SetTop(rect, y * _ch);
            _overlay.Children.Add(rect);
        }
    }

    // ---- input ----------------------------------------------------------------------------------

    (int Row, int Col) CellAt(Windows.Foundation.Point p) =>
        (Math.Clamp((int)(p.Y / _ch), 0, _term.Size.Rows - 1), Math.Clamp((int)(p.X / _cw), 0, _term.Size.Cols - 1));

    void OnPress(object s, PointerRoutedEventArgs e)
    {
        _host.Focus(FocusState.Pointer);
        var (r, c) = CellAt(e.GetCurrentPoint(_host).Position);
        _sel.Begin(r, c); _dragging = true; DrawSelection();
    }

    void OnMove(object s, PointerRoutedEventArgs e)
    {
        if (!_dragging) return;
        var (r, c) = CellAt(e.GetCurrentPoint(_host).Position);
        _sel.Extend(r, c); DrawSelection();
    }

    static bool Down(VirtualKey k) => InputKeyboardSource.GetKeyStateForCurrentThread(k).HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down);

    void OnKeyDown(object s, KeyRoutedEventArgs e)
    {
        bool ctrl = Down(VirtualKey.Control), shift = Down(VirtualKey.Shift), alt = Down(VirtualKey.Menu);
        if (ctrl && e.Key == VirtualKey.C && (shift || _sel.Active)) { Copy(); e.Handled = true; return; }
        if (ctrl && e.Key == VirtualKey.V) { _ = PasteAsync(); e.Handled = true; return; }
        if (shift && e.Key is VirtualKey.PageUp or VirtualKey.PageDown) { Scroll((e.Key == VirtualKey.PageUp ? 1 : -1) * Math.Max(1, _term.Size.Rows - 1)); e.Handled = true; return; }
        if (e.Key == VirtualKey.End && _term.ScrollOffset > 0) { Scroll(-_term.ScrollOffset); e.Handled = true; return; }
        _sel.Clear(); DrawSelection();
        if (TermKeys.FromVirtualKey((int)e.Key) is { } k)
        {
            if (k.Kind == TermKeyKind.Tab && shift) k = new TermKey(TermKeyKind.BackTab);
            Send(k, shift && k.Kind != TermKeyKind.BackTab, ctrl, alt); e.Handled = true;
        }
        else if (ctrl && e.Key is >= VirtualKey.A and <= VirtualKey.Z)
        {
            Send(new TermKey(TermKeyKind.Text, ((char)('a' + (e.Key - VirtualKey.A))).ToString()), false, true, alt); e.Handled = true;
        }
    }

    void OnChar(UIElement s, CharacterReceivedRoutedEventArgs e)
    {
        var ch = e.Character;
        if (ch < 0x20 || ch == 0x7F || Down(VirtualKey.Control)) return; // control keys went through KeyDown
        Send(new TermKey(TermKeyKind.Text, ch.ToString()), false, false, Down(VirtualKey.Menu));
        e.Handled = true;
    }

    void Send(TermKey k, bool shift, bool ctrl, bool alt)
    {
        try { if (_term.ScrollOffset > 0) _term.Scroll(-_term.ScrollOffset); _term.SendKey(k, shift, ctrl, alt); }
        catch (Exception e) { _scrollNote.Text = "Cannot send: " + e.Message; }
    }

    void Scroll(int lines) { _term.Scroll(lines); _sel.Clear(); _seq = 0; Tick(); }

    public void Copy()
    {
        var text = SelectedText;
        if (text.Length == 0) return;
        var pkg = new Windows.ApplicationModel.DataTransfer.DataPackage();
        pkg.SetText(text);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
    }

    /// <summary>Pastes the clipboard's text; the core turns CRLF into CR.</summary>
    public async Task PasteAsync()
    {
        var view = Windows.ApplicationModel.DataTransfer.Clipboard.GetContent();
        if (!view.Contains(Windows.ApplicationModel.DataTransfer.StandardDataFormats.Text)) return;
        try { _term.Paste(await view.GetTextAsync()); }
        catch (Exception e) { _scrollNote.Text = "Cannot paste: " + e.Message; }
    }

    // ---- self-test hooks ----------------------------------------------------------------------

    public void SelectAll(int row0, int col0, int row1, int col1) { _sel.Begin(row0, col0); _sel.Extend(row1, col1); DrawSelection(); }
    public void Type(string text) { foreach (var ch in text) Send(new TermKey(TermKeyKind.Text, ch.ToString()), false, false, false); }
    public void PressEnter() => Send(new TermKey(TermKeyKind.Enter), false, false, false);
    public void PressBackspace() => Send(new TermKey(TermKeyKind.Backspace), false, false, false);
    public int CursorCol => _term.Cursor.Col;
    public void ScrollBack(int lines) => Scroll(lines);
    public int ScrollOffset => _term.ScrollOffset;
    public int ScrollbackCount => _term.ScrollbackCount;
}

/// <summary>A console in a window of its own.</summary>
public sealed class ConsoleWindow : Window
{
    public ConsoleView View { get; }

    public ConsoleWindow(ICoreClient core, string key)
    {
        Title = "Console";
        View = new ConsoleView(core, key);
        Content = View;
        AppWindow.Resize(new Windows.Graphics.SizeInt32(960, 640));
        Closed += (_, _) => View.Dispose();
        Activated += (_, _) => View.FocusTerminal();
    }
}
