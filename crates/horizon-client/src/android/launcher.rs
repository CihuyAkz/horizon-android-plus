//! A native Rust installer and version screen, sharing the game's Vulkan renderer.
use crate::catalog::{Catalog, Page};
use crate::game;
#[path = "login.rs"]
mod login;
thread_local! {static SESSION:std::cell::RefCell<Option<PathBuf>>=const{std::cell::RefCell::new(None)};}
const HTML_LAUNCHER_ONLY: bool = true;
static WEB_STATUS: std::sync::OnceLock<Mutex<String>> = std::sync::OnceLock::new();
static WEB_STATUS_PUBLISHED_AT: std::sync::OnceLock<Mutex<Option<Instant>>> = std::sync::OnceLock::new();
pub(super) fn web_status() -> String {
    WEB_STATUS.get_or_init(|| Mutex::new(r#"{"connected":true}"#.into()))
        .lock().unwrap_or_else(|error| error.into_inner()).clone()
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AndroidRuntimeManifest {
    schema: u32,
    id: String,
    abi: String,
    minecraft_version: String,
    commit: String,
    sha256: String,
}

fn runtime_dir(home: &std::path::Path, id: &str) -> PathBuf {
    home.join("horizon-runtimes").join(id)
}

fn read_runtime_manifest(home: &std::path::Path, id: &str) -> Option<AndroidRuntimeManifest> {
    if !crate::catalog::valid_runtime_id(id) { return None; }
    let dir = runtime_dir(home, id);
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    if !meta.is_dir() || meta.file_type().is_symlink() { return None; }
    let lib = dir.join("libhorizon_runtime.so");
    let lib_meta = std::fs::symlink_metadata(&lib).ok()?;
    if !lib_meta.is_file() || lib_meta.file_type().is_symlink() { return None; }
    let manifest_path = dir.join("runtime.json");
    let manifest_meta = std::fs::symlink_metadata(&manifest_path).ok()?;
    if !manifest_meta.is_file() || manifest_meta.file_type().is_symlink() || manifest_meta.len() > 16 * 1024 {
        return None;
    }
    let bytes = std::fs::read(manifest_path).ok()?;
    let manifest: AndroidRuntimeManifest = serde_json::from_slice(&bytes).ok()?;
    (manifest.schema == 1
        && manifest.id == id
        && manifest.abi == crate::android_runtime_abi()
        && manifest.minecraft_version == horizon_core::VANILLA_VERSION
        && manifest.sha256.len() == 64
        && manifest.sha256.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(manifest)
}

fn installed_runtime(home: &std::path::Path, id: &str) -> bool {
    read_runtime_manifest(home, id).is_some()
}

fn runtime_versions(home: &std::path::Path, loaded: &str) -> Vec<serde_json::Value> {
    let mut versions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home.join("horizon-runtimes")) {
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else { continue; };
            let Some(manifest) = read_runtime_manifest(home, &id) else { continue; };
            let label = format!("{} · {}", id, manifest.commit);
            let current = id == loaded;
            versions.push(serde_json::json!({
                "id": id,
                "label": label,
                "commit": manifest.commit.clone(),
                "installed": true,
                "current": current,
                "abi": manifest.abi.clone(),
                "minecraftVersion": manifest.minecraft_version.clone(),
            }));
        }
    }
    let apk_default = std::fs::read_to_string(home.join("horizon-runtime-default.txt"))
        .unwrap_or_else(|_| crate::android_runtime_version());
    if !versions.iter().any(|item| item["id"].as_str() == Some(apk_default.trim())) {
        if let Some(manifest) = read_runtime_manifest(home, apk_default.trim()) {
            versions.push(serde_json::json!({
                "id": manifest.id.clone(),
                "label": format!("{} · {}", manifest.id, manifest.commit),
                "commit": manifest.commit.clone(),
                "installed": true,
                "current": manifest.id.as_str() == loaded,
                "abi": manifest.abi.clone(),
                "minecraftVersion": manifest.minecraft_version.clone(),
            }));
        }
    }
    versions.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
    versions
}

pub(crate) fn session_argument() -> Option<PathBuf> {
    SESSION.with(|s| s.borrow_mut().take())
}
#[path = "chooser.rs"]
mod chooser;
#[path = "launcher_gesture.rs"]
mod gesture;
#[path = "launcher_pages.rs"]
mod pages;
use horizon_client::{
    Result,
    gfx::Renderer,
    hud::{HudAtlas, HudBatch},
    ui_theme::{Face, Ui},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    window::{Window, WindowId},
};

pub(super) struct Launcher {
    home: PathBuf,
    controller_edges: horizon_client::gamepad::Edges,
    controller_repeat: Option<(horizon_client::gamepad::Button, Instant, bool)>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    gui: Option<game::UiResources>,
    pub(super) audio: Option<horizon_client::audio::Audio>,
    game: Option<Box<dyn ApplicationHandler>>,
    cursor: (f64, f64),
    mouse_down: bool,
    feedback: Option<(pages::Action, Instant)>,
    install: Arc<Mutex<horizon_client::official_install::InstallProgress>>,
    worker: Option<mpsc::Receiver<std::result::Result<(), String>>>,
    error: Option<String>,
    catalog: Catalog,
    targets: Vec<([f32; 4], pages::Action)>,
    list_targets: Vec<([f32; 4], pages::Action)>,
    list_view: Option<[f32; 3]>,
    pending_focus: Option<pages::Action>,
    drag: Option<gesture::Drag>,
    chooser: Option<chooser::Chooser>,
    login: login::Login,
    session_worker: Option<mpsc::Receiver<std::result::Result<Option<PathBuf>, String>>>,
    session_ready: bool,
    game_loading_requested: bool,
    focus: Option<usize>,
    drawer_open: bool,
    sidebar_scroll: f32,
    sidebar_view: Option<[f32; 3]>,
    sidebar_width: f32,
}

fn ensure_launcher_assets(home: &std::path::Path) -> Result<()> {
    let root = home.join("installer/assets/horizon/textures/gui");
    std::fs::create_dir_all(&root)?;
    for (name, data) in [
        ("launcher-logo.png", include_bytes!("../../assets/horizon-launcher-logo.png").as_slice()),
        ("launcher-home-glow.png", include_bytes!("../../assets/horizon-launcher-home-glow.png").as_slice()),
        ("launcher-play-button.png", include_bytes!("../../assets/horizon-launcher-play-button.png").as_slice()),
        ("icons/home.png", include_bytes!("../../assets/horizon-icons/home.png").as_slice()),
        ("icons/download.png", include_bytes!("../../assets/horizon-icons/download.png").as_slice()),
        ("icons/package.png", include_bytes!("../../assets/horizon-icons/package.png").as_slice()),
        ("icons/box.png", include_bytes!("../../assets/horizon-icons/box.png").as_slice()),
        ("icons/wand.png", include_bytes!("../../assets/horizon-icons/wand.png").as_slice()),
        ("icons/library.png", include_bytes!("../../assets/horizon-icons/library.png").as_slice()),
        ("icons/settings.png", include_bytes!("../../assets/horizon-icons/settings.png").as_slice()),
        ("icons/user.png", include_bytes!("../../assets/horizon-icons/user.png").as_slice()),
        ("icons/search.png", include_bytes!("../../assets/horizon-icons/search.png").as_slice()),
        ("icons/bell.png", include_bytes!("../../assets/horizon-icons/bell.png").as_slice()),
        ("icons/sun.png", include_bytes!("../../assets/horizon-icons/sun.png").as_slice()),
        ("icons/moon.png", include_bytes!("../../assets/horizon-icons/moon.png").as_slice()),
        ("icons/more.png", include_bytes!("../../assets/horizon-icons/more.png").as_slice()),
        ("icons/layers.png", include_bytes!("../../assets/horizon-icons/layers.png").as_slice()),
        ("icons/play.png", include_bytes!("../../assets/horizon-icons/play.png").as_slice()),
    ] {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read(&path).ok().as_deref() != Some(data) {
            std::fs::write(path, data)?;
        }
    }
    Ok(())
}

impl Launcher {
    fn publish_web_status(&self) {
        let now = Instant::now();
        let published_at = WEB_STATUS_PUBLISHED_AT.get_or_init(|| Mutex::new(None));
        let mut published_at = published_at.lock().unwrap_or_else(|error| error.into_inner());
        if published_at.is_some_and(|last| now.duration_since(last) < Duration::from_millis(750)) {
            return;
        }
        *published_at = Some(now);
        drop(published_at);
        let progress = *self.install.lock().unwrap_or_else(|error| error.into_inner());
        let installing = self.worker.is_some();
        let game_home = self.catalog.config.game_home(&self.home);
        let items = self.catalog.local.iter().enumerate().map(|(index, item)| {
            let kind = match item.kind {
                horizon_client::content_files::Kind::Mods => "mods",
                horizon_client::content_files::Kind::Packs => "resourcepacks",
                horizon_client::content_files::Kind::Shaders => "shaders",
            };
            serde_json::json!({
                "index": index,
                "kind": kind,
                "file": item.file,
                "title": item.title,
                "enabled": item.enabled,
            })
        }).collect::<Vec<_>>();
        let saves = game_home.join("saves");
        let mut worlds = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&saves) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = std::fs::symlink_metadata(&path) else { continue; };
                if !meta.is_dir() || meta.file_type().is_symlink() || !path.join("level.dat").is_file() {
                    continue;
                }
                let modified = meta.modified().ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs()).unwrap_or(0);
                worlds.push(serde_json::json!({
                    "name": entry.file_name().to_string_lossy(),
                    "modified": modified,
                }));
            }
        }
        worlds.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let signed_in = self.login.signed_in;
        let apk_runtime = std::fs::read_to_string(self.home.join("horizon-runtime-default.txt"))
            .unwrap_or_else(|_| crate::android_runtime_version()).trim().to_owned();
        let loaded_runtime = std::fs::read_to_string(self.home.join("horizon-runtime-selection.txt"))
            .unwrap_or_else(|_| apk_runtime.clone()).trim().to_owned();
        let content_operation = crate::documents::take_web_result().map(|(id, kind, message)| {
            serde_json::json!({
                "id": id,
                "kind": match kind {
                    horizon_client::content_files::Kind::Mods => "mods",
                    horizon_client::content_files::Kind::Packs => "resourcepacks",
                    horizon_client::content_files::Kind::Shaders => "shaders",
                },
                "message": message,
            })
        });
        let content_conflict = crate::documents::web_conflict().map(|(kind, filename, message)| {
            serde_json::json!({
                "kind": match kind {
                    horizon_client::content_files::Kind::Mods => "mods",
                    horizon_client::content_files::Kind::Packs => "resourcepacks",
                    horizon_client::content_files::Kind::Shaders => "shaders",
                },
                "filename": filename,
                "message": message,
                "canKeepBoth": kind != horizon_client::content_files::Kind::Mods,
            })
        });
        let content_progress = crate::documents::progress_snapshot().map(|progress| {
            serde_json::json!({
                "kind": match progress.kind {
                    horizon_client::content_files::Kind::Mods => "mods",
                    horizon_client::content_files::Kind::Packs => "resourcepacks",
                    horizon_client::content_files::Kind::Shaders => "shaders",
                },
                "fileName": progress.file_name,
                "stage": progress.stage,
                "percent": progress.percent,
                "active": progress.active,
                "convert": progress.convert,
                "waitingForChoice": progress.waiting_for_choice,
            })
        });
        let value = serde_json::json!({
            "connected": true,
            "version": horizon_core::VANILLA_VERSION,
            "runtimeVersion": loaded_runtime,
            "apkRuntimeVersion": apk_runtime,
            "runtimeAbi": crate::android_runtime_abi(),
            "runtimeVersions": runtime_versions(&self.home, &loaded_runtime),
            "activeInstanceId": self.catalog.config.active_instance().id,
            "instances": self.catalog.config.instances.iter().map(|instance| serde_json::json!({
                "id": instance.id,
                "name": instance.name,
                "version": instance.version,
                "resolvedVersion": instance.version.as_deref().unwrap_or("follow-current"),
            })).collect::<Vec<_>>(),
            "installed": horizon_client::official_install::ready(&self.home),
            "installing": installing,
            "busy": installing || self.session_worker.is_some() || crate::documents::busy(),
            "install": {
                "stage": progress.stage.label(),
                "done": progress.done,
                "total": progress.total,
            },
            "status": self.catalog.status,
            "error": self.error,
            "account": {
                "profile": self.login.profile,
                "signedIn": signed_in,
                "pending": self.login.pending(),
                "offlineName": self.catalog.config.name,
                "status": self.login.status(&self.catalog.config.name),
                "message": self.login.message(),
            },
            "settings": {
                "renderer": match self.catalog.config.renderer {
                    crate::catalog::RendererMode::Vulkan => "vulkan",
                    crate::catalog::RendererMode::OpenGl => "open-gl",
                    crate::catalog::RendererMode::VulkanCompatibility => "vulkan-compatibility",
                },
                "renderScalePercent": self.catalog.config.render_scale.percent(),
                "orientation": match self.catalog.config.orientation {
                    crate::catalog::Orientation::Landscape => "landscape",
                    crate::catalog::Orientation::Portrait => "portrait",
                    crate::catalog::Orientation::ReverseLandscape => "reverse-landscape",
                    crate::catalog::Orientation::ReversePortrait => "reverse-portrait",
                    crate::catalog::Orientation::Auto => "auto",
                },
                "immersive": self.catalog.config.screen.immersive,
                "avoidCutout": self.catalog.config.screen.avoid_cutout,
                "aspect": match self.catalog.config.screen.aspect {
                    crate::display::Aspect::Native => "native",
                    crate::display::Aspect::Wide => "wide",
                    crate::display::Aspect::Classic => "classic",
                    crate::display::Aspect::Square => "square",
                },
                "displayHz": self.catalog.config.device.display_hz,
                "performanceHints": self.catalog.config.device.performance_hints,
                "gpuProfile": self.catalog.config.device.gpu_profile,
                "recordPerformance": self.catalog.config.record_performance,
            },
            "items": items,
            "worlds": worlds,
            "operation": content_operation,
            "conflict": content_conflict,
            "contentProgress": content_progress,
        });
        *WEB_STATUS.get_or_init(|| Mutex::new(String::new()))
            .lock().unwrap_or_else(|error| error.into_inner()) = value.to_string();
    }

    fn apply_web_setting(&mut self, payload: &str) -> Result<()> {
        let request: serde_json::Value = serde_json::from_str(payload)
            .map_err(|error| error.to_string())?;
        let key = request.get("key").and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Missing setting key".to_string())?;
        let value = request.get("value")
            .ok_or_else(|| "Missing setting value".to_string())?;
        let mut orientation_changed = false;
        let mut screen_changed = false;
        let mut scale_changed = false;
        let mut device_changed = false;
        match key {
            "renderer" => {
                let mode = match value.as_str() {
                    Some("vulkan") => crate::catalog::RendererMode::Vulkan,
                    Some("open-gl") => crate::catalog::RendererMode::OpenGl,
                    Some("vulkan-compatibility") => crate::catalog::RendererMode::VulkanCompatibility,
                    _ => return Err("Unsupported rendering mode".into()),
                };
                self.catalog.config.renderer = mode;
                self.catalog.status = "Rendering mode saved. It will be used on the next game launch.".into();
            }
            "renderScalePercent" => {
                let percent = value.as_u64().and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| "Invalid client resolution scale".to_string())?;
                if !(25..=100).contains(&percent) {
                    return Err("Client resolution scale must be between 25 and 100 percent".into());
                }
                self.catalog.config.render_scale = crate::display::RenderScale::new(percent);
                scale_changed = true;
            }
            "orientation" => {
                self.catalog.config.orientation = match value.as_str() {
                    Some("landscape") => crate::catalog::Orientation::Landscape,
                    Some("portrait") => crate::catalog::Orientation::Portrait,
                    Some("reverse-landscape") => crate::catalog::Orientation::ReverseLandscape,
                    Some("reverse-portrait") => crate::catalog::Orientation::ReversePortrait,
                    Some("auto") => crate::catalog::Orientation::Auto,
                    _ => return Err("Unsupported screen orientation".into()),
                };
                orientation_changed = true;
            }
            "immersive" => {
                self.catalog.config.screen.immersive = value.as_bool()
                    .ok_or_else(|| "Invalid immersive fullscreen value".to_string())?;
                screen_changed = true;
            }
            "avoidCutout" => {
                self.catalog.config.screen.avoid_cutout = value.as_bool()
                    .ok_or_else(|| "Invalid camera cutout value".to_string())?;
                screen_changed = true;
            }
            "aspect" => {
                self.catalog.config.screen.aspect = match value.as_str() {
                    Some("native") => crate::display::Aspect::Native,
                    Some("wide") => crate::display::Aspect::Wide,
                    Some("classic") => crate::display::Aspect::Classic,
                    Some("square") => crate::display::Aspect::Square,
                    _ => return Err("Unsupported aspect ratio".into()),
                };
                screen_changed = true;
            }
            "displayHz" => {
                let hz = value.as_u64().and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| "Invalid display refresh rate".to_string())?;
                if !crate::device_preferences::DISPLAY_RATES.contains(&hz) {
                    return Err("Unsupported display refresh rate".into());
                }
                self.catalog.config.device.display_hz = hz;
                device_changed = true;
            }
            "performanceHints" => {
                self.catalog.config.device.performance_hints = value.as_bool()
                    .ok_or_else(|| "Invalid performance hint value".to_string())?;
                device_changed = true;
            }
            "gpuProfile" => {
                self.catalog.config.device.gpu_profile = value.as_bool()
                    .ok_or_else(|| "Invalid GPU profiling value".to_string())?;
                device_changed = true;
            }
            "recordPerformance" => {
                self.catalog.config.record_performance = value.as_bool()
                    .ok_or_else(|| "Invalid performance recording value".to_string())?;
                if !self.catalog.config.record_performance {
                    crate::performance::stop();
                }
            }
            _ => return Err("Unsupported Android setting".into()),
        }
        self.catalog.config.save(&self.home)?;
        if orientation_changed {
            crate::platform::set_orientation(self.catalog.config.orientation);
        }
        if screen_changed {
            self.configure_screen();
        }
        if scale_changed {
            crate::display::set_scale(self.catalog.config.render_scale);
            if let (Some(window), Some(renderer)) = (&self.window, &mut self.renderer) {
                crate::display::apply(window, renderer)?;
                window.request_redraw();
            }
        }
        if device_changed {
            crate::device_runtime::configure(self.catalog.config.device);
        }
        self.publish_web_status();
        Ok(())
    }

    fn handle_web_commands(&mut self, events: &ActiveEventLoop) {
        for (action, payload) in crate::take_web_commands() {
            let result: Result<()> = (|| {
                match action {
                1 => {
                    self.game_loading_requested = payload == "loading";
                    let result = self.action(events);
                    if result.is_err() && self.game_loading_requested {
                        self.game_loading_requested = false;
                        crate::platform::cancel_web_game_loading();
                    }
                    result
                },
                2 => {
                    self.login.start(&self.catalog.config.microsoft_client_id);
                    self.login.hide();
                    Ok(())
                }
                3 => {
                    self.login.sign_out();
                    self.login.hide();
                    Ok(())
                }
                4 => {
                    let (kind_name, shader_mode) = match payload.strip_prefix("shaders:") {
                        Some("convert") => ("shaders", crate::documents::ShaderImportMode::Convert),
                        Some("import") => ("shaders", crate::documents::ShaderImportMode::ImportOnly),
                        Some(_) => return Err("Unsupported shader import mode".into()),
                        None => (payload.as_str(), crate::documents::ShaderImportMode::Convert),
                    };
                    let kind = match kind_name {
                        "mods" => Some(horizon_client::content_files::Kind::Mods),
                        "resourcepacks" => Some(horizon_client::content_files::Kind::Packs),
                        "shaders" => Some(horizon_client::content_files::Kind::Shaders),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        crate::documents::begin_with_shader_mode(
                            kind,
                            self.catalog.config.game_home(&self.home),
                            shader_mode,
                        ).map_err(Into::into)
                    } else {
                        Err("Unsupported content type".into())
                    }
                }
                5 => {
                    if let Ok(index) = payload.parse::<usize>() {
                        self.catalog.toggle_local(&self.home, index);
                        Ok(())
                    } else { Err("Invalid content index".into()) }
                }
                6 => {
                    if let Ok(index) = payload.parse::<usize>() {
                        self.catalog.pending_delete = self.catalog.local.get(index).cloned();
                        self.catalog.remove_local(&self.home);
                        Ok(())
                    } else { Err("Invalid content index".into()) }
                }
                7 => { self.catalog.refresh(&self.home); Ok(()) }
                8 => {
                    let name = payload.trim();
                    if !(3..=16).contains(&name.len())
                        || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                    {
                        Err("Offline player name must be 3–16 letters, numbers, or underscores".into())
                    } else {
                        self.catalog.config.name = name.to_owned();
                        self.catalog.config.save(&self.home).map_err(Into::into)
                    }
                }
                9 => crate::documents::resolve_web_conflict(&payload).map_err(Into::into),
                10 => self.apply_web_setting(&payload),
                11 => crate::performance::export(&self.home).map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::Other, error).into()
                }),
                12 => {
                    let request: serde_json::Value = serde_json::from_str(&payload)
                        .map_err(|error| error.to_string())?;
                    let name = request.get("name").and_then(serde_json::Value::as_str)
                        .ok_or_else(|| "Missing instance name".to_string())?;
                    let version = request.get("version").and_then(serde_json::Value::as_str)
                        .map(str::trim).filter(|value| !value.is_empty());
                    if let Some(version) = version {
                        if !installed_runtime(&self.home, version) {
                            return Err("That Horizon build is not installed or is incompatible with this device".into());
                        }
                    }
                    let id = self.catalog.config.create_instance(name)?;
                    if let Some(version) = version {
                        self.catalog.config.set_instance_version_for(&id, Some(version))?;
                    }
                    let game_home = self.catalog.config.game_home(&self.home);
                    std::fs::create_dir_all(&game_home)?;
                    crate::backend::set_game_home(game_home.clone());
                    self.catalog.config.save(&self.home)?;
                    self.catalog.refresh(&self.home);
                    self.error = None;
                    self.catalog.status = if let Some(version) = version {
                        format!("Instance created with Horizon build {version}")
                    } else {
                        "Instance created and set to follow the latest compatible build".into()
                    };
                    Ok(())
                }
                13 => {
                    self.catalog.config.select_instance(payload.trim())?;
                    let game_home = self.catalog.config.game_home(&self.home);
                    std::fs::create_dir_all(&game_home)?;
                    crate::backend::set_game_home(game_home.clone());
                    self.catalog.config.save(&self.home)?;
                    self.catalog.refresh(&self.home);
                    self.error = None;
                    self.catalog.status = "Active Instance changed".into();
                    Ok(())
                }
                14 => {
                    let version = payload.trim();
                    if version.is_empty() {
                        self.catalog.config.set_instance_version(None)?;
                        self.catalog.config.save(&self.home)?;
                        self.error = None;
                        self.catalog.status = "Active Instance follows the latest installed Horizon build".into();
                        Ok(())
                    } else if installed_runtime(&self.home, version) {
                        self.catalog.config.set_instance_version(Some(version))?;
                        self.catalog.config.save(&self.home)?;
                        self.error = None;
                        self.catalog.status = format!("Active Instance build preference saved: {version}");
                        Ok(())
                    } else {
                        Err("That Android Horizon build is not installed or is incompatible with this device".into())
                    }
                }
                15 => {
                    let id = payload.trim().to_owned();
                    let profile_home = self.catalog.config.profile_home(&self.home);
                    self.catalog.config.remove_instance(&id)?;
                    if id != "default" {
                        let target = profile_home.join("instances").join(&id);
                        if target.exists() {
                            let metadata = std::fs::symlink_metadata(&target)?;
                            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                                return Err("Instance folder is not a regular directory".into());
                            }
                            std::fs::remove_dir_all(target)?;
                        }
                    }
                    self.catalog.config.save(&self.home)?;
                    let game_home = self.catalog.config.game_home(&self.home);
                    std::fs::create_dir_all(&game_home)?;
                    crate::backend::set_game_home(game_home);
                    self.catalog.refresh(&self.home);
                    Ok(())
                }
                16 => {
                    let id = payload.trim();
                    if installed_runtime(&self.home, id) {
                        self.catalog.status = format!("Installed Horizon build {id}");
                        self.error = None;
                        Ok(())
                    } else {
                        Err("The imported Horizon runtime failed validation".into())
                    }
                }
                18 => {
                    let request: serde_json::Value = serde_json::from_str(&payload)
                        .map_err(|error| error.to_string())?;
                    let id = request.get("id").and_then(serde_json::Value::as_str)
                        .ok_or_else(|| "Missing instance ID".to_string())?;
                    let version = request.get("version").and_then(serde_json::Value::as_str)
                        .filter(|value| !value.trim().is_empty());
                    if let Some(version) = version {
                        if !installed_runtime(&self.home, version) {
                            return Err("That Horizon build is not installed or is incompatible with this device".into());
                        }
                    }
                    self.catalog.config.set_instance_version_for(id, version)?;
                    self.catalog.config.save(&self.home)?;
                    self.error = None;
                    self.catalog.status = if let Some(version) = version {
                        format!("Horizon build preference saved for {id}: {version}")
                    } else {
                        format!("Instance {id} follows the latest installed Horizon build")
                    };
                    Ok(())
                }
                    _ => Ok(()),
                }
            })();
            if let Err(error) = result { self.fail(error); }
        }
        self.publish_web_status();
    }

    pub(super) fn new(home: PathBuf) -> Self {
        let light_theme = horizon_client::ui_theme::preference(&home.join("installer"), "theme").as_deref() == Some("light");
        horizon_client::ui_theme::color::set_light(light_theme);
        // Upgrade existing installations that retained the old download cache.
        let cleanup_home = home.clone();
        std::thread::spawn(move || {
            if let Err(error) =
                horizon_client::official_install::cleanup_completed_downloads(&cleanup_home)
            {
                log::warn!("Could not clean completed Minecraft downloads: {error}");
            }
        });
        let catalog = Catalog::new(&home);
        crate::backend::set_game_home(catalog.config.game_home(&home));
        crate::configure_input_mode(catalog.config.input_host);
        crate::device_runtime::configure(catalog.config.device);
        Self {
            home,
            controller_edges: Default::default(),
            controller_repeat: None,
            window: None,
            renderer: None,
            gui: None,
            audio: None,
            game: None,
            cursor: (0.0, 0.0),
            mouse_down: false,
            feedback: None,
            install: Arc::default(),
            worker: None,
            error: None,
            catalog,
            targets: vec![],
            list_targets: vec![],
            list_view: None,
            pending_focus: None,
            drag: None,
            chooser: None,
            login: login::Login::new(),
            session_worker: None,
            session_ready: false,
            game_loading_requested: false,
            focus: None,
            drawer_open: false,
            sidebar_scroll: 0.0,
            sidebar_view: None,
            sidebar_width: 0.0,
        }
    }

    fn restore_launcher(&mut self, events: &ActiveEventLoop) {
        if !horizon_client::android_runtime::take_launcher_return() {
            return;
        }
        self.game_loading_requested = false;
        if let Some(mut game) = self.game.take() {
            game.exiting(events);
            drop(game);
        }
        crate::performance::stop();
        crate::platform::hide();
        horizon_client::auth::clear_current();
        self.session_ready = false;
        horizon_client::discord_presence::stop();
        horizon_client::ui_theme::reset_atlas();
        self.cursor = (0.0, 0.0);
        self.drag = None;
        self.chooser = None;
        self.focus = None;
        self.pending_focus = None;
        if let Err(error) = std::env::set_current_dir(&self.home) {
            self.fail(error);
            return;
        }
        horizon_client::android_runtime::clear_gamepads();
        self.controller_edges = Default::default();
        self.controller_repeat = None;
        self.catalog.config.input_host = horizon_client::android_runtime::input_mode();
        self.catalog.refresh(&self.home);
        self.login.refresh_status();
        self.catalog.page = Page::Play;
        if let Ok(message) = std::fs::read_to_string(self.home.join("android-error.txt")) {
            self.error = Some(message);
        }
        if let Err(error) = self.open(events) {
            self.fail(error);
            events.exit();
        } else {
            crate::platform::set_web_launcher_visible(true);
            self.publish_web_status();
        }
    }
    pub(super) fn configure_screen(&self) {
        crate::native_ime::configure_screen(
            self.catalog.config.screen.immersive,
            self.catalog.config.screen.avoid_cutout,
        );
        winit::platform::android::set_native_content_aspect(
            self.catalog.config.screen.aspect.ratio(),
        );
    }
    fn fail(&mut self, error: impl std::fmt::Display) {
        log::error!("{error}");
        self.error = Some(error.to_string());
        let _ = std::fs::write(self.home.join("android-error.txt"), error.to_string());
    }

    fn open(&mut self, events: &ActiveEventLoop) -> Result<()> {
        crate::platform::set_launcher_orientation();
        self.configure_screen();
        crate::display::set_scale(self.catalog.config.render_scale);
        horizon_client::android_runtime::configure_gpu(false);
        if let Some(renderer) = &mut self.renderer {
            if let Some(size) = crate::display::prepare(self.window.as_ref().unwrap())? {
                renderer.resize(size[0], size[1]);
            }
            renderer.recreate_surface()?;
            let size = self.window.as_ref().unwrap().inner_size();
            renderer.resize(size.width, size.height);
            return Ok(());
        }
        let window = Arc::new(
            events.create_window(Window::default_attributes().with_title("Horizon Android"))?,
        );
        crate::display::prepare(&window)?;
        if HTML_LAUNCHER_ONLY {
            self.window = Some(window);
            self.renderer = None;
            self.gui = None;
            return Ok(());
        }
        ensure_launcher_assets(&self.home)?;
        let gui = game::UiResources {
            atlas: HudAtlas::build(&self.home.join("installer"))?,
            panorama: Arc::new(horizon_client::gfx::panorama::Faces {
                size: 1,
                pixels: [0u8; 4].repeat(6),
            }),
        };
        self.renderer = Some(game::loading_renderer(
            window.clone(),
            false,
            Some(&gui),
            None,
            horizon_client::graphics::GraphicsApi::Auto,
        )?);
        self.window = Some(window);
        self.gui = Some(gui);
        Ok(())
    }

    fn action(&mut self, events: &ActiveEventLoop) -> Result<()> {
        if self.worker.is_some() {
            if self.game_loading_requested {
                self.game_loading_requested = false;
                crate::platform::cancel_web_game_loading();
            }
            return Ok(());
        }
        if self.session_worker.is_some() {
            return Ok(());
        }
        if self.login.pending() {
            if self.game_loading_requested {
                self.game_loading_requested = false;
                crate::platform::cancel_web_game_loading();
            }
            return Ok(());
        }
        if crate::documents::shader_conflict() {
            return Err(
                "Resolve the pending content file conflict in Horizon before launching the game"
                    .into(),
            );
        }
        crate::configure_input_mode(self.catalog.config.input_host);
        crate::device_runtime::configure(self.catalog.config.device);
        if horizon_client::official_install::ready(&self.home) {
            let current_runtime = crate::android_runtime_version();
            if let Some(pinned) = self.catalog.config.active_instance().version.as_deref()
                && pinned != current_runtime
            {
                return Err(format!("Instance requires {pinned}, but this APK contains {current_runtime}. Install an Android build for that version first.").into());
            }
            horizon_client::android_resources::repair_structure_transforms(
                &self.home.join("engine-data.zip"),
                &self
                    .home
                    .join(".local/horizon-client")
                    .join(horizon_core::VANILLA_VERSION),
            )?;
            crate::platform::hide();
            self.catalog.config.save(&self.home)?;
            horizon_client::android_runtime::configure_graphics(
                self.catalog.config.renderer == crate::catalog::RendererMode::OpenGl,
            );
            horizon_client::android_runtime::configure_gpu(
                self.catalog.config.renderer == crate::catalog::RendererMode::Vulkan,
            );
            // Migrate the earlier launcher preset once; subsequent edits belong to Key Binds.
            let keypad_path = self
                .catalog
                .config
                .game_home(&self.home)
                .join("horizon-keypad.json");
            if self.catalog.config.keypad_controls && !keypad_path.exists() {
                if let Some(dir) = keypad_path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                horizon_client::keypad::Settings {
                    enabled: true,
                    ..Default::default()
                }
                .save(&keypad_path)?;
            }
            self.catalog.config.keypad_controls = false;
            let game_home = self.catalog.config.game_home(&self.home);
            if !self.session_ready && crate::account::Accounts::load().selected().is_some() {
                let id = self.catalog.config.microsoft_client_id.clone();
                let (sender, receiver) = mpsc::channel();
                self.session_worker = Some(receiver);
                self.catalog.status = "Preparing Microsoft session...".into();
                std::thread::spawn(move || {
                    let result = (|| {
                        login::Login::session(&id)?
                            .map(|(a, s)| {
                                crate::account::write_session_file(
                                    &game_home.join("sessions"),
                                    &a,
                                    &s,
                                )
                            })
                            .transpose()
                    })();
                    let _ = sender.send(result);
                });
                return Ok(());
            }
            self.session_ready = false;
            std::fs::create_dir_all(&game_home)?;
            crate::backend::set_game_home(game_home.clone());
            std::env::set_current_dir(game_home)?;
            self.audio = None;
            self.error = None;
            let _ = std::fs::remove_file(self.home.join("android-error.txt"));
            if self.catalog.config.record_performance || self.catalog.config.device.gpu_profile {
                crate::performance::start(
                    &self.home,
                    serde_json::json!({
                        "renderer": self.catalog.config.renderer.label(),
                        "resolution_percent": self.catalog.config.render_scale.percent(),
                        "source_commit": option_env!("HORIZON_ANDROID_BUILD_COMMIT"),
                        "source_dirty": option_env!("HORIZON_ANDROID_BUILD_DIRTY"),
                        "performance_recording": self.catalog.config.record_performance,
                        "gpu_profiling": self.catalog.config.device.gpu_profile,
                    }),
                )?;
            }
            // The A press that launches the game must not accept its first menu too.
            horizon_client::android_runtime::clear_gamepads();
            self.controller_edges = Default::default();
            self.controller_repeat = None;
            let game = match game::android_game() {
                Ok(game) => game,
                Err(error) => {
                    crate::performance::stop();
                    return Err(error);
                }
            };
            // Hide the HTML layer only after the real game was successfully constructed.
            crate::platform::set_web_launcher_visible(false);
            // Android has one native window; release its surface before handing it over.
            self.renderer = None;
            self.window = None;
            self.gui = None;
            horizon_client::ui_theme::reset_atlas();
            self.game = Some(game);
            self.game.as_mut().unwrap().resumed(events);
        } else {
            if !self.home.join("engine-data.zip").is_file() {
                return Err(
                    "This APK is missing its engine data. Install a complete build.".into(),
                );
            }
            self.error = None;
            *self.install.lock().unwrap() = Default::default();
            let home = self.home.clone();
            let install = self.install.clone();
            let (sender, receiver) = mpsc::channel();
            self.worker = Some(receiver);
            std::thread::spawn(move || {
                let result =
                    horizon_client::official_install::install_with_progress(&home, |update| {
                        let mut current = install.lock().unwrap();
                        let done = if current.stage == update.stage {
                            current.done.max(update.done)
                        } else {
                            update.done
                        };
                        *current =
                            horizon_client::official_install::InstallProgress { done, ..update };
                    });
                let _ = sender.send(result.map_err(|e| e.to_string()));
            });
        }
        Ok(())
    }
}

