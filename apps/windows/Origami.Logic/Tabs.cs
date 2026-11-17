namespace Origami.Logic;

/// <summary>
/// The open tabs, one per machine. Selecting a machine shows it in the active tab, replacing that tab's machine unless the machine already
/// has a tab (then that tab is selected); opening in a new tab always adds one. Closing returns the tab to select next.
/// </summary>
public sealed class TabSet
{
    readonly List<string> _keys = new();
    public IReadOnlyList<string> Keys => _keys;
    public string? Active { get; private set; }
    (string Previous, string Replacement)? _replaced; // the last Select's replacement, so OpenNew can undo it

    public void Select(string key)
    {
        _replaced = null;
        if (_keys.Contains(key)) { Active = key; return; }
        var at = Active is null ? -1 : _keys.IndexOf(Active);
        if (at < 0) _keys.Add(key);
        else { _replaced = (_keys[at], key); _keys[at] = key; } // the active tab shows the newly selected machine
        Active = key;
    }

    /// <summary>
    /// Opens a machine in a tab of its own. If it only just replaced the active tab (the first click of a double click selects it first),
    /// that replacement is undone: the previous machine gets its tab back and this one a new tab beside it.
    /// </summary>
    public void OpenNew(string key)
    {
        if (_replaced is { } r && Active == key)
        {
            var at = _keys.IndexOf(key);
            _keys[at] = r.Previous;
            _keys.Insert(at + 1, key);
            _replaced = null;
            return;
        }
        _replaced = null;
        if (!_keys.Contains(key)) _keys.Add(key);
        Active = key;
    }

    /// <summary>Closes a tab; the neighbour to its left (else right) becomes active. Returns the new active key, or null when none is left.</summary>
    public string? Close(string key)
    {
        _replaced = null;
        var i = _keys.IndexOf(key);
        if (i < 0) return Active;
        _keys.RemoveAt(i);
        if (Active == key) Active = _keys.Count == 0 ? null : _keys[Math.Max(0, i - 1)];
        return Active;
    }

    /// <summary>Drops tabs for machines no longer in the library.</summary>
    public void Retain(IEnumerable<string> existing)
    {
        var keep = existing.ToHashSet();
        foreach (var k in _keys.Where(k => !keep.Contains(k)).ToList()) Close(k);
    }
}
