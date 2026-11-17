using System.Text;

namespace Origami.Logic;

public static class Screenshot
{
    /// <summary>"Octane2 2026-11-16 at 14.03.09.png": the machine's name and the local time, safe as a Windows file name.</summary>
    public static string FileName(string machine, DateTime local)
    {
        var bad = Path.GetInvalidFileNameChars();
        var name = new string(machine.Select(c => Array.IndexOf(bad, c) >= 0 ? '-' : c).ToArray()).Trim().TrimEnd('.');
        if (name.Length == 0) name = "Origami";
        return $"{name} {local:yyyy-MM-dd} at {local:HH.mm.ss}.png";
    }

    /// <summary>The first path in <paramref name="dir"/> for <paramref name="fileName"/> that does not exist, adding " 2", " 3" on a clash.</summary>
    public static string Unique(string dir, string fileName, Func<string, bool> exists)
    {
        var stem = Path.GetFileNameWithoutExtension(fileName);
        var path = Path.Combine(dir, fileName);
        for (int n = 2; exists(path); n++) path = Path.Combine(dir, $"{stem} {n}.png");
        return path;
    }
}

/// <summary>The binary PPM (P6) the core's screendump writes.</summary>
public static class Ppm
{
    /// <summary>The picture as tight BGRX, the layout the PNG encoder takes.</summary>
    public static (int W, int H, byte[] Bgrx) ReadBgrx(byte[] b)
    {
        int p = 0;
        string Tok()
        {
            while (b[p] is (byte)' ' or (byte)'\n' or (byte)'\r') p++;
            int s = p;
            while (b[p] is not ((byte)' ' or (byte)'\n' or (byte)'\r')) p++;
            return Encoding.ASCII.GetString(b, s, p - s);
        }
        if (Tok() != "P6") throw new InvalidDataException("not a binary PPM");
        int w = int.Parse(Tok()), h = int.Parse(Tok());
        Tok(); p++; // the maximum value, then the single whitespace before the pixels
        if (b.Length - p < w * h * 3) throw new InvalidDataException("the PPM is shorter than its size says");
        var o = new byte[w * h * 4];
        for (int i = 0; i < w * h; i++) { o[i * 4] = b[p + i * 3 + 2]; o[i * 4 + 1] = b[p + i * 3 + 1]; o[i * 4 + 2] = b[p + i * 3]; o[i * 4 + 3] = 255; }
        return (w, h, o);
    }
}