impl ApplicationHandler for Launcher {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        self.configure_screen();
        if let Some(game) = &mut self.game {
            return game.resumed(events);
        }
        if let Err(error) = self.open(events) {
            self.fail(error);
            events.exit();
        }
    }
    fn suspended(&mut self, events: &ActiveEventLoop) {
        if let Some(game) = &mut self.game {
            return game.suspended(events);
        }
        self.drag = None;
        if let Some(renderer) = &mut self.renderer {
            renderer.suspend_surface();
        }
    }
    fn window_event(&mut self, events: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if !(self.game.is_some() && crate::documents::shader_operation_pending())
            && crate::documents::handle_event(&event)
        {
            return;
        }
        if matches!(event, WindowEvent::Resized(_)) {
            let _ = crate::native_ime::viewport_bottom();
        }
        if matches!(event, WindowEvent::Focused(true)) {
            self.configure_screen();
        }
        if let Some(game) = &mut self.game {
            let first_game_frame = self.game_loading_requested
                && matches!(&event, WindowEvent::RedrawRequested);
            game.window_event(events, id, event);
            if first_game_frame {
                self.game_loading_requested = false;
                crate::platform::finish_web_game_loading();
            }
            self.restore_launcher(events);
            return;
        }
        if HTML_LAUNCHER_ONLY {
            match event {
                WindowEvent::CloseRequested => events.exit(),
                WindowEvent::Resized(size) => {
                    if let Some(renderer) = &mut self.renderer {
                        renderer.resize(size.width, size.height);
                    }
                }
                WindowEvent::Focused(false) => {
                    horizon_client::android_runtime::clear_gamepads();
                    self.controller_repeat = None;
                }
                _ => {}
            }
            return;
        }
        let result = match event {
            WindowEvent::CloseRequested => {
                if self.login.visible || self.login.pending() {
                    self.login.hide();
                    return;
                }
                events.exit();
                Ok(())
            }
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
                Ok(())
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.focus = None;
                self.pending_focus = None;
                self.cursor = (position.x, position.y);
                Ok(())
            }
            WindowEvent::Touch(touch) => self.touch_event(events, touch),
            WindowEvent::Focused(false) => {
                horizon_client::android_runtime::clear_gamepads();
                self.controller_repeat = None;
                self.mouse_down = false;
                self.drag = None;
                Ok(())
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if state == ElementState::Pressed {
                    self.mouse_down = true;
                    Ok(())
                } else if std::mem::take(&mut self.mouse_down) {
                    self.activate(events)
                } else {
                    Ok(())
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let y = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 42.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 / self.ui_density(),
                };
                let density = self.ui_density();
                let cursor = (self.cursor.0 as f32 / density, self.cursor.1 as f32 / density);
                if let Some([top, bottom, maximum]) = self.sidebar_view
                    && cursor.0 < self.sidebar_width
                    && cursor.1 >= top
                    && cursor.1 <= bottom
                {
                    self.sidebar_scroll = (self.sidebar_scroll - y).clamp(0.0, maximum);
                } else if let Some(chooser) = &mut self.chooser {
                    chooser.scroll = (chooser.scroll - y).max(0.0);
                } else {
                    self.catalog.scroll = (self.catalog.scroll - y).max(0.0);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                Ok(())
            }
            WindowEvent::RedrawRequested => self.draw_pages(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if self.navigate(&event) {
                    Ok(())
                } else if self.catalog.editing.is_none()
                    && !event.repeat
                    && matches!(
                        event.logical_key,
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter)
                    )
                {
                    self.activate(events)
                } else {
                    self.type_key(event);
                    Ok(())
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                self.catalog
                    .edit(horizon_client::osk::Output::Text(text), &self.home);
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.fail(error);
        }
    }
    fn device_event(&mut self, events: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        if let Some(game) = &mut self.game {
            game.device_event(events, id, event);
        } else {
            horizon_client::android_runtime::gamepad_event(
                winit::platform::android::native_device_id(id),
                &event,
            );
        }
    }
    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        if let Some(game) = &mut self.game {
            game.about_to_wait(events);
            self.restore_launcher(events);
            if self.game.is_none() { self.publish_web_status(); }
            return;
        }
        if !HTML_LAUNCHER_ONLY {
            crate::platform::poll_gamepad_connections();
            if let Err(error) = self.controller_frame(events) {
                self.fail(error);
            }
        }
        if let (Some(window), Some(renderer)) = (&self.window, &mut self.renderer)
            && let Err(error) = crate::display::apply(window, renderer)
        {
            crate::display::set_scale(crate::display::RenderScale::default());
            self.fail(error);
        }
        if let Some(result) = self.worker.as_ref().and_then(|w| w.try_recv().ok()) {
            self.worker = None;
            if let Err(error) = result {
                self.fail(error);
            }
            if self.game_loading_requested {
                self.game_loading_requested = false;
                crate::platform::cancel_web_game_loading();
            }
        }
        for out in crate::platform::poll() {
            if matches!(out, horizon_client::osk::Output::Enter) {
                crate::platform::hide();
            }
            self.catalog.edit(out, &self.home);
        }
        if let Some(audio) = &mut self.audio {
            audio.tick(false, &Default::default());
        }
        self.catalog.poll(&self.home);
        if let Some(notice) = crate::documents::poll() {
            self.catalog.status = notice.text;
            if notice.changed {
                self.catalog.refresh(&self.home);
                self.catalog.show_installed(notice.kind);
            }
        }
        for result in crate::gallery::poll() {
            self.catalog.status = result.unwrap_or_else(|e| format!("File export failed: {e}"));
        }
        self.login.poll();
        if let Some(result) = self.session_worker.as_ref().and_then(|w| w.try_recv().ok()) {
            self.session_worker = None;
            match result {
                Ok(file) => {
                    SESSION.with(|s| *s.borrow_mut() = file);
                    self.session_ready = true;
                    if let Err(e) = self.action(events) {
                        self.fail(e);
                        if self.game_loading_requested {
                            self.game_loading_requested = false;
                            crate::platform::cancel_web_game_loading();
                        }
                    }
                }
                Err(e) => {
                    self.fail(e);
                    if self.game_loading_requested {
                        self.game_loading_requested = false;
                        crate::platform::cancel_web_game_loading();
                    }
                }
            }
        }

        self.handle_web_commands(events);
        if !HTML_LAUNCHER_ONLY {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        events.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(100),
        ));
    }
    fn exiting(&mut self, events: &ActiveEventLoop) {
        if let Some(game) = &mut self.game {
            game.exiting(events);
        }
    }
}

