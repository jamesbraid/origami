namespace Origami.Logic;

/// <summary>Names all three apps show in the same places. The self-test writes them to JSON so a check can compare the apps.</summary>
public static class Labels
{
    public const string SidebarHeading = "Machines";

    /// <summary>Settings is one scrolling page here: these are its section headings, named as the other apps name their tabs.</summary>
    public static class Settings
    {
        public const string ProcessorMemory = "Processor & memory", Graphics = "Graphics", Drives = "Drives", Network = "Network",
            SharedFolders = "Shared folders", StartupFirmware = "Startup & firmware";
    }

    /// <summary>The New Machine fields in the order they appear; the IRIX release row only follows a ready-made disk.</summary>
    public static class NewMachine
    {
        public const string Engine = "Engine", Model = "Model", Name = "Name", Disk = "Disk", Advanced = "Advanced";
    }
}
