//! What the app remembers between runs, in `~/.config/maxnines-desktop/settings.json`.
//! The refresh token is kept apart, in the keyring: see `secrets`.

use std::path::PathBuf;

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

fn path() -> PathBuf {
    config_home().join("maxnines-desktop").join("settings.json")
}

impl Settings {
    /// What was saved, or the defaults if nothing was or it can't be read.
    pub fn load() -> Self {
        let mut settings: Self = std::fs::read_to_string(path())
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
        let path = path();
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                let text = serde_json::to_string_pretty(self).unwrap_or_default();
                std::fs::write(&path, text)
            });
        if let Err(error) = written {
            eprintln!("couldn't save the settings to {}: {error}", path.display());
        }
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
    use std::path::PathBuf;

    fn path() -> PathBuf {
        super::config_home()
            .join("autostart")
            .join(format!("{}.desktop", crate::APP_ID))
    }

    pub fn is_on() -> bool {
        path().exists()
    }

    /// Turns it on or off, saying why if it couldn't.
    pub fn set(on: bool) -> Result<(), String> {
        let path = path();
        if !on {
            return match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(error.to_string())
                }
                _ => Ok(()),
            };
        }
        // The program as it was started, so a copy run from anywhere starts itself.
        let program = std::env::current_exe().map_err(|error| error.to_string())?;
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=MaxNines Desktop\n\
             Comment=Your MaxNines server's alerts\nExec=\"{}\" --background\n\
             Icon={}\nX-GNOME-Autostart-enabled=true\n",
            program.display(),
            crate::APP_ID,
        );
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, entry))
            .map_err(|error| error.to_string())
    }
}
