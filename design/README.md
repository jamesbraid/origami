# Design system

The shared look and rules of the macOS, Windows and GTK apps. Open
`index.html` in a browser: it shows every colour role with its contrast, the
type ramp, spacing, and each element in its macOS, Windows and GNOME form,
light and dark, with what to do and what not to do.

| File | What it holds | Who reads it |
|---|---|---|
| `../resources/identity/tokens.toml` | sizes, spacing, radii, icon sizes, durations and Origami's own colours (granite) | `cargo xtask tokens`, which generates constants for all three apps and the core |
| `platform.toml` | what each platform calls a colour role, type role or icon, with reference values, and how each lays out its toolbar and status bar | this page only; never app code |
| `../resources/identity/stills/manifest.json` | chassis colours, measured from each machine's photograph | the core and this page |
| `index.html` | the reference page | people |
| `colour-baseline.txt` | colour literals the apps held when the check began | the check |

## How the apps use it

- **Numbers** come from the generated constants: `Tokens.Space.S` in Swift
  and C#, `{StaticResource Space.S}` in XAML, `tokens::space::S` in Rust.
  After editing `tokens.toml`, run `cargo xtask tokens`. It rewrites the
  generated files and the data block at the end of `index.html`.
- **Colours** come from the platform by role, as `platform.toml` names them:
  `NSColor.systemGreen`, `SystemFillColorSuccessBrush`, `@success_color`.
  The light and dark values there exist only to draw the swatches. The
  accent is always the user's own.
- **Words** come from the core (`origami-core`'s `ui_text.rs` and the
  facade's labels), never from an app.
- **Each platform follows its own conventions.** The commands, words and
  states are shared; where they sit is not. The toolbar is macOS's unified
  toolbar, Windows's title bar with a command bar in the content, and
  GNOME's header bar, as `[toolbar.*]` in `platform.toml` lays them out.
  Never copy one platform's layout into another.
- **Window layout.** One sidebar at the leading edge holds the library, or
  the install guide while an install runs, with a button back to the
  library. The tab bar shows only when two or more machines are open. The
  status bar (`size.status_bar_height`) shows the picture size and pointer
  mode only while the machine runs, under the content and not the sidebar.
- Keys marked "Not yet used" in `tokens.toml` are agreed but not wired into
  the apps yet. Each one is adopted as the apps move to the redesign.

## The check

`cargo xtask tokens --check` runs in the Bindings and GTK workflows. It
fails when:
- the generated files or `index.html`'s data are stale;
- an app gains a colour literal: `0xRRGGBB`, `#RRGGBB`, `#AARRGGBB` or
  `Color.FromArgb(...)` in `apps/macos/Sources`, `apps/windows` or
  `crates/origami-gtk/src`, outside generated code, self-tests and tests;
- `colour-baseline.txt` lists a literal the code no longer has. Run
  `cargo xtask tokens` to shrink it.

The baseline only shrinks by itself. Add to it by hand, with the reason in
the commit message, only for a colour that no platform colour or token can
give: for example the terminal's ANSI palette, which is the guest's, not
ours.

Sizes are not scanned. Bare numbers are too common in layout code for a
scan to tell a spacing value from anything else, so sizes are left to
review.

## Review checklist

A change to what people see answers each point in its pull request:

1. Every word comes from the core, in sentence case, with no `·`, no
   command line, file format or path, and never "Unknown".
2. State is a dot and its word, in the library row, title-bar subtitle, tab
   and Diagnostics header only, never in the status bar. Tabs show only with
   two or more machines.
3. Sizes come from tokens, colours from the platform by role or from the
   chassis manifest. `cargo xtask tokens --check` passes.
4. Forms label every field, put units after values and apply at once.
   Dialogs follow the platform's button order. Empty states have an image,
   a title, one sentence and at most one button.
5. All three apps change together and each follows its own platform's
   conventions, toolbar included.
6. The pull request shows all three apps, light and dark.

## Adding an element

1. Draw it in `index.html`: add a `section(...)` call with a render function
   that takes the platform and mode and uses only the stage's variables.
   Add the dos and don'ts beside it.
2. If it needs a new number, add it to `tokens.toml` under the right table
   with a "Not yet used" comment. If it needs a new colour role, type role
   or icon, add it to `platform.toml` with each platform's name.
3. Run `cargo xtask tokens`, open `index.html` and check all six stages.
4. Build it in all three apps in the same change, from the generated
   constants and the named platform colours.
