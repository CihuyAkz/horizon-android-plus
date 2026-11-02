//! A pack's files in memory, so rewrites can be tried and undone, and turned back into a zip.

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use horizon_shaders::Pack;

pub struct Files {
    /// The pack's file or folder name.
    pub name: String,
    /// Every file by its path from the pack's root (`shaders/...`, `LICENSE`, ...).
    pub entries: BTreeMap<String, Vec<u8>>,
    /// Changes made while reading (a folder around `shaders/` taken off, ...), for the report.
    pub layout_notes: Vec<String>,
}

impl Files {
    pub fn read(path: &Path) -> Result<Self, String> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut entries = BTreeMap::new();
        if path.is_dir() {
            let mut total = 0u64;
            walk(path, path, &mut entries, &mut total)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        } else {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
                .map_err(|e| format!("{}: not a readable zip: {e}", path.display()))?;
            for i in 0..zip.len() {
                let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
                if f.is_dir() {
                    continue;
                }
                let Some(p) = f.enclosed_name() else { continue };
                let key = p.to_string_lossy().replace('\\', "/");
                let mut data = Vec::new();
                f.read_to_end(&mut data).map_err(|e| e.to_string())?;
                entries.insert(key, data);
            }
        }
        let mut files = Files {
            name,
            entries,
            layout_notes: Vec::new(),
        };
        files.unwrap_folder();
        if !files.entries.keys().any(|k| k.starts_with("shaders/")) {
            return Err(format!("{}: no shaders/ folder: not an Iris/OptiFine pack", files.name));
        }
        Ok(files)
    }

    /// A zip that wraps the pack in one more folder (`Pack v1/shaders/...`) is moved up, since
    /// Horizon, like Iris, wants `shaders/` at the root.
    fn unwrap_folder(&mut self) {
        if self.entries.keys().any(|k| k.starts_with("shaders/")) {
            return;
        }
        let Some(prefix) = self
            .entries
            .keys()
            .find_map(|k| k.find("/shaders/").map(|i| k[..=i].to_owned()))
        else {
            return;
        };
        self.entries = std::mem::take(&mut self.entries)
            .into_iter()
            .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|s| (s.to_owned(), v)))
            .collect();
        self.layout_notes.push(format!(
            "The pack was inside a `{prefix}` folder; moved up so `shaders/` is at the root."
        ));
    }

    /// The text of `shaders/<path>`.
    pub fn text(&self, shader_path: &str) -> Option<String> {
        self.entries
            .get(&format!("shaders/{shader_path}"))
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn set_text(&mut self, shader_path: &str, text: String) {
        self.entries.insert(format!("shaders/{shader_path}"), text.into_bytes());
    }

    pub fn remove(&mut self, shader_path: &str) -> Option<Vec<u8>> {
        self.entries.remove(&format!("shaders/{shader_path}"))
    }

    pub fn restore(&mut self, shader_path: &str, data: Option<Vec<u8>>) {
        let key = format!("shaders/{shader_path}");
        match data {
            Some(d) => {
                self.entries.insert(key, d);
            }
            None => {
                self.entries.remove(&key);
            }
        }
    }

    /// The pack as Horizon opens it.
    pub fn pack(&self) -> Result<Pack, String> {
        let bytes = self.zip_bytes(zip::CompressionMethod::Stored)?;
        Pack::from_zip_bytes(&self.name, bytes).map_err(|e| e.to_string())
    }

    pub fn write_zip(&self, to: &Path) -> Result<(), String> {
        let bytes = self.zip_bytes(zip::CompressionMethod::Deflated)?;
        std::fs::write(to, bytes).map_err(|e| format!("{}: {e}", to.display()))
    }

    fn zip_bytes(&self, method: zip::CompressionMethod) -> Result<Vec<u8>, String> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(method);
        for (k, v) in &self.entries {
            w.start_file(k.as_str(), opts).map_err(|e| e.to_string())?;
            w.write_all(v).map_err(|e| e.to_string())?;
        }
        Ok(w.finish().map_err(|e| e.to_string())?.into_inner())
    }
}

const MAX_PACK_BYTES: u64 = 96 << 20;

fn walk(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, Vec<u8>>,
    total: &mut u64,
) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        let metadata = std::fs::symlink_metadata(&p)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "shader pack contains a symbolic link",
            ));
        }
        if metadata.is_dir() {
            walk(root, &p, out, total)?;
        } else if metadata.is_file() {
            *total = (*total).checked_add(metadata.len()).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "shader pack size overflow")
            })?;
            if *total > MAX_PACK_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "shader pack exceeds Android's 96 MiB conversion limit",
                ));
            }
            let key = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(key, std::fs::read(&p)?);
        }
    }
    Ok(())
}
