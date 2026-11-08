use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub thumb: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    pub created_at: i64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub hash: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkey: String,
    pub history_size: usize,
    pub paste_automatically: bool,
    pub launch_at_login: bool,
    pub monitoring: bool,
    pub theme: String,
    /// Accent palette: blue | indigo | purple | pink | red | orange | green | graphite
    pub accent: String,
    /// Where the popup appears: cursor | top_right | bottom_right | center | custom
    pub position: String,
    /// Last position in physical pixels, used when `position == "custom"`.
    pub pos_x: i32,
    pub pos_y: i32,
    /// Window sizes in logical pixels; 0 means "use the built-in default".
    pub popup_w: f64,
    pub popup_h: f64,
    pub settings_w: f64,
    pub settings_h: f64,
    /// Keep the popup on screen after copying and after losing focus.
    /// Esc, the hotkey and pasting still close it.
    pub keep_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: "Alt+V".into(),
            history_size: 200,
            paste_automatically: true,
            launch_at_login: false,
            monitoring: true,
            theme: "system".into(),
            accent: "blue".into(),
            position: "cursor".into(),
            pos_x: 0,
            pos_y: 0,
            popup_w: 0.0,
            popup_h: 0.0,
            settings_w: 0.0,
            settings_h: 0.0,
            keep_open: false,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub items: Vec<Item>,
    pub settings: Settings,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            settings: Settings::default(),
            dir: PathBuf::new(),
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One-time handover from the previous name/identifier (`app.wincy.clipboard`).
/// Copies history and images into the new data dir the first time we start,
/// then rewrites the absolute image paths so they survive deleting the old dir.
pub fn migrate_legacy(new_dir: &Path) {
    if new_dir.join("history.json").exists() {
        return; // already have data (migrated earlier, or started fresh)
    }
    let Some(parent) = new_dir.parent() else {
        return;
    };
    let legacy = parent.join("app.wincy.clipboard");
    if !legacy.is_dir() || copy_dir(&legacy, new_dir).is_err() {
        return;
    }

    let path = new_dir.join("history.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let from = legacy.to_string_lossy().replace('\\', "\\\\");
    let to = new_dir.to_string_lossy().replace('\\', "\\\\");
    let patched = text.replace(&from, &to);
    if patched != text {
        let _ = fs::write(&path, patched);
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            let _ = fs::copy(entry.path(), &target);
        }
    }
    Ok(())
}

impl Store {
    pub fn load(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(dir.join("images"));
        let path = dir.join("history.json");
        let mut store: Store = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        store.dir = dir;
        // Drop entries whose image file vanished.
        store.items.retain(|i| match &i.file {
            Some(p) => Path::new(p).exists(),
            None => true,
        });
        store
    }

    pub fn save(&self) {
        let path = self.dir.join("history.json");
        if let Ok(json) = serde_json::to_string(self) {
            let tmp = self.dir.join("history.json.tmp");
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }

    pub fn image_path(&self, id: &str) -> PathBuf {
        self.dir.join("images").join(format!("{}.png", id))
    }

    pub fn add(&mut self, mut item: Item) {
        // Same content copied again: move it to the top instead of duplicating.
        if let Some(pos) = self.items.iter().position(|i| i.hash == item.hash) {
            let old = self.items.remove(pos);
            item.pinned = old.pinned || item.pinned;
            item.created_at = now_ms();
            if let Some(path) = old.file {
                if item.file.as_deref() != Some(path.as_str()) {
                    let _ = fs::remove_file(path);
                }
            }
        }
        self.items.insert(0, item);
        self.trim();
        self.save();
    }

    /// Drops the oldest unpinned entries beyond `history_size`. A value of 0
    /// means "no limit" — nothing is ever dropped automatically.
    pub fn trim(&mut self) {
        let limit = self.settings.history_size;
        if limit == 0 {
            return;
        }
        let limit = limit.max(1);
        let mut unpinned = self.items.iter().filter(|i| !i.pinned).count();
        if unpinned <= limit {
            return;
        }
        let mut idx = self.items.len();
        while unpinned > limit && idx > 0 {
            idx -= 1;
            if !self.items[idx].pinned {
                let removed = self.items.remove(idx);
                if let Some(p) = removed.file {
                    let _ = fs::remove_file(p);
                }
                unpinned -= 1;
            }
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        if let Some(pos) = self.items.iter().position(|i| i.id == id) {
            let it = self.items.remove(pos);
            if let Some(p) = it.file {
                let _ = fs::remove_file(p);
            }
            self.save();
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self, include_pinned: bool) {
        self.items.retain(|i| {
            let keep = !include_pinned && i.pinned;
            if !keep {
                if let Some(p) = &i.file {
                    let _ = fs::remove_file(p);
                }
            }
            keep
        });
        self.save();
    }

    pub fn toggle_pin(&mut self, id: &str) -> Option<bool> {
        let pos = self.items.iter().position(|i| i.id == id)?;
        let pinned = !self.items[pos].pinned;
        self.items[pos].pinned = pinned;
        self.save();
        Some(pinned)
    }
}

pub fn hash_of(kind: &str, text: &str, png: Option<&[u8]>, files: &[String]) -> u64 {
    let mut h = DefaultHasher::new();
    kind.hash(&mut h);
    text.hash(&mut h);
    files.hash(&mut h);
    if let Some(p) = png {
        p.hash(&mut h);
    }
    h.finish()
}

pub fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}
