using Windows.UI;

namespace Origami.App;

/// <summary>The status pill colours, shared by every window that shows a state.</summary>
static class Tones
{
    public static Color For(string tone) => tone switch
    {
        "green" => Color.FromArgb(255, 0x2E, 0x8B, 0x57),
        "amber" => Color.FromArgb(255, 0xB5, 0x7A, 0x00),
        "red" => Color.FromArgb(255, 0xC0, 0x39, 0x2B),
        _ => Color.FromArgb(255, 0x6E, 0x6E, 0x78),
    };
}
