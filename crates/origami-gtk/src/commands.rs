//! Menu and toolbar commands. The core's table owns the ones it knows
//! (identifier, title, shortcut, when it is enabled); the app adds the ones
//! only a GTK front end has. Windows, menus, shortcuts and the automation
//! socket all go through the identifiers here.

use origami_core::facade::{
    show_in_label, Command as CoreCommand, Core, EnabledWhen, Platform, UiString,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Machine,
    View,
    Window,
    App,
}

#[derive(Clone)]
pub struct Command {
    pub id: String,
    pub title: String,
    /// What it does, ending with its shortcut, from the core's table.
    pub tooltip: String,
    /// GTK accelerator strings.
    pub accels: Vec<String>,
    pub group: Group,
    /// Set for the core's commands; the app's own are enabled by window code.
    pub when: Option<EnabledWhen>,
}

/// `Ctrl+Shift+S` as GTK writes it, `<Control><Shift>s`.
pub fn gtk_accel(accelerator: &str) -> String {
    let mut parts: Vec<&str> = accelerator.split('+').collect();
    // A trailing "+" key splits into an empty last part.
    let key = parts.pop().unwrap_or("");
    let mut out = String::new();
    for m in parts {
        out.push_str(match m {
            "Ctrl" => "<Control>",
            "Shift" => "<Shift>",
            "Alt" => "<Alt>",
            "Cmd" => "<Super>",
            other => other,
        });
    }
    out.push_str(match key {
        "." => "period",
        "," => "comma",
        "?" => "question",
        k if k.len() == 1 => return out + &k.to_lowercase(),
        k => k,
    });
    out
}

fn from_core(c: CoreCommand) -> Command {
    let group = match c.id.split('.').next() {
        Some("app") => Group::App,
        Some("view") => Group::View,
        Some("window") => Group::Window,
        _ => Group::Machine,
    };
    Command {
        tooltip: c.tooltip(Platform::Linux),
        accels: c
            .shortcuts
            .iter()
            .filter(|s| s.platform == Platform::Linux)
            .map(|s| gtk_accel(&s.accelerator))
            .collect(),
        id: c.id,
        title: c.title,
        group,
        when: Some(c.enabled_when),
    }
}

fn extra(id: &str, title: &str, accels: &[&str], group: Group) -> Command {
    Command {
        id: id.into(),
        title: title.into(),
        tooltip: String::new(),
        accels: accels.iter().map(|a| (*a).into()).collect(),
        group,
        when: None,
    }
}

/// The commands, the core's first.
pub fn table(core: &Core) -> Vec<Command> {
    let mut out: Vec<Command> = core.commands().into_iter().map(from_core).collect();
    // The core's Capture mouse has no Linux shortcut; this app has had one.
    if let Some(c) = out.iter_mut().find(|c| c.id == "machine.capture-mouse") {
        c.accels.push("<Control><Shift>g".into());
    }
    out.extend([
        extra(
            "machine.pointer-follow",
            UiString::FollowMyMouse.text(),
            &[],
            Group::Machine,
        ),
        extra(
            "machine.pointer-capture",
            UiString::CaptureOnClick.text(),
            &[],
            Group::Machine,
        ),
        extra(
            "card.show-folder",
            &show_in_label(Platform::Linux),
            &[],
            Group::Machine,
        ),
        extra("view.show-library", "Show library", &["F9"], Group::View),
        extra("view.full-screen", "Full screen", &["F11"], Group::View),
        extra("view.fit", "Fit", &["<Control>9"], Group::View),
        extra(
            "view.actual-size",
            "Actual size",
            &["<Control>0"],
            Group::View,
        ),
        extra("view.scale-1x", "1×", &["<Control><Alt>1"], Group::View),
        extra("view.scale-2x", "2×", &["<Control><Alt>2"], Group::View),
        extra(
            "window.move-to-new-window",
            "Move tab to new window",
            &[],
            Group::Window,
        ),
        extra(
            "window.close-tab",
            "Close tab",
            &["<Control>w"],
            Group::Window,
        ),
        extra(
            "app.preferences",
            "Preferences",
            &["<Control>comma"],
            Group::App,
        ),
        extra("app.about", "About Origami", &[], Group::App),
    ]);
    out
}

/// The action name windows register a command under.
pub fn action(id: &str) -> String {
    format!("win.{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_accelerators_become_gtk_ones() {
        assert_eq!(gtk_accel("Ctrl+Shift+S"), "<Control><Shift>s");
        assert_eq!(gtk_accel("Alt+F4"), "<Alt>F4");
        assert_eq!(gtk_accel("Ctrl+."), "<Control>period");
        assert_eq!(gtk_accel("Ctrl+N"), "<Control>n");
    }
}
