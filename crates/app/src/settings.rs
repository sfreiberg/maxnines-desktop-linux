//! What the app remembers between runs, in `~/.config/maxnines-desktop/settings.json`.
//! The refresh token is kept apart, in the keyring: see `secrets`.

use std::path::{Path, PathBuf};

use maxnines_desktop_kit::NotifyWhen;
use serde::{Deserialize, Serialize};

/// How often the server can be asked, in seconds.
pub const INTERVALS: [u32; 3] = [15, 30, 60];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The address the person opens MaxNines at, as they typed it.
    pub address: String,
    pub interval_secs: u32,
    pub notify_critical: bool,
    pub notify_warnings: bool,
    pub notify_resolved: bool,
    /// Who's signed in, and as what, while the app is.
    pub user: Option<String>,
    pub role: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            address: String::new(),
            interval_secs: INTERVALS[0],
            notify_critical: true,
            notify_warnings: true,
            notify_resolved: true,
            user: None,
            role: None,
        }
    }
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

fn path(config_home: &Path) -> PathBuf {
    config_home.join("maxnines-desktop").join("settings.json")
}

impl Settings {
    /// What was saved, or the defaults if nothing was or it can't be read.
    pub fn load() -> Self {
        Self::load_from(&config_home())
    }

    fn load_from(config_home: &Path) -> Self {
        let mut settings: Self = std::fs::read_to_string(path(config_home))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if !INTERVALS.contains(&settings.interval_secs) {
            settings.interval_secs = INTERVALS[0];
        }
        settings
    }

    /// Saves them, saying why not if it can't, which only costs remembering them.
    pub fn save(&self) {
        let path = path(&config_home());
        if let Err(error) = self.save_to(&path) {
            eprintln!("couldn't save the settings to {}: {error}", path.display());
        }
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    pub fn notify_when(&self) -> NotifyWhen {
        NotifyWhen {
            critical: self.notify_critical,
            warnings: self.notify_warnings,
            resolved: self.notify_resolved,
        }
    }
}

/// Starting when the person signs in, through a desktop entry in
/// `~/.config/autostart`, as every desktop reads.
pub mod autostart {
    use std::path::{Path, PathBuf};

    fn path(config_home: &Path) -> PathBuf {
        config_home
            .join("autostart")
            .join(format!("{}.desktop", crate::APP_ID))
    }

    pub fn is_on() -> bool {
        path(&super::config_home()).exists()
    }

    /// Turns it on or off, saying why if it couldn't.
    pub fn set(on: bool) -> Result<(), String> {
        // The program as it was started, so a copy run from anywhere starts itself.
        let program = std::env::current_exe().map_err(|error| error.to_string())?;
        set_in(&super::config_home(), on, &program).map_err(|error| error.to_string())
    }

    pub(super) fn set_in(config_home: &Path, on: bool, program: &Path) -> std::io::Result<()> {
        let path = path(config_home);
        if !on {
            return match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(&path, entry(program))
    }

    /// The desktop entry that starts it in the tray.
    fn entry(program: &Path) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=MaxNines Desktop\n\
             Comment=Your MaxNines server's alerts\nExec={} --background\n\
             Icon={}\nX-GNOME-Autostart-enabled=true\n",
            exec_argument(&program.to_string_lossy()),
            crate::APP_ID,
        )
    }

    /// `argument` as one argument of an `Exec=` line, quoted and escaped as the Desktop
    /// Entry spec says, so a program in a folder with any name starts. Inside the
    /// quotes, `"`, `` ` ``, `$` and `\` each take a backslash; then, as for any string
    /// in the file, every backslash is doubled and a line break is written `\n`; and
    /// `%` is doubled, as it would otherwise start a field code such as `%f`.
    pub(super) fn exec_argument(argument: &str) -> String {
        let mut quoted = String::from('"');
        for character in argument.chars() {
            match character {
                '"' | '`' | '$' => {
                    quoted.push_str("\\\\");
                    quoted.push(character);
                }
                '\\' => quoted.push_str("\\\\\\\\"),
                '%' => quoted.push_str("%%"),
                '\n' => quoted.push_str("\\n"),
                '\t' => quoted.push_str("\\t"),
                '\r' => quoted.push_str("\\r"),
                other => quoted.push(other),
            }
        }
        quoted.push('"');
        quoted
    }

    #[cfg(test)]
    pub(super) fn is_on_in(config_home: &Path) -> bool {
        path(config_home).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_come_back_as_saved_and_start_as_the_other_apps_do() {
        let home = tempfile::tempdir().unwrap();
        // Nothing saved yet: every notification on, asked every 15 seconds.
        let defaults = Settings::load_from(home.path());
        assert_eq!(defaults, Settings::default());
        assert_eq!(defaults.interval_secs, 15);
        assert!(defaults.notify_when().any());

        let changed = Settings {
            address: "https://monitor.example.com".into(),
            interval_secs: 60,
            notify_warnings: false,
            user: Some("sam@example.com".into()),
            role: Some("responder".into()),
            ..Settings::default()
        };
        changed.save_to(&path(home.path())).unwrap();
        assert_eq!(Settings::load_from(home.path()), changed);
        assert!(!changed.notify_when().warnings);
    }

    #[test]
    fn settings_that_cant_be_used_fall_back_to_the_defaults() {
        let home = tempfile::tempdir().unwrap();
        let file = path(home.path());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        // An interval the app doesn't offer, and a field it doesn't know.
        std::fs::write(&file, r#"{"interval_secs": 7, "theme": "dark"}"#).unwrap();
        assert_eq!(Settings::load_from(home.path()).interval_secs, 15);
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(Settings::load_from(home.path()), Settings::default());
    }

    #[test]
    fn opening_at_login_writes_and_removes_a_desktop_entry() {
        let home = tempfile::tempdir().unwrap();
        let program = Path::new("/usr/bin/maxnines-desktop");
        assert!(!autostart::is_on_in(home.path()));
        autostart::set_in(home.path(), true, program).unwrap();
        assert!(autostart::is_on_in(home.path()));
        let entry =
            std::fs::read_to_string(home.path().join("autostart/com.maxnines.Desktop.desktop"))
                .unwrap();
        assert!(entry.starts_with("[Desktop Entry]\n"), "{entry}");
        assert!(
            entry.contains("Exec=\"/usr/bin/maxnines-desktop\" --background\n"),
            "{entry}"
        );
        autostart::set_in(home.path(), false, program).unwrap();
        assert!(!autostart::is_on_in(home.path()));
        // Turning off what's already off is fine.
        autostart::set_in(home.path(), false, program).unwrap();
    }

    #[test]
    fn the_program_is_escaped_as_the_desktop_entry_spec_says() {
        assert_eq!(
            autostart::exec_argument("/usr/bin/maxnines-desktop"),
            r#""/usr/bin/maxnines-desktop""#
        );
        // Each of `"`, `` ` ``, `$` and `\` is escaped for the quotes and then again
        // for the file's strings, and `%` isn't read as a field code.
        assert_eq!(
            autostart::exec_argument(r#"/opt/My "Apps"/`x`/$HOME/a\b/100%/maxnines-desktop"#),
            r#""/opt/My \\"Apps\\"/\\`x\\`/\\$HOME/a\\\\b/100%%/maxnines-desktop""#
        );
        // A line break would otherwise end the line.
        assert_eq!(autostart::exec_argument("/tmp/a\nb"), r#""/tmp/a\nb""#);
    }
}
