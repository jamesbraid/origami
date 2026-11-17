namespace Origami.Logic;

/// <summary>Follow sends absolute head pixels; capture sends relative motion until Ctrl+Alt or focus loss. Pure logic.</summary>
public sealed class PointerController
{
    public PointerMode Mode { get; set; } = PointerMode.Follow;
    public bool Captured { get; private set; }

    readonly KeyboardState _keys;
    public PointerController(KeyboardState keys) { _keys = keys; }

    /// <summary>Panel DIP position to head pixel; null when outside the picture.</summary>
    public static (int X, int Y)? ToHead(double px, double py, double panelW, double panelH, int headW, int headH)
    {
        if (panelW <= 0 || panelH <= 0 || px < 0 || py < 0 || px >= panelW || py >= panelH) return null;
        var x = (int)(px / panelW * headW);
        var y = (int)(py / panelH * headH);
        return (Math.Clamp(x, 0, headW - 1), Math.Clamp(y, 0, headH - 1));
    }

    /// <summary>Host pointer moved to (px,py) in panel DIPs.</summary>
    public InputEvent? OnMove(double px, double py, double panelW, double panelH, int headW, int headH,
                              double centerX, double centerY)
    {
        if (Captured)
        {
            int dx = (int)Math.Round(px - centerX), dy = (int)Math.Round(py - centerY);
            return dx == 0 && dy == 0 ? null : InputEvent.Move(dx, dy);
        }
        if (Mode != PointerMode.Follow) return null;
        var p = ToHead(px, py, panelW, panelH, headW, headH);
        return p is null ? null : InputEvent.At(p.Value.X, p.Value.Y);
    }

    /// <summary>In capture mode the first click only captures: <paramref name="captureNow"/> tells the view to grab.</summary>
    public InputEvent? OnButton(int button, bool down, out bool captureNow)
    {
        captureNow = false;
        if (Mode == PointerMode.Capture && !Captured)
        {
            if (down) { Captured = true; captureNow = true; }
            return null;
        }
        return InputEvent.Button(button, down);
    }

    /// <summary>When captured and Ctrl+Alt are down, frees the pointer and returns the key-ups to send.</summary>
    public List<InputEvent>? OnKeyState(out bool released)
    {
        released = false;
        if (Captured && _keys.CtrlDown && _keys.AltDown)
        {
            Captured = false;
            released = true;
            return _keys.ReleaseAll();
        }
        return null;
    }

    public void ForceRelease() => Captured = false;

    /// <summary>Focus lost: free the pointer and lift every key.</summary>
    public List<InputEvent> OnFocusLost()
    {
        Captured = false;
        return _keys.ReleaseAll();
    }
}