impl Launcher {
    fn controller_frame(&mut self, events: &ActiveEventLoop) -> Result<()> {
        use horizon_client::gamepad::Button;
        use winit::keyboard::{Key, NamedKey};
        let mut state = horizon_client::android_runtime::gamepad_state();
        for (b, held) in [
            (Button::DPadLeft, state.left[0] < -0.55),
            (Button::DPadRight, state.left[0] > 0.55),
            (Button::DPadUp, state.left[1] > 0.55),
            (Button::DPadDown, state.left[1] < -0.55),
        ] {
            state.set(b, held || state.held(b));
        }
        let now = Instant::now();
        let mut pressed: Vec<_> = self
            .controller_edges
            .update(&state, 0.5)
            .into_iter()
            .filter_map(|(b, down)| down.then_some(b))
            .collect();
        if let Some((button, last, repeating)) = self.controller_repeat {
            if !state.held(button) {
                self.controller_repeat = None;
            } else if now.duration_since(last).as_secs_f32() >= if repeating { 0.09 } else { 0.35 }
            {
                pressed.push(button);
                self.controller_repeat = Some((button, now, true));
            }
        }
        for b in pressed {
            let direction = match b {
                Button::DPadUp => Some(NamedKey::ArrowUp),
                Button::DPadDown => Some(NamedKey::ArrowDown),
                Button::DPadLeft => Some(NamedKey::ArrowLeft),
                Button::DPadRight => Some(NamedKey::ArrowRight),
                _ => None,
            };
            if let Some(key) = direction {
                self.navigate_key(&Key::Named(key));
                if self.controller_repeat.is_none_or(|(old, _, _)| old != b) {
                    self.controller_repeat = Some((b, now, false));
                }
            } else if b == Button::South {
                self.activate(events)?;
            } else if b == Button::East {
                crate::platform::hide();
                self.catalog.editing = None;
                if self.chooser.take().is_none() {
                    if self.login.visible {
                        self.login.visible = false;
                    } else {
                        self.catalog.page = Page::Play;
                    }
                }
                self.focus = None;
            }
        }
        Ok(())
    }
}
