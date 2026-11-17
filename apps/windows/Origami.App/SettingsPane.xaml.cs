using Microsoft.UI.Xaml.Controls;
using Origami.Logic;

namespace Origami.App;

public sealed partial class SettingsPane : UserControl
{
    AppSettings _s = new();
    public event Action<AppSettings>? Saved;
    public event Action? Closed;

    public SettingsPane()
    {
        InitializeComponent();
        SaveButton.Click += (_, _) =>
        {
            _s.LibraryRoot = RootBox.Text.Trim();
            _s.DefaultPointerMode = ModeButtons.SelectedIndex == 1 ? PointerMode.Capture : PointerMode.Follow;
            _s.ConfirmQuitWithRunningMachines = ConfirmSwitch.IsOn;
            Saved?.Invoke(_s);
        };
        CloseButton.Click += (_, _) => Closed?.Invoke();
    }

    public void Load(AppSettings s)
    {
        _s = s;
        RootBox.Text = s.LibraryRoot;
        ModeButtons.SelectedIndex = s.DefaultPointerMode == PointerMode.Capture ? 1 : 0;
        ConfirmSwitch.IsOn = s.ConfirmQuitWithRunningMachines;
    }
}
