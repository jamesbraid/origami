using Origami.Logic;

namespace Origami.App;

/// <summary>Saves a machine's picture as a PNG in the Pictures folder, with no prompt. All of it runs off the UI thread.</summary>
public static class Screenshots
{
    public static string PicturesDir { get; set; } = Environment.GetFolderPath(Environment.SpecialFolder.MyPictures);

    /// <summary>The core writes a PPM; it is converted to a PNG named for the machine and the time. Returns the PNG's path.</summary>
    public static Task<string> SaveAsync(ICoreClient core, string key, string machineName) => Task.Run(async () =>
    {
        var tmp = Path.Combine(Path.GetTempPath(), "origami-shot-" + Guid.NewGuid().ToString("N") + ".ppm");
        try
        {
            core.Screendump(key, 0, tmp);
            var (w, h, bgrx) = Ppm.ReadBgrx(await File.ReadAllBytesAsync(tmp));
            Directory.CreateDirectory(PicturesDir);
            var path = Screenshot.Unique(PicturesDir, Screenshot.FileName(machineName, DateTime.Now), File.Exists);
            await Shot.SavePng(path, bgrx, w, h);
            return path;
        }
        finally { try { File.Delete(tmp); } catch (IOException) { } }
    });
}
