using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Origami.Logic;

namespace Origami.App;

/// <summary>About: the cube on a photograph, the version, who made it and the Acknowledgements and Licences buttons.
/// A window because a dialog cannot host the cube. Typing "hinv" slides the host inventory over it; Esc puts it back.</summary>
public sealed class AboutWindow : Window
{
    /// <summary>Reflecting a running machine's screen in the cube is off for now; the code stays.</summary>
    public static readonly bool ReflectsRunningMachine = false;
    public bool ReflectsMachine => _frames != null;
    public CubeHost Cube { get; } = new() { MinHeight = 220 };
    readonly ICoreClient _core;
    IFrames? _frames;
    readonly bool _dark;
    readonly WordTyped _egg = new("hinv");
    readonly Windows.UI.ViewManagement.UISettings _ui = new();
    readonly ScrollViewer _creditsScroll;
    readonly Border _hinv = new() { Visibility = Visibility.Collapsed };
    readonly TextBlock _hinvText = new() { FontFamily = new FontFamily("Cascadia Mono, Consolas"), FontSize = 12, Padding = new Thickness(24, 16, 24, 24) };
    readonly TranslateTransform _slide = new();
    readonly Button _acknowledgements, _licences;
    readonly CreditsDocument? _credits;
    bool _hinvBusy, _closed;

    /// <summary>The Acknowledgements or Licences sheet, while one is open.</summary>
    public CreditsWindow? Sheet { get; private set; }
    /// <summary>Presses the button, so a test goes the way a click does.</summary>
    public void ClickAcknowledgements() => Press(_acknowledgements);
    public void ClickLicences() => Press(_licences);

    static void Press(Button b) =>
        ((Microsoft.UI.Xaml.Automation.Provider.IInvokeProvider)new Microsoft.UI.Xaml.Automation.Peers.ButtonAutomationPeer(b)
            .GetPattern(Microsoft.UI.Xaml.Automation.Peers.PatternInterface.Invoke)).Invoke();

    void Show(CreditsWindow.Page page)
    {
        if (_credits is null) return;
        if (Sheet?.Which == page) { Sheet.Activate(); return; }
        Sheet?.Close();
        var sheet = Sheet = new CreditsWindow(page, _credits);
        sheet.Closed += (_, _) => { if (Sheet == sheet) Sheet = null; };
        sheet.Activate();
    }

    public bool HinvShown => _hinv.Visibility == Visibility.Visible;
    public string HinvText => _hinvText.Text;
    public IntPtr Hwnd => WinRT.Interop.WindowNative.GetWindowHandle(this);
    /// <summary>Puts keyboard focus where the egg's letters are read.</summary>
    public void FocusForKeys() => _creditsScroll.Focus(FocusState.Programmatic);

    public AboutWindow(ICoreClient core, string? runningKey, bool dark)
    {
        _core = core; _dark = dark;
        Title = "About Origami";
        AppIcon.Apply(this);
        Cube.WindowHandle = () => WinRT.Interop.WindowNative.GetWindowHandle(this);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(Cube, "about.cube");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(Cube, "The SGI cube");

        var credits = new StackPanel { Spacing = 4, Padding = new Thickness(24, 16, 24, 24), HorizontalAlignment = HorizontalAlignment.Center };
        credits.Children.Add(new TextBlock { Text = "Origami", Style = (Style)Application.Current.Resources["TitleTextBlockStyle"], HorizontalAlignment = HorizontalAlignment.Center });
        credits.Children.Add(new TextBlock { Text = "Version " + (typeof(AboutWindow).Assembly.GetName().Version?.ToString(3) ?? "0"), Opacity = 0.7, HorizontalAlignment = HorizontalAlignment.Center });
        try { _credits = CreditsDocument.Parse(uniffi.origami_ffi.OrigamiFfiMethods.OrigamiCreditsJson()); }
        catch (Exception e) { App.Log("about: no credits: " + e.Message); }
        if (_credits is { } c)
        {
            credits.Children.Add(new TextBlock { Text = c.Copyright, Opacity = 0.7, HorizontalAlignment = HorizontalAlignment.Center });
            credits.Children.Add(new TextBlock { Text = c.LicenceName, Opacity = 0.7, HorizontalAlignment = HorizontalAlignment.Center });
            credits.Children.Add(new HyperlinkButton { Content = c.ProjectUrl.Replace("https://", ""), NavigateUri = new Uri(c.ProjectUrl), HorizontalAlignment = HorizontalAlignment.Center });
        }
        _acknowledgements = new Button { Content = "Acknowledgements", IsEnabled = _credits != null };
        _licences = new Button { Content = "Licences", IsEnabled = _credits != null };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(_acknowledgements, "about.acknowledgements");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(_licences, "about.licences");
        _acknowledgements.Click += (_, _) => Show(CreditsWindow.Page.Acknowledgements);
        _licences.Click += (_, _) => Show(CreditsWindow.Page.Licences);
        credits.Children.Add(new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10, HorizontalAlignment = HorizontalAlignment.Center, Margin = new Thickness(0, 8, 0, 0), Children = { _acknowledgements, _licences } });

