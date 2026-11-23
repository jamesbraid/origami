using uniffi.origami_ffi;

namespace Origami.App;

/// <summary>Words every app shows in the same places; the core owns the text.</summary>
internal static class Ui
{
    internal static string T(OrigamiUiString id) => OrigamiFfiMethods.OrigamiUiString(id);
}
