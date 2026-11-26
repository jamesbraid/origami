//! The look that is ours: the state dot, the fade of each machine's measured
//! colour and the plate under text on granite. Chrome, sidebars and system
//! controls stay libadwaita's.

use gtk::gdk;
use gtk::prelude::*;
use origami_core::facade::Tone;

const COMMON: &str = "
.state-dot { min-width: 8px; min-height: 8px; border-radius: 999px; background-color: currentColor; }
.glyph { min-width: 34px; min-height: 34px; border-radius: 9px; color: white; background: #7b7b84; }
.tile { padding: 0 8px; border-radius: 10px; background: @card_bg_color; }
.product-line { color: alpha(@window_fg_color, 0.7); }
.tile-title { font-weight: 700; font-size: 1.05em; }
tabbar .box { padding: 0; background: @window_bg_color; box-shadow: inset 0 -1px alpha(@window_fg_color, 0.15); }
tabbar tabbox { padding: 4px 8px; min-height: 26px; }
tabbar tab { min-height: 26px; padding: 0 8px; border-radius: 6px; }
tabbar tab > image { -gtk-icon-size: 20px; }
tabbar tab button.image-button { min-width: 20px; min-height: 20px; }
tabbar tab:hover { background: alpha(@window_fg_color, 0.07); }
.hero { padding: 24px; }
/* The hero still at its own size, not a status page icon's. */
statuspage.machine-art image.icon { -gtk-icon-size: 240px; }
tabbar.flat .box { box-shadow: none; }
.start-surround { background-image: linear-gradient(to bottom, #282a30, #1f2126); }
.scrim { background: alpha(black, 0.45); }
.plate { padding: 14px 22px; border-radius: 14px; background: alpha(black, 0.55); }
.cpu-panel { background: #1c1c20; padding: 8px 14px; }
.on-dark { color: #f2f3f5; }
.hud { background: alpha(black, 0.65); color: white; padding: 8px 14px; border-radius: 10px; }
";

/// `.fade-<still>`: the machine's measured colour at 35% at the top of a stopped
/// machine's pane, fading to the window background (and no edge) at the bottom.
fn fades() -> String {
    crate::art::colours()
        .map(|(id, c)| format!(".fade-{id} {{ background-color: @window_bg_color; background-image: linear-gradient(to bottom, alpha(#{c:06x}, 0.35), alpha(#{c:06x}, 0)); }}\n"))
        .collect()
}

/// `tabbar.chassis-<still> tab:selected`: the selected tab tinted and outlined in the machine's colour.
fn tab_tints() -> String {
    crate::art::colours()
        .map(|(id, c)| {
            format!("tabbar.chassis-{id} tab:selected {{ background: alpha(#{c:06x}, 0.22); box-shadow: inset 0 0 0 1px alpha(#{c:06x}, 0.5); }}\n")
        })
        .collect()
}

/// The style class that colours a tone, from libadwaita's own semantic colours.
fn tone_class(tone: Tone) -> &'static str {
    match tone {
        Tone::None => "dim-label",
        Tone::Success => "success",
        Tone::Caution => "warning",
        Tone::Critical => "error",
    }
}

/// A dot in the tone's colour, drawn by CSS so it follows light, dark and high contrast.
pub fn dot(tone: Tone) -> gtk::Box {
    let d = gtk::Box::builder()
        .css_classes(["state-dot", tone_class(tone)])
        .valign(gtk::Align::Center)
        .build();
    d.set_accessible_role(gtk::AccessibleRole::Presentation);
    d
}

/// Recolour a dot made by [`dot`].
pub fn set_dot(d: &gtk::Box, tone: Tone) {
    for t in [Tone::None, Tone::Success, Tone::Caution, Tone::Critical] {
        d.remove_css_class(tone_class(t));
    }
    d.add_css_class(tone_class(tone));
}

/// The tone's colour as a 24-bit RGB, read from the theme, for a tab's indicator.
pub fn tone_rgb(tone: Tone) -> u32 {
    let probe = gtk::Label::builder()
        .css_classes([tone_class(tone)])
        .build();
    let c = probe.color();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(c.red()) << 16 | byte(c.green()) << 8 | byte(c.blue())
}

pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!("{COMMON}\n{}{}", fades(), tab_tints()));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