        var grid = new Grid();
        grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(240) });
        grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        Grid.SetRow(Cube, 0);
        var scroll = _creditsScroll = new ScrollViewer { Content = credits, IsTabStop = true };
        _hinv.Background = Application.Current.Resources.TryGetValue("SolidBackgroundFillColorBaseBrush", out var bg) && bg is Brush b
            ? b : new SolidColorBrush(Windows.UI.Color.FromArgb(255, dark ? (byte)0x20 : (byte)0xF3, dark ? (byte)0x20 : (byte)0xF3, dark ? (byte)0x20 : (byte)0xF3));
        _hinv.Child = new ScrollViewer { Content = _hinvText };
        _hinv.RenderTransform = _slide;
        _hinv.Tapped += (_, _) => DismissHinv();
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(_hinv, "about.hinv");
        var below = new Grid();
        Grid.SetRow(below, 1);
        below.Children.Add(scroll); below.Children.Add(_hinv);
        below.Clip = new RectangleGeometry();
        below.SizeChanged += (_, e) => ((RectangleGeometry)below.Clip).Rect = new Windows.Foundation.Rect(0, 0, e.NewSize.Width, e.NewSize.Height);
        grid.Children.Add(Cube); grid.Children.Add(below);
        Content = grid;
        grid.KeyDown += OnKeyDown;
        AppWindow.Resize(new Windows.Graphics.SizeInt32(520, 660));

        Cube.ShiftClicked += () => Cube.NextScene();
        Closed += (_, _) => { _closed = true; Sheet?.Close(); Cube.Close(); _frames?.Dispose(); };
        grid.Loaded += (_, _) => { Style(runningKey); FocusForKeys(); };
    }

    void OnKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Right) { Cube.NextScene(); e.Handled = true; return; }
        if (e.Key == Windows.System.VirtualKey.Escape && HinvShown) { DismissHinv(); e.Handled = true; return; }
        if (e.Key is >= Windows.System.VirtualKey.A and <= Windows.System.VirtualKey.Z && _egg.Feed((char)e.Key)) _ = ShowHinvAsync();
    }

    async Task ShowHinvAsync()
    {
        if (_hinvBusy || HinvShown) return;
        _hinvBusy = true;
        string text;
        // The core reads the disk and may open the GPU, so keep it off the UI thread.
        try { text = await Task.Run(() => _core.Hinv()); }
        catch (Exception e) { text = "hinv: " + e.Message; }
        _hinvBusy = false;
        if (_closed) return;
        _hinvText.Text = text;
        _hinv.Visibility = Visibility.Visible;
        if (!_ui.AnimationsEnabled) { _slide.Y = 0; return; }
        _slide.Y = _hinv.ActualHeight > 0 ? _hinv.ActualHeight : 300;
        var anim = new DoubleAnimation { To = 0, Duration = TimeSpan.FromMilliseconds(220), EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut } };
        Storyboard.SetTarget(anim, _slide); Storyboard.SetTargetProperty(anim, "Y");
        var sb = new Storyboard(); sb.Children.Add(anim); sb.Begin();
    }

    void DismissHinv()
    {
        _hinv.Visibility = Visibility.Collapsed;
        FocusForKeys();
    }

    void Style(string? runningKey)
    {
        var reduce = !new Windows.UI.ViewManagement.UISettings().AnimationsEnabled;
        if (ReflectsRunningMachine && runningKey != null)
        {
            try
            {
                _frames = _core.OpenFrames(runningKey);
                Cube.SetStyle(Frame.CubeEnvMachine, 0, null, _dark, false, reduce, interactive: true);
                Cube.SetMachine(_frames.MachineHandle);
                return;
            }
            catch (Exception e) { App.Log("about: no machine reflection: " + e.Message); _frames?.Dispose(); _frames = null; }
        }
        // The scene is the one for the local time, read once when About opens.
        var now = DateTime.Now;
        Cube.SetStyle(Frame.CubeEnvScene, Frame.origami_cube_scene_for_time((uint)now.Hour, (uint)now.Minute), null, _dark, false, reduce, interactive: true);
    }
}
