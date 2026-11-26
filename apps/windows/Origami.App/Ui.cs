using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;
using Origami.Logic;
using uniffi.origami_ffi;

namespace Origami.App;

/// <summary>Words every app shows in the same places, and the system brushes the app borrows; the core owns the text, Windows the colours.</summary>
internal static class Ui
{
    internal static string T(OrigamiUiString id) => OrigamiFfiMethods.OrigamiUiString(id);

    // Brushes the app keeps for text and cards. They are one instance each, so a change of theme only has to recolour them:
    // a brush looked up from the application's resources keeps the colour of the theme it was read in.
    static readonly Dictionary<string, SolidColorBrush> Live_ = new();
    static readonly List<(string Key, Border Probe)> Probes = new();

    internal static Brush Res(string key)
    {
        if (Live_.TryGetValue(key, out var live)) return live;
        var from = Application.Current.Resources.TryGetValue(key, out var v) && v is SolidColorBrush b ? b.Color : Microsoft.UI.Colors.Gray;
        return Live_[key] = new SolidColorBrush(from);
    }

    /// <summary>Puts a probe for each of those brushes into the window, so its theme resources resolve for the window's own theme.</summary>
    internal static void Attach(Panel host)
    {
        foreach (var key in new[] { "TextFillColorSecondaryBrush", "SystemFillColorSuccessBrush", "SystemFillColorCautionBrush", "SystemFillColorCriticalBrush", "CardBackgroundFillColorDefaultBrush", "CardStrokeColorDefaultBrush", "AccentTextFillColorPrimaryBrush" })
        {
            var probe = (Border)XamlReader.Load($"<Border xmlns=\"http://schemas.microsoft.com/winfx/2006/xaml/presentation\" Width=\"1\" Height=\"1\" Opacity=\"0\" IsHitTestVisible=\"False\" Background=\"{{ThemeResource {key}}}\"/>");
            host.Children.Add(probe);
            Probes.Add((key, probe));
            Res(key);
        }
        Retheme();
    }

    /// <summary>Recolours the shared brushes from the probes, which follow the window's theme.</summary>
    internal static void Retheme()
    {
        foreach (var (key, probe) in Probes)
            if (probe.Background is SolidColorBrush b && Live_.TryGetValue(key, out var live)) live.Color = b.Color;
    }

    static string ToneKey(Tone t) => t switch
    {
        Tone.Success => "SystemFillColorSuccessBrush",
        Tone.Caution => "SystemFillColorCautionBrush",
        Tone.Critical => "SystemFillColorCriticalBrush",
        _ => "TextFillColorSecondaryBrush",
    };

    internal static Brush ToneBrush(Tone t) => Res(ToneKey(t));
    internal static Brush Secondary => Res("TextFillColorSecondaryBrush");
    internal static Brush CardFill => Res("CardBackgroundFillColorDefaultBrush");
    internal static Brush CardStroke => Res("CardStrokeColorDefaultBrush");

    /// <summary>The status dot: loaded from markup so its brush is a live theme resource and follows light, dark and contrast themes.</summary>
    internal static FrameworkElement Dot(Tone t, double size = Tokens.Icon.RowDot)
    {
        var px = size.ToString(System.Globalization.CultureInfo.InvariantCulture);
        return (FrameworkElement)XamlReader.Load(
            $"<Ellipse xmlns=\"http://schemas.microsoft.com/winfx/2006/xaml/presentation\" Width=\"{px}\" Height=\"{px}\" VerticalAlignment=\"Center\" Fill=\"{{ThemeResource {ToneKey(t)}}}\"/>");
    }

    /// <summary>The style for a destructive button: its label in the critical colour, as Fluent alerts show it.</summary>
    internal static Style? Destructive => Application.Current.Resources.TryGetValue("DestructiveButtonStyle", out var v) ? v as Style : null;

    internal static ButtonRole Role(OrigamiButtonRole r) => Enum.Parse<ButtonRole>(r.ToString());

    /// <summary>The core's alert in the Windows words, with its buttons ready for <see cref="DialogLayout"/>.</summary>
    internal static (OrigamiAlert Alert, IReadOnlyList<AlertButton> Buttons) Alert(OrigamiAlertKind kind)
    {
        var a = OrigamiFfiMethods.OrigamiAlert(kind, OrigamiPlatform.Windows);
        return (a, a.Buttons.Select(b => new AlertButton(b.Label, Role(b.Role))).ToList());
    }

    /// <summary>The About version line from the build's full version (the tag and git description where it has one), like the other apps.</summary>
    internal static string VersionLine()
    {
        var assembly = typeof(Ui).Assembly;
        var full = assembly.GetCustomAttributes(typeof(System.Reflection.AssemblyInformationalVersionAttribute), false)
            .OfType<System.Reflection.AssemblyInformationalVersionAttribute>().FirstOrDefault()?.InformationalVersion ?? assembly.GetName().Version?.ToString(3) ?? "0";
        return OrigamiFfiMethods.OrigamiVersionLine(full.Split('+')[0]);
    }

    internal static string Tooltip(string id) => OrigamiFfiMethods.OrigamiCommandTooltip(id, OrigamiPlatform.Windows) ?? "";
}
