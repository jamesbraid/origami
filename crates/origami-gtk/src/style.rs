//! The look that is ours: status pills, the fade of each
//! machine's measured colour and the plate under text on granite. Chrome, sidebars and
//! system controls stay libadwaita's.

use gtk::gdk;

const COMMON: &str = "
.pill { padding: 1px 9px; border-radius: 999px; font-weight: 600; font-size: 0.85em; }
.pill-grey { background: alpha(@window_fg_color, 0.12); }
.pill-green { background: alpha(@success_color, 0.20); color: @success_color; }
.pill-amber { background: alpha(@warning_color, 0.22); color: @warning_color; }
.pill-red { background: alpha(@error_color, 0.20); color: @error_color; }
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
tabbar.flat .box { box-shadow: none; }
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
