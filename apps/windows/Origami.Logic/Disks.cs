namespace Origami.Logic;

/// <summary>What the core says about a disk image file.</summary>
public sealed record DiskImageCheck(ulong Bytes, bool IsChd, ulong FreeBytes, string? UsedBy, string? Problem);

/// <summary>What to say under the choices for an image, and whether it can be added; the core words it.</summary>
public sealed record ImageVerdict(string? Note, bool Warning, bool Ready);

/// <summary>The New Machine disk choices and how an image is kept, as the core words them.</summary>
public sealed record DiskLabels(IReadOnlyList<string> Choices, IReadOnlyList<string> Place, IReadOnlyList<string> Keep, string SameKind, string Ready, string Release, uint DefaultGb);

/// <summary>A ready-made disk the server offers for a machine; <c>Label</c> is what to show, such as "IRIX 6.5.22, 835 MB".</summary>
public sealed record ReadyDisk(string Name, string Version, ulong Bytes, bool Cached, string Label);

/// <summary>One line of the Drives page: the core's title for the drive and the caption under it.</summary>
public sealed record DriveRow(string Name, string Title, string Caption);

/// <summary>A downloaded disk, listed in Preferences.</summary>
public sealed record CachedDisk(string Key, string Label, ulong Bytes);

/// <summary>How far a copy has got, with the core's line for it.</summary>
public sealed record CopyProgress(double Fraction, string Text);

/// <summary>The system disk a new machine starts with.</summary>
public abstract record SystemDisk
{
    public sealed record Empty(uint Gb) : SystemDisk;
    /// <summary>The file used where it is, or copied into the machine's folder; <c>Overlay</c> keeps a CHD's changes apart.</summary>
    public sealed record Image(string Path, bool Copy, bool Overlay) : SystemDisk;
    /// <summary>A <see cref="ReadyDisk.Name"/>: downloaded and checked first if needed, then copied in with its changes kept apart.</summary>
    public sealed record Ready(string Name) : SystemDisk;
}

/// <summary>The image a person chose and the questions about it; the macOS and GTK apps ask the same and say the same.</summary>
public sealed class ImageChoice
{
    public string? Path { get; set; }
    public DiskImageCheck? Check { get; set; }
    /// <summary>What the disk holds, "Reading…" until the core says.</summary>
    public string Badge { get; set; } = "";
    /// <summary>The core's sentence when the file could not be read at all.</summary>
    public string? Error { get; set; }
    public bool Copy { get; set; } = true;
    /// <summary>A CHD keeps changes apart unless told to save into the image.</summary>
    public bool Apart { get; set; } = true;

    /// <summary>Set by the panel from the core each time the choice changes.</summary>
    public ImageVerdict? Verdict { get; set; }

    public bool Ready => Verdict?.Ready ?? false;

    public bool Overlay => Apart && (Check?.IsChd ?? false);

    /// <summary>The system disk for a new machine; null until the image can be added.</summary>
    public SystemDisk? System => Ready && Path is { } p ? new SystemDisk.Image(p, Copy, Overlay) : null;
}
