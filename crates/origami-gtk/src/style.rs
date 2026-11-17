//! The look that is ours: the Indigo accent, status pills, the wash of each
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
.tile { padding: 12px 14px; border-radius: 12px; background: alpha(@window_fg_color, 0.05); }
.tile-title { font-weight: 700; font-size: 1.05em; }
.hero { padding: 24px; border-radius: 18px; background: alpha(@accent_bg_color, 0.08); }
.plate { padding: 14px 22px; border-radius: 14px; background: alpha(black, 0.55); }
.on-dark { color: #f2f3f5; }
.hud { background: alpha(black, 0.65); color: white; padding: 8px 14px; border-radius: 10px; }
";

const LIGHT: &str = "@define-color accent_bg_color #4a52c4; @define-color accent_color #4a52c4; @define-color accent_fg_color #ffffff;";
const DARK: &str = "@define-color accent_bg_color #7c83f2; @define-color accent_color #7c83f2; @define-color accent_fg_color #0d0e1a;";

/// `.wash-<still>`: a faint tint of the machine's measured colour.
fn washes() -> String {
    crate::art::colours()
        .map(|(id, c)| format!(".wash-{id} {{ background: alpha(#{c:06x}, 0.14); }}\n"))
        .collect()
}

pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    let load = {
        let provider = provider.clone();
        move || {
            let accent = if adw::StyleManager::default().is_dark() {
                DARK
            } else {
                LIGHT
            };
            provider.load_from_string(&format!("{accent}\n{COMMON}\n{}", washes()));
        }
    };
    load();
    adw::StyleManager::default().connect_dark_notify(move |_| load());
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
