//! The look that is ours: the Indigo accent, status pills, chassis colours and
//! the bezel around the guest's screen. Chrome, sidebars and system controls
//! stay libadwaita's.

use gtk::gdk;

const COMMON: &str = "
.pill { padding: 1px 9px; border-radius: 999px; font-weight: 600; font-size: 0.85em; }
.pill-grey { background: alpha(@window_fg_color, 0.12); }
.pill-green { background: alpha(@success_color, 0.20); color: @success_color; }
.pill-amber { background: alpha(@warning_color, 0.22); color: @warning_color; }
.pill-red { background: alpha(@error_color, 0.20); color: @error_color; }
.glyph { min-width: 34px; min-height: 34px; border-radius: 9px; color: white; }
.chassis-octane { background: #1f9e8f; }
.chassis-octane2 { background: #3b6fd4; }
.chassis-o2 { background: #6a58d6; }
.chassis-fuel { background: #cf3b36; }
.chassis-tezro { background: #7a4aa6; }
.chassis-origin { background: #5b6068; }
.chassis-onyx { background: #44525f; }
.chassis-other { background: #7b7b84; }
.tile { padding: 12px 14px; border-radius: 12px; background: alpha(@window_fg_color, 0.05); }
.tile-title { font-weight: 700; font-size: 1.05em; }
.hero { padding: 24px; border-radius: 18px; background: alpha(@accent_bg_color, 0.08); }
.screen-surround { background: #15171c; }
.hud { background: alpha(black, 0.65); color: white; padding: 8px 14px; border-radius: 10px; }
";

const LIGHT: &str = "@define-color accent_bg_color #4a52c4; @define-color accent_color #4a52c4; @define-color accent_fg_color #ffffff;";
const DARK: &str = "@define-color accent_bg_color #7c83f2; @define-color accent_color #7c83f2; @define-color accent_fg_color #0d0e1a;";

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
            provider.load_from_string(&format!("{accent}\n{COMMON}"));
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
