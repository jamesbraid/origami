namespace Origami.Logic;

// What the core says about a machine's state, in the app's own types. The words come from the core; these only carry them.

/// <summary>The states a person tells apart, named as the core names them.</summary>
public enum ShownState { Stopped, Starting, Running, Paused, Saving, LeftRunning, Installing, NoPicture, Exited, NeedsUpgrade, CantOpen }

/// <summary>How a word should look; the core decides, the app picks the colour.</summary>
public enum Tone { None, Success, Caution, Critical }

/// <summary>Everything a library row shows: the state word, the second line and the tooltip.</summary>
public sealed record RowInfo(ShownState State, string Word, Tone Tone, string Line, Tone LineTone, string Description);

public enum CardActionKind { OpenConsole, PowerOff, Reconnect, Resume, Upgrade, ShowInFolder, TryAgain, OpenSettings, ChooseFirmware, ChooseDisk }

public sealed record CardAction(CardActionKind Kind, string Label);

/// <summary>What the machine page shows in place of the picture. An empty title and body are for a card that is only its buttons.</summary>
public sealed record CardInfo(string Title, string Body, IReadOnlyList<CardAction> Actions, string? Details);

/// <summary>The bar under a running machine's picture; null parts are not shown.</summary>
public sealed record StatusBarInfo(string? Picture, string? Pointer);

/// <summary>What the person can do about an error, as the core names it.</summary>
public enum ErrorFix { None, OpenSettings, ChooseFirmware, ChooseDisk }

/// <summary>An error as the core words it: one plain sentence, the technical part for Show details, and the fix on offer.</summary>
public sealed record Problem(string Message, string? Detail, ErrorFix Fix);

public enum ButtonRole { Safe, Destructive, Cancel }

public sealed record AlertButton(string Label, ButtonRole Role);

/// <summary>Where a core alert's buttons go on a ContentDialog, and which one Return picks.</summary>
public sealed record DialogLayout(string? Primary, string? Secondary, string Close, int Default, int Destructive)
{
    public const int PrimarySlot = 0, SecondarySlot = 1, CloseSlot = 2, None = -1;

    /// <summary>Safe answers and destructive ones fill Primary then Secondary in the core's order, Cancel is Close, and Return picks
    /// the safe answer, else Cancel: an alert never defaults to the destructive button.</summary>
    public static DialogLayout Of(IReadOnlyList<AlertButton> buttons)
    {
        var acts = buttons.Where(b => b.Role != ButtonRole.Cancel).ToList();
        var cancel = buttons.FirstOrDefault(b => b.Role == ButtonRole.Cancel)?.Label ?? acts.Last().Label;
        if (buttons.All(b => b.Role != ButtonRole.Cancel)) acts.RemoveAt(acts.Count - 1);
        var safe = acts.FindIndex(b => b.Role == ButtonRole.Safe);
        var bad = acts.FindIndex(b => b.Role == ButtonRole.Destructive);
        return new DialogLayout(acts.ElementAtOrDefault(0)?.Label, acts.ElementAtOrDefault(1)?.Label, cancel,
            safe >= 0 ? safe : CloseSlot, bad >= 0 ? bad : None);
    }

    /// <summary>The label of the button a slot stands for.</summary>
    public string? Label(int slot) => slot switch { PrimarySlot => Primary, SecondarySlot => Secondary, _ => Close };

    /// <summary>The core's button a slot stands for.</summary>
    public AlertButton? Chosen(IReadOnlyList<AlertButton> buttons, int slot) =>
        Label(slot) is { } l ? buttons.First(b => b.Label == l) : null;
}

/// <summary>Cuts the middle out of a name that is too wide, as macOS and GNOME do; WinUI can only trim the end.</summary>
public static class MiddleTrim
{
    public const string Ellipsis = "…";

    /// <summary>The text, or its head and tail around an ellipsis, keeping as many characters as still fit in <paramref name="available"/>.</summary>
    public static string Fit(string text, double available, Func<string, double> width)
    {
        if (text.Length < 2 || width(text) <= available) return text;
        int lo = 0, hi = text.Length - 1;
        while (lo < hi)
        {
            var mid = (lo + hi + 1) / 2;
            if (width(Cut(text, mid)) <= available) lo = mid; else hi = mid - 1;
        }
        return Cut(text, lo);
    }

    static string Cut(string text, int kept)
    {
        var head = kept / 2;
        return text[..head] + Ellipsis + text[^(kept - head)..];
    }
}
