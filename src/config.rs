use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub default_profile: String,
    pub profiles: Vec<Profile>,
    pub bookmarks: Vec<Bookmark>,
    pub schemes: Vec<Scheme>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub font_family: String,
    pub font_size: f32,
    /// 1.0 is opaque. Lower values show the desktop through the cell background.
    pub opacity: f32,
    pub scrollback: usize,
    /// Empty uses `$SHELL`.
    pub command: String,
    /// Empty inherits the current directory.
    pub working_directory: String,
    pub scheme: String,
    pub cursor_shape: String,
    pub copy_on_select: bool,
    pub notify_on_bell: bool,
    pub notify_on_exit: bool,
    pub notify_on_activity: bool,
    pub padding: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub directory: String,
    pub command: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scheme {
    pub name: String,
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub selection: String,
    /// 16 ANSI colors, normal then bright.
    pub ansi: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_profile: "Default".into(),
            profiles: vec![Profile::default()],
            bookmarks: vec![Bookmark {
                name: "Home".into(),
                directory: std::env::var("HOME").unwrap_or_else(|_| "/".into()),
                command: String::new(),
            }],
            schemes: vec![Scheme::breeze_dark(), Scheme::breeze_light(), Scheme::solarized_dark()],
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            font_family: "Hack".into(),
            font_size: 13.0,
            opacity: 1.0,
            scrollback: 10_000,
            command: String::new(),
            working_directory: String::new(),
            scheme: "Breeze Dark".into(),
            cursor_shape: "block".into(),
            copy_on_select: true,
            notify_on_bell: true,
            notify_on_exit: true,
            notify_on_activity: false,
            padding: 6.0,
        }
    }
}

impl Scheme {
    pub fn breeze_dark() -> Self {
        Self {
            name: "Breeze Dark".into(),
            foreground: "#fcfcfc".into(),
            background: "#1b1e20".into(),
            cursor: "#3daee9".into(),
            selection: "#3daee9".into(),
            ansi: vec![
                "#232627", "#ed1515", "#11d116", "#f67400", "#1d99f3", "#9b59b6", "#1abc9c",
                "#fcfcfc", "#7f8c8d", "#c0392b", "#1cdc9a", "#fdbc4b", "#3daee9", "#8e44ad",
                "#16a085", "#ffffff",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }

    pub fn breeze_light() -> Self {
        Self {
            name: "Breeze Light".into(),
            foreground: "#232627".into(),
            background: "#fcfcfc".into(),
            cursor: "#3daee9".into(),
            selection: "#93cee9".into(),
            ansi: vec![
                "#232627", "#ed1515", "#11d116", "#f67400", "#1d99f3", "#9b59b6", "#1abc9c",
                "#fcfcfc", "#7f8c8d", "#c0392b", "#1cdc9a", "#fdbc4b", "#3daee9", "#8e44ad",
                "#16a085", "#ffffff",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }

    pub fn solarized_dark() -> Self {
        Self {
            name: "Solarized Dark".into(),
            foreground: "#839496".into(),
            background: "#002b36".into(),
            cursor: "#93a1a1".into(),
            selection: "#073642".into(),
            ansi: vec![
                "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198",
                "#eee8d5", "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4",
                "#93a1a1", "#fdf6e3",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }

    pub fn bg_rgba(&self, opacity: f32) -> [f32; 4] {
        let [r, g, b] = parse_hex(&self.background);
        [r, g, b, opacity.clamp(0.05, 1.0)]
    }

    pub fn fg(&self) -> [f32; 3] {
        parse_hex(&self.foreground)
    }

    pub fn cursor(&self) -> [f32; 3] {
        parse_hex(&self.cursor)
    }

    pub fn selection(&self) -> [f32; 3] {
        parse_hex(&self.selection)
    }

    pub fn ansi(&self, index: usize) -> [f32; 3] {
        self.ansi
            .get(index)
            .map(|s| parse_hex(s))
            .unwrap_or([1.0, 1.0, 1.0])
    }
}

pub fn parse_hex(s: &str) -> [f32; 3] {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return [1.0, 1.0, 1.0];
    }
    let Ok(n) = u32::from_str_radix(s, 16) else {
        return [1.0, 1.0, 1.0];
    };
    [
        ((n >> 16) & 0xff) as f32 / 255.0,
        ((n >> 8) & 0xff) as f32 / 255.0,
        (n & 0xff) as f32 / 255.0,
    ]
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
            });
        base.join("hack-shell").join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(cfg) => cfg,
                Err(err) => {
                    log::warn!("config parse failed ({err}), using defaults");
                    Self::default()
                },
            },
            Err(_) => {
                let cfg = Self::default();
                let _ = cfg.save();
                cfg
            },
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        fs::write(path, text)
    }

    pub fn profile(&self, name: &str) -> Profile {
        self.profiles
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .unwrap_or_else(|| self.profiles.first().cloned().unwrap_or_default())
    }

    pub fn scheme(&self, name: &str) -> Scheme {
        self.schemes
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .unwrap_or_else(Scheme::breeze_dark)
    }
}
