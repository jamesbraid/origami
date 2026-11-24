# One source of UI truth

The macOS, Windows and GTK apps share what they can and differ only where the
platform asks for it. A difference that is in neither place is a bug.

1. **Logic, state and words live in the core.** If two apps would write an
   `if` or a sentence about it, the core owns it and the apps display it.
2. **Shared numbers live in `resources/identity/tokens.toml`.** Run
   `cargo xtask tokens` after editing; it writes Rust constants into
   `origami-core` and Swift, C# and XAML files into the apps. CI runs
   `cargo xtask tokens --check` and fails on stale output. Only values at least
   two apps need belong there. It holds agreed values: some apps use them
   already, the rest adopt them as their pull requests land, and each
   unused key says so. Units are points, DIPs and logical pixels.
3. **Everything else is native by design:** controls, fonts, window chrome,
   dialog button order, menu placement, status shades, icons and the accent
   colour (each platform's own). Mark a deliberate difference in code with a
   one-line `native: <reason>` comment.

Shortcuts are behaviour, not tokens: they stay in the core command table.

The design system in [`design/`](../design/README.md) shows every token,
colour role and element, and its check stops new hard-coded colours.

## Decided

- The tab bar shows only when two or more machines are open. The status bar
  shows only while a machine runs. Each platform keeps its own toolbar
  layout. [`design/`](../design/README.md) draws all three.
- Paused is amber.
- "No picture" appears after 30 s on every platform.
- A fixed scale that does not fit falls back to the same smooth Fit everywhere.
- Screenshots come from the guest framebuffer.
- The summary pane fades from the chassis tint to the window background
  (`pane` in the tokens file).

## Not yet moved

Status words and tones, dialog copy, command ids, product names, scale layout,
screenshots and summary cards still live in each app. They move into the core
one at a time, each with a shared test, as the app pull requests land.
