namespace Origami.Logic;

/// <summary>Spots a word typed one letter at a time, ignoring case; any other letter starts the word over.</summary>
public sealed class WordTyped
{
    readonly string _word;
    int _at;

    public WordTyped(string word) { _word = word.ToLowerInvariant(); }

    /// <summary>True when this letter completes the word.</summary>
    public bool Feed(char letter)
    {
        letter = char.ToLowerInvariant(letter);
        if (letter == _word[_at])
        {
            if (++_at < _word.Length) return false;
            _at = 0;
            return true;
        }
        _at = letter == _word[0] ? 1 : 0;
        return false;
    }
}
