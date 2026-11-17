using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Origami.Logic;

namespace Origami.App;

/// <summary>About: the cube at rest on a British Columbia photograph, the version, credits and licence.
/// A window of its own because a dialog cannot host the cube's surface. Typing "hinv" slides the host's inventory over the credits;
/// Esc or a click puts them back.</summary>
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
    bool _hinvBusy, _closed;

    public bool HinvShown => _hinv.Visibility == Visibility.Visible;
    public string HinvText => _hinvText.Text;
    public IntPtr Hwnd => WinRT.Interop.WindowNative.GetWindowHandle(this);
    /// <summary>Puts keyboard focus where the egg's letters are read.</summary>
    public void FocusForKeys() => _creditsScroll.Focus(FocusState.Programmatic);

    public AboutWindow(ICoreClient core, string? runningKey, bool dark)
    {
        _core = core; _dark = dark;
        Title = "About Origami";
        Cube.WindowHandle = () => WinRT.Interop.WindowNative.GetWindowHandle(this);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(Cube, "about.cube");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(Cube, "The SGI cube");

        var credits = new StackPanel { Spacing = 8, Padding = new Thickness(24, 16, 24, 24) };
        credits.Children.Add(new TextBlock { Text = "Origami", Style = (Style)Application.Current.Resources["TitleTextBlockStyle"] });
        credits.Children.Add(new TextBlock { Text = "Version " + (typeof(AboutWindow).Assembly.GetName().Version?.ToString(3) ?? "0"), Opacity = 0.7 });
        credits.Children.Add(new TextBlock { Text = "All trademarks are the property of their respective owners.", TextWrapping = TextWrapping.Wrap });
        credits.Children.Add(new TextBlock { Text = "Reflections", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Margin = new Thickness(0, 12, 0, 0) });
        for (uint i = 0; i < Frame.origami_cube_scene_count(); i++)
            credits.Children.Add(new TextBlock
            {
                Text = $"{Frame.SceneText(i, 1)}: {Frame.SceneText(i, 2)}",
                TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8, IsTextSelectionEnabled = true,
            });
        credits.Children.Add(new TextBlock { Text = "Licence", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Margin = new Thickness(0, 12, 0, 0) });
        credits.Children.Add(new TextBlock { Text = ReadLicence(), TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8, IsTextSelectionEnabled = true });

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
        AppWindow.Resize(new Windows.Graphics.SizeInt32(520, 760));

        Cube.ShiftClicked += () => Cube.NextScene();
        Closed += (_, _) => { _closed = true; Cube.Close(); _frames?.Dispose(); };
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
                Cube.SetStyle(Frame.CubeEnvMachine, 0, null, _dark, false, reduce);
                Cube.SetMachine(_frames.MachineHandle);
                return;
            }
            catch (Exception e) { App.Log("about: no machine reflection: " + e.Message); _frames?.Dispose(); _frames = null; }
        }
        // The scene is the one for the local time, read once when About opens.
        var now = DateTime.Now;
        Cube.SetStyle(Frame.CubeEnvScene, Frame.origami_cube_scene_for_time((uint)now.Hour, (uint)now.Minute), null, _dark, false, reduce);
    }

    static string ReadLicence()
    {
        var path = Path.Combine(AppContext.BaseDirectory, "LICENSE");
        try { return File.ReadAllText(path); } catch (IOException) { return "BSD 3-Clause License. See the LICENSE file."; }
    }
}
