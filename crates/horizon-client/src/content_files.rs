//! Local add-on files. Archive paths and expansion limits are checked before pack installation.
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mods,
    Packs,
    Shaders,
}
impl Kind {
    pub const ALL: [Self; 3] = [Self::Mods, Self::Packs, Self::Shaders];
    pub fn folder(self) -> &'static str {
        match self {
            Self::Mods => "horizon-mods",
            Self::Packs => "resourcepacks",
            Self::Shaders => "shaderpacks",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Mods => "Mods",
            Self::Packs => "Resource Packs",
            Self::Shaders => "Shaders",
        }
    }
    pub fn accepts(self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        match self {
            Self::Mods => lower.ends_with(".hmod"),
            Self::Packs => lower.ends_with(".zip"),
            Self::Shaders => lower.ends_with(".zip") || lower.ends_with(".hshader"),
        }
    }
}
pub const MAX_INPUT: u64 = 256 << 20;
const MAX_EXPANDED: u64 = 512 << 20;
fn shader_source_file(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [".vsh", ".fsh", ".glsl", ".csh", ".gsh", ".tcs", ".tes"]
        .iter()
        .any(|extension| path.ends_with(extension))
}
fn shader_pack_prefix(path: &str) -> Option<&str> {
    if path.starts_with("shaders/") {
        Some("")
    } else {
        path.find("/shaders/").map(|index| &path[..=index])
    }
}
pub fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 180
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '\0'])
        && !name.chars().any(char::is_control)
        && matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
        && Path::new(name).components().count() == 1
}
pub fn validate(
    path: &Path,
    kind: Kind,
    name: &str,
    cancelled: impl Fn() -> bool,
) -> crate::Result<bool> {
    if !plain_name(name) || !kind.accepts(name) {
        return Err(
            "Choose a ZIP pack, .hshader shader pack or sandboxed .hmod matching this folder"
                .into(),
        );
    }
    let file = std::fs::File::open(path)?;
    if kind == Kind::Mods && file.metadata()?.len() > 32 << 20 {
        return Err("Horizon mod exceeds 32 MiB".into());
    }
    if file.metadata()?.len() > MAX_INPUT {
        return Err("Import exceeds 256 MiB".into());
    }
    let mut zip = zip::ZipArchive::new(file)?;
    if zip.len() > 32768 {
        return Err("Archive contains too many files".into());
    }
    let mut names = HashSet::new();
    let mut total = 0u64;
    let mut pack = false;
    let mut shader = false;
    let mut native_shader = false;
    let mut manifest = false;
    let mut wasm = false;
    let mut buffer = [0u8; 65536];
    for i in 0..zip.len() {
        if cancelled() {
            return Err("Import cancelled".into());
        }
        let mut entry = zip.by_index(i)?;
        let raw = entry.name().to_owned();
        if raw.is_empty()
            || raw.len() > 1024
            || raw.chars().any(char::is_control)
            || raw.contains(['\\', ':', '\0'])
            || raw.starts_with('/')
            || raw
                .trim_end_matches('/')
                .split('/')
                .any(|part| part.is_empty() || part == ".." || part == ".")
            || !names.insert(raw.clone())
            || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err("Archive contains an unsafe or duplicate path".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Archive size overflow")?;
        if total > MAX_EXPANDED || kind == Kind::Mods && total > 64 << 20 {
            return Err("Expanded archive exceeds 512 MiB".into());
        }
        pack |= raw == "pack.mcmeta";
        native_shader |= raw == "horizon-shaders.toml";
        shader |= !entry.is_dir()
            && shader_source_file(&raw)
            && shader_pack_prefix(&raw).is_some();
        manifest |= raw == "horizon-mod.toml";
        wasm |= raw == "mod.wasm";
        let expected = entry.size();
        if raw == "pack.mcmeta" && expected > 1 << 20 {
            return Err("Pack metadata is too large".into());
        }
        let mut metadata = Vec::new();
        let mut actual = 0u64;
        while !entry.is_dir() {
            if cancelled() {
                return Err("Import cancelled".into());
            }
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            actual += n as u64;
            if raw == "pack.mcmeta" {
                metadata.extend_from_slice(&buffer[..n]);
            }
            if actual > expected || actual > MAX_EXPANDED {
                return Err("Archive size mismatch".into());
            }
        }
        if !entry.is_dir() && actual != expected {
            return Err("Incomplete archive entry".into());
        }
        if raw == "pack.mcmeta"
            && !serde_json::from_slice::<serde_json::Value>(&metadata)?
                .get("pack")
                .is_some_and(|v| v.is_object())
        {
            return Err("Resource pack metadata has no pack object".into());
        }
    }
    if !match kind {
        Kind::Packs => pack,
        Kind::Shaders => shader,
        Kind::Mods => manifest && wasm,
    } {
        return Err(format!(
            "Archive is not a valid {} package or is missing its required files",
            kind.label()
        )
        .into());
    }
    Ok(kind == Kind::Shaders && native_shader)
}
pub fn check_and_convert_classic_shader(destination: &Path) -> crate::Result<Option<bool>> {
    const SOURCE: &[u8] = b"float cloudHeightM = isnan(cloudHeight) ? 192.0 : cloudHeight; // iris returns \"nan\" if there are no clouds";
    const HORIZON: &[u8] = b"float cloudHeightM = (isnan(cloudHeight) || cloudHeight < 1.0) ? 192.0 : cloudHeight; // iris returns \"nan\" if there are no clouds; Horizon omits it as 0";
    const MAX_COMMON: u64 = 8 << 20;

    let path = destination.join("shaders/lib/common.glsl");
    for directory in [destination.join("shaders"), destination.join("shaders/lib")] {
        match fs::symlink_metadata(&directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err("Shader pack contains an unsafe common.glsl path".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Shader common.glsl is not a regular file".into());
    }
    if metadata.len() > MAX_COMMON {
        return Err("Shader common.glsl is unexpectedly large".into());
    }

    let bytes = fs::read(&path)?;
    let source_count = bytes.windows(SOURCE.len()).filter(|part| *part == SOURCE).count();
    let horizon_count = bytes.windows(HORIZON.len()).filter(|part| *part == HORIZON).count();
    if horizon_count == 1 && source_count == 0 {
        return Ok(Some(false));
    }
    if source_count != 1 || horizon_count != 0 {
        return Ok(None);
    }

    let start = bytes
        .windows(SOURCE.len())
        .position(|part| part == SOURCE)
        .ok_or("Verified shader conversion pattern disappeared")?;
    let mut converted = Vec::with_capacity(bytes.len() + HORIZON.len() - SOURCE.len());
    converted.extend_from_slice(&bytes[..start]);
    converted.extend_from_slice(HORIZON);
    converted.extend_from_slice(&bytes[start + SOURCE.len()..]);

    let temporary = destination.join(".horizon-common.glsl.tmp");
    let result = (|| -> crate::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&converted)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(Some(true))
}

pub fn extract_shader_archive(
    path: &Path,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> crate::Result<()> {
    let result = (|| -> crate::Result<()> {
        let file = File::open(path)?;
        let mut zip = zip::ZipArchive::new(file)?;
        if zip.len() > 32768 || destination.exists() {
            return Err("Shader archive or destination is invalid".into());
        }
        let mut prefix: Option<String> = None;
        for i in 0..zip.len() {
            if cancelled() {
                return Err("Import cancelled".into());
            }
            let entry = zip.by_index(i)?;
            let raw = entry.name();
            if !entry.is_dir()
                && shader_source_file(raw)
                && let Some(root) = shader_pack_prefix(raw)
            {
                let candidate = root.to_owned();
                if prefix.as_ref().is_some_and(|old| old != &candidate) {
                    return Err("Shader ZIP contains more than one pack folder".into());
                }
                prefix = Some(candidate);
            }
        }
        let prefix = prefix
            .as_deref()
            .ok_or("Shader ZIP has no shaders folder")?;
        fs::create_dir(destination)?;
        let mut total = 0u64;
        for i in 0..zip.len() {
            if cancelled() {
                return Err("Import cancelled".into());
            }
            let mut entry = zip.by_index(i)?;
            let raw = entry.name().to_owned();
            let relative = if prefix.is_empty() {
                raw.as_str()
            } else if let Some(relative) = raw.strip_prefix(prefix) {
                relative
            } else {
                continue;
            };
            let relative = relative.trim_end_matches('/');
            if relative.is_empty() {
                continue;
            }
            if relative.len() > 1024
                || relative.contains(['\\', ':', '\0'])
                || relative.starts_with('/')
                || relative
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
                || entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err("Shader archive contains an unsafe path".into());
            }
            let relative_path = Path::new(relative);
            if relative_path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err("Shader archive contains an unsafe path".into());
            }
            let target = destination.join(relative_path);
            if entry.is_dir() || relative.ends_with('/') {
                fs::create_dir_all(&target)?;
                continue;
            }
            total = total
                .checked_add(entry.size())
                .ok_or("Shader archive size overflow")?;
            if total > MAX_EXPANDED {
                return Err("Expanded shader archive exceeds 512 MiB".into());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            let expected = entry.size();
            let mut actual = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                if cancelled() {
                    return Err("Import cancelled".into());
                }
                let count = entry.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                actual += count as u64;
                if actual > expected || actual > MAX_EXPANDED {
                    return Err("Shader archive size mismatch".into());
                }
                output.write_all(&buffer[..count])?;
            }
            if actual != expected {
                return Err("Incomplete shader archive entry".into());
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}
pub fn copy_bounded(
    input: &mut impl Read,
    output: &mut impl io::Write,
    mut progress: impl FnMut(u64) -> bool,
) -> io::Result<u64> {
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        if !progress(total) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Import cancelled",
            ));
        }
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_INPUT {
            return Err(io::Error::other("Import exceeds 256 MiB"));
        }
        output.write_all(&buffer[..n])?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    #[test]
    fn validates_structure_paths_metadata_and_cancellation() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-import-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("stage.part");
        let metadata = br#"{"pack":{"pack_format":1,"description":"test"}}"#;
        std::fs::write(
            &path,
            archive(&[("pack.mcmeta", metadata), ("assets/test.png", b"png")]),
        )
        .unwrap();
        assert_eq!(
            validate(&path, Kind::Packs, "Test.zip", || false).unwrap(),
            false
        );
        assert!(validate(&path, Kind::Packs, "Test.zip", || true).is_err());
        assert!(validate(&path, Kind::Packs, "../Test.zip", || false).is_err());
        for name in ["../outside", "/absolute", "C:/outside", "folder\\outside"] {
            std::fs::write(&path, archive(&[("pack.mcmeta", metadata), (name, b"bad")])).unwrap();
            assert!(
                validate(&path, Kind::Packs, "Test.zip", || false).is_err(),
                "{name}"
            );
        }
        std::fs::write(&path, archive(&[("pack.mcmeta", b"not JSON")])).unwrap();
        assert!(validate(&path, Kind::Packs, "Test.zip", || false).is_err());
        std::fs::write(&path, archive(&[("shaders/test.fsh", b"shader")])).unwrap();
        assert!(validate(&path, Kind::Shaders, "Test.zip", || false).is_ok());
        assert!(validate(&path, Kind::Packs, "Test.zip", || false).is_err());
        std::fs::write(&path, archive(&[("horizon-shaders.toml", b"manifest")])).unwrap();
        assert!(validate(&path, Kind::Shaders, "Test.zip", || false).unwrap());
        std::fs::write(&path, archive(&[("horizon-mod.toml", b"manifest")])).unwrap();
        assert!(validate(&path, Kind::Mods, "Test.hmod", || false).is_err());
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink(
                "link",
                "../outside",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        std::fs::write(&path, writer.finish().unwrap().into_inner()).unwrap();
        assert!(validate(&path, Kind::Packs, "Test.zip", || false).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn extracts_classic_shader_zip_with_one_wrapper_directory() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-shader-extract-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let archive_path = dir.join("Example.zip");
        let bytes = archive(&[
            ("Example/shaders/final.fsh", b"void main() {}"),
            ("Example/shaders/gbuffers_terrain.vsh", b"void main() {}"),
            ("Example/shaders.properties", b"oldLighting=true"),
        ]);
        std::fs::write(&archive_path, bytes).unwrap();
        assert!(!validate(&archive_path, Kind::Shaders, "Example.zip", || false).unwrap());
        let destination = dir.join("staged-pack");
        extract_shader_archive(&archive_path, &destination, || false).unwrap();
        assert_eq!(
            std::fs::read(destination.join("shaders/final.fsh")).unwrap(),
            b"void main() {}"
        );
        assert_eq!(
            std::fs::read(destination.join("shaders.properties")).unwrap(),
            b"oldLighting=true"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn streamed_copy_is_bounded_and_cancellable() {
        let mut out = Vec::new();
        assert_eq!(
            copy_bounded(&mut Cursor::new(b"test"), &mut out, |_| true).unwrap(),
            4
        );
        assert_eq!(out, b"test");
        assert!(copy_bounded(&mut io::repeat(0), &mut io::sink(), |_| true).is_err());
        assert!(copy_bounded(&mut Cursor::new(b"test"), &mut io::sink(), |_| false).is_err());
    }
    #[test]
    fn classic_shader_zips_extract_from_root_or_nested_wrapping_folders() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-shader-import-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shader.zip");
        let target = dir.join("shader");
        std::fs::write(
            &path,
            archive(&[
                ("Download/Pack/shaders/final.fsh", b"final"),
                ("Download/Pack/shaders/final.vsh", b"vertex"),
                ("Download/Pack/lang/en_us.lang", b"name"),
                ("__MACOSX/._Pack", b"metadata"),
            ]),
        )
        .unwrap();
        assert!(!validate(&path, Kind::Shaders, "shader.zip", || false).unwrap());
        extract_shader_archive(&path, &target, || false).unwrap();
        assert_eq!(
            std::fs::read(target.join("shaders/final.fsh")).unwrap(),
            b"final"
        );
        assert_eq!(
            std::fs::read(target.join("lang/en_us.lang")).unwrap(),
            b"name"
        );
        std::fs::remove_dir_all(&target).unwrap();
        std::fs::write(
            &path,
            archive(&[("shaders/final.fsh", b"final")]),
        )
        .unwrap();
        extract_shader_archive(&path, &target, || false).unwrap();
        assert_eq!(
            std::fs::read(target.join("shaders/final.fsh")).unwrap(),
            b"final"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn shader_extraction_cleans_partial_files_when_cancelled() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-shader-cancel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shader.zip");
        let target = dir.join("shader");
        std::fs::write(&path, archive(&[("shaders/final.fsh", b"final")])).unwrap();
        assert!(extract_shader_archive(&path, &target, || true).is_err());
        assert!(!target.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn converts_only_the_verified_classic_shader_line() {
        const SOURCE: &[u8] = b"float cloudHeightM = isnan(cloudHeight) ? 192.0 : cloudHeight; // iris returns \"nan\" if there are no clouds";
        const HORIZON: &[u8] = b"float cloudHeightM = (isnan(cloudHeight) || cloudHeight < 1.0) ? 192.0 : cloudHeight; // iris returns \"nan\" if there are no clouds; Horizon omits it as 0";
        let dir = std::env::temp_dir().join(format!(
            "horizon-shader-convert-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let common = dir.join("shaders/lib/common.glsl");
        let other = dir.join("shaders/final.fsh");
        fs::create_dir_all(common.parent().unwrap()).unwrap();
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        let original = [b"before\n".as_slice(), SOURCE, b"\nafter\n"].concat();
        fs::write(&common, &original).unwrap();
        fs::write(&other, b"unchanged shader source\n").unwrap();

        assert_eq!(check_and_convert_classic_shader(&dir).unwrap(), Some(true));
        let converted = fs::read(&common).unwrap();
        assert!(converted.windows(HORIZON.len()).any(|part| part == HORIZON));
        assert!(!converted.windows(SOURCE.len()).any(|part| part == SOURCE));
        assert_eq!(&converted[..7], b"before\n");
        assert!(converted.ends_with(b"\nafter\n"));
        assert_eq!(fs::read(&other).unwrap(), b"unchanged shader source\n");
        assert_eq!(check_and_convert_classic_shader(&dir).unwrap(), Some(false));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_classic_shader_is_left_unchanged() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-shader-unsupported-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let common = dir.join("shaders/lib/common.glsl");
        fs::create_dir_all(common.parent().unwrap()).unwrap();
        fs::write(&common, b"float cloudHeightM = cloudHeight;\n").unwrap();
        let before = fs::read(&common).unwrap();
        assert_eq!(check_and_convert_classic_shader(&dir).unwrap(), None);
        assert_eq!(fs::read(&common).unwrap(), before);
        fs::remove_dir_all(dir).unwrap();
    }

}
