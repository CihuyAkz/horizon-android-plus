//! A native Rust installer and version screen, sharing the game's Vulkan renderer.
use crate::catalog::{Catalog, Page};
use crate::game;
#[path = "chooser.rs"]
mod chooser;
#[path = "launcher_gesture.rs"]
mod gesture;
#[path = "login.rs"]
mod login;
#[path = "launcher_pages.rs"]
mod pages;
#[path = "wardrobe.rs"]
mod wardrobe;
use horizon_client::{
    Result,
    gfx::Renderer,
    hud::{HudAtlas, HudBatch},
    ui_theme::{Face, Ui},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    window::{Window, WindowId},
};

struct PreparedAccount {
    key: String,
    file: PathBuf,
    appearance: crate::skins::cloud::Cache,
}

static SESSION_ARGUMENT: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static WEB_STATUS: OnceLock<Mutex<String>> = OnceLock::new();

pub(crate) fn session_argument() -> Option<PathBuf> {
    SESSION_ARGUMENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn set_session_argument(path: Option<PathBuf>) {
    *SESSION_ARGUMENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = path;
}

pub(crate) fn web_status() -> String {
    WEB_STATUS
        .get_or_init(|| Mutex::new(r#"{"connected":false,"status":"Waiting for Android launcher runtime"}"#.into()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn set_web_status(status: String) {
    *WEB_STATUS
        .get_or_init(|| Mutex::new("Horizon Android is ready".into()))
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = status;
}
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
    exit_confirmation: bool,
    login: login::Login,
    wardrobe: wardrobe::Wardrobe,
    session_worker: Option<mpsc::Receiver<std::result::Result<PreparedAccount, String>>>,
    session_ready: bool,
    prepared_session: Option<PathBuf>,
    focus: Option<usize>,
    diagnostic_export: Option<mpsc::Receiver<std::result::Result<String, String>>>,
    web_status_at: Option<Instant>,
}

impl Launcher {
    pub(super) fn refresh_wardrobe_account(&mut self) {
        if self.catalog.page == Page::Skins {
            self.catalog.scroll = 0.0;
        }
        // Keep the profile-scoped wardrobe state current even when the HTML launcher
        // is the only visible UI and the native Rust pages are never opened.
        self.wardrobe.open(
            &self.catalog.config.name,
            &self.catalog.config.microsoft_client_id,
        );
    }

    fn edit_text(&mut self, output: horizon_client::osk::Output) {
        if self.login.edit(self.catalog.editing, &output) {
            if matches!(
                output,
                horizon_client::osk::Output::Enter | horizon_client::osk::Output::Close
            ) {
                self.catalog.editing = None;
                crate::platform::hide();
            }
        } else {
            self.catalog.edit(output, &self.home);
        }
    }
    pub(super) fn new(home: PathBuf) -> Self {
        // Upgrade existing installations that retained the old download cache.
        let cleanup_home = home.clone();
        std::thread::spawn(move || {
            if let Err(error) =
                horizon_client::official_install::cleanup_completed_downloads(&cleanup_home)
            {
                log::warn!("Could not clean completed Minecraft downloads: {error}");
            }
        });
        let mut catalog = Catalog::new(&home);
        if crate::diagnostics::previous_unclean(&home) {
            catalog.status = crate::diagnostics::summary(&home);
        }
        crate::configure_input_mode(catalog.config.input_host);
        crate::device_runtime::configure(catalog.config.device);
        Self {
            wardrobe: wardrobe::Wardrobe::new(&home),
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
            exit_confirmation: false,
            login: login::Login::new(),
            session_worker: None,
            session_ready: false,
            prepared_session: None,
            focus: None,
            diagnostic_export: None,
            web_status_at: None,
        }
    }

    fn restore_launcher(&mut self, events: &ActiveEventLoop) {
        if !horizon_client::android_runtime::take_launcher_return() {
            return;
        }
        if let Some(mut game) = self.game.take() {
            game.exiting(events);
            drop(game);
        }
        crate::document_store::set_game_active(false);
        crate::performance::stop();
        crate::platform::hide();
        horizon_client::auth::clear_current();
        self.session_ready = false;
        self.prepared_session = None;
        horizon_client::discord_presence::stop();
        horizon_client::ui_theme::reset_atlas();
        self.cursor = (0.0, 0.0);
        self.drag = None;
        self.chooser = None;
        self.exit_confirmation = false;
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
        crate::diagnostics::heartbeat("returned_to_launcher", || serde_json::json!({}));
        self.catalog.page = Page::Play;
        if let Ok(message) = std::fs::read_to_string(self.home.join("android-error.txt")) {
            self.error = Some(message);
        }
        if let Err(error) = self.open(events) {
            self.fail(error);
            events.exit();
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
        crate::diagnostics::record_error(&error.to_string());
        self.error = Some(error.to_string());
        let _ = std::fs::write(self.home.join("android-error.txt"), error.to_string());
    }

    fn open(&mut self, events: &ActiveEventLoop) -> Result<()> {
        crate::platform::set_orientation(self.catalog.config.launcher_orientation);
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
            if self.catalog.config.renderer == crate::catalog::RendererMode::OpenGl {
                horizon_client::graphics::GraphicsApi::OpenGl
            } else {
                horizon_client::graphics::GraphicsApi::Auto
            },
        )?);
        self.window = Some(window);
        self.gui = Some(gui);
        Ok(())
    }

    fn request_back(&mut self) {
        crate::platform::hide();
        if self.exit_confirmation {
            self.exit_confirmation = false;
        } else if self.login.visible {
            self.login.hide();
        } else if self.chooser.take().is_some() {
            // Back dismisses the current dialog, including the exit question.
        } else if self.catalog.editing.take().is_none() {
            self.exit_confirmation = true;
        }
        self.focus = None;
        self.pending_focus = self.exit_confirmation.then_some(pages::Action::Dismiss);
        self.targets.clear();
        self.list_targets.clear();
        self.drag = None;
        if let Err(error) = self.catalog.config.save(&self.home) {
            self.fail(error);
        }
    }

    fn action(&mut self, events: &ActiveEventLoop) -> Result<()> {
        if self.exit_confirmation
            || self.chooser.is_some()
            || self.worker.is_some()
            || self.session_worker.is_some()
        {
            return Ok(());
        }
        if self.login.pending() {
            return Ok(());
        }
        crate::diagnostics::heartbeat(
            "play_requested",
            || serde_json::json!({"renderer":self.catalog.config.renderer.label(),"resolution_percent":self.catalog.config.render_scale.percent()}),
        );
        horizon_client::android_runtime::configure_worker_limit(self.catalog.config.worker_limit);
        horizon_client::android_runtime::configure_cache_budget(
            self.catalog.config.cache_budget_mib,
        );
        crate::configure_input_mode(self.catalog.config.input_host);
        crate::device_runtime::configure(self.catalog.config.device);
        if horizon_client::official_install::ready(&self.home) {
            horizon_client::android_resources::repair_structure_transforms(
                &self.home.join("engine-data.zip"),
                &self
                    .home
                    .join(".local/horizon-client")
                    .join(horizon_core::VANILLA_VERSION),
            )?;
            crate::platform::hide();
            self.catalog.config.save(&self.home)?;
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
                let wardrobe_home = self.home.join("wardrobe");
                let (sender, receiver) = mpsc::channel();
                self.session_worker = Some(receiver);
                self.catalog.status = "Preparing account session and appearance...".into();
                std::thread::spawn(move || {
                    let result = (|| {
                        let (account, secrets) = login::Login::session(&id)?
                            .ok_or("The selected account changed. Choose Play again.")?;
                        let appearance =
                            crate::skins::cloud::fetch(&account, &secrets, &wardrobe_home)
                                .unwrap_or_default();
                        let file = crate::account::write_session_file(
                            &game_home.join("sessions"),
                            &account,
                            &secrets,
                        )?;
                        Ok(PreparedAccount {
                            key: account.key(),
                            file,
                            appearance,
                        })
                    })();
                    if let Err(unsent) = sender.send(result)
                        && let Ok(prepared) = unsent.0
                    {
                        let _ = std::fs::remove_file(prepared.file);
                    }
                });
                return Ok(());
            }
            self.session_ready = false;
            std::fs::create_dir_all(&game_home)?;
            if crate::account::Accounts::load().selected().is_none() {
                self.wardrobe
                    .prepare_game(&self.catalog.config.name, &game_home)?;
            }
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
            // The existing game loader parses android_arguments() synchronously, so the
            // prepared session path only needs to be published for this constructor call.
            horizon_client::skins::appearance::configure(&game_home)
                .map_err(std::io::Error::other)?;
            let session_file = self.prepared_session.take();
            set_session_argument(session_file.clone());
            horizon_client::android_runtime::configure_graphics(
                self.catalog.config.renderer == crate::catalog::RendererMode::OpenGl,
            );
            horizon_client::android_runtime::configure_gpu(
                self.catalog.config.renderer == crate::catalog::RendererMode::Vulkan,
            );
            // The launcher and game can use different Android Activity orientations.
            crate::platform::set_orientation(self.catalog.config.game_orientation);
            crate::document_store::set_game_active(true);
            let game = crate::game::android_game();
            set_session_argument(None);
            if let Some(file) = session_file {
                let _ = std::fs::remove_file(file);
            }
            let game = match game {
                Ok(game) => game,
                Err(error) => {
                    crate::platform::set_orientation(self.catalog.config.launcher_orientation);
                    crate::document_store::set_game_active(false);
                    crate::performance::stop();
                    return Err(error);
                }
            };
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

impl Launcher {
    fn web_setting(&mut self, payload: &str) -> std::result::Result<(), String> {
        let value: serde_json::Value = serde_json::from_str(payload)
            .map_err(|_| "Invalid settings request".to_owned())?;
        let key = value.get("key").and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Missing setting name".to_owned())?;
        let setting = value.get("value").ok_or_else(|| "Missing setting value".to_owned())?;
        match key {
            "renderer" => {
                self.catalog.config.renderer = match setting.as_str() {
                    Some("vulkan") => crate::catalog::RendererMode::Vulkan,
                    Some("open-gl") => crate::catalog::RendererMode::OpenGl,
                    Some("vulkan-compatibility") => crate::catalog::RendererMode::VulkanCompatibility,
                    _ => return Err("Unsupported rendering mode".into()),
                };
            }
            "renderScalePercent" => {
                let percent = setting.as_u64().filter(|v| (25..=100).contains(v))
                    .ok_or_else(|| "Resolution scale must be between 25 and 100".to_owned())?;
                self.catalog.config.render_scale = crate::display::RenderScale::new(percent as u32);
                crate::display::set_scale(self.catalog.config.render_scale);
            }
            "displayHz" => {
                let hz = setting.as_u64().and_then(|v| u16::try_from(v).ok())
                    .filter(|v| crate::device_preferences::DISPLAY_RATES.contains(v))
                    .ok_or_else(|| "Unsupported display refresh rate".to_owned())?;
                self.catalog.config.device.display_hz = hz;
            }
            "performanceHints" => self.catalog.config.device.performance_hints = setting.as_bool()
                .ok_or_else(|| "Invalid performance-hints value".to_owned())?,
            "gpuProfile" => self.catalog.config.device.gpu_profile = setting.as_bool()
                .ok_or_else(|| "Invalid GPU-profiling value".to_owned())?,
            "recordPerformance" => self.catalog.config.record_performance = setting.as_bool()
                .ok_or_else(|| "Invalid performance-recording value".to_owned())?,
            "launcherOrientation" => {
                self.catalog.config.launcher_orientation = serde_json::from_value(setting.clone())
                    .map_err(|_| "Unsupported launcher orientation".to_owned())?;
                crate::platform::set_orientation(self.catalog.config.launcher_orientation);
            }
            "gameOrientation" => {
                self.catalog.config.game_orientation = serde_json::from_value(setting.clone())
                    .map_err(|_| "Unsupported in-game orientation".to_owned())?;
            }
            "immersive" => self.catalog.config.screen.immersive = setting.as_bool()
                .ok_or_else(|| "Invalid immersive-mode value".to_owned())?,
            "avoidCutout" => self.catalog.config.screen.avoid_cutout = setting.as_bool()
                .ok_or_else(|| "Invalid cutout preference".to_owned())?,
            "aspect" => {
                self.catalog.config.screen.aspect = serde_json::from_value(setting.clone())
                    .map_err(|_| "Unsupported aspect ratio".to_owned())?;
            }
            _ => return Err("Unsupported Android setting".into()),
        }
        self.catalog.config.save(&self.home).map_err(|error| error.to_string())?;
        crate::device_runtime::configure(self.catalog.config.device);
        self.configure_screen();
        Ok(())
    }

    fn web_account_command(&mut self, payload: &str) -> std::result::Result<(), String> {
        let value: serde_json::Value = serde_json::from_str(payload)
            .map_err(|_| "Invalid account request".to_owned())?;
        match value.get("op").and_then(serde_json::Value::as_str) {
            Some("select") => {
                let key = value.get("key").and_then(serde_json::Value::as_str)
                    .filter(|key| !key.is_empty() && key.len() <= 80)
                    .ok_or_else(|| "Invalid account selection".to_owned())?;
                let mut accounts = crate::account::Accounts::load();
                if !accounts.accounts.iter().any(|account| account.key() == key) {
                    return Err("That saved account is no longer available".into());
                }
                accounts.selected = Some(key.to_owned());
                accounts.save()?;
                self.session_ready = false;
                self.prepared_session = None;
                self.session_worker = None;
                self.login.refresh_status();
                self.wardrobe.open(&self.catalog.config.name, &self.catalog.config.microsoft_client_id);
                Ok(())
            }
            Some("offline") => {
                let mut accounts = crate::account::Accounts::load();
                accounts.selected = None;
                accounts.save()?;
                self.session_ready = false;
                self.prepared_session = None;
                self.session_worker = None;
                self.login.refresh_status();
                self.wardrobe.open(&self.catalog.config.name, &self.catalog.config.microsoft_client_id);
                Ok(())
            }
            _ => Err("Unsupported account operation".into()),
        }
    }

    fn web_wardrobe_command(&mut self, payload: &str) -> std::result::Result<(), String> {
        let value: serde_json::Value = serde_json::from_str(payload)
            .map_err(|_| "Invalid wardrobe request".to_owned())?;
        let operation = value.get("op").and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Missing wardrobe operation".to_owned())?;
        let client_id = self.catalog.config.microsoft_client_id.clone();
        match operation {
            "refresh" => self.wardrobe.open(&self.catalog.config.name, &client_id),
            "kind" => self.wardrobe.capes = value.get("cape").and_then(serde_json::Value::as_bool).unwrap_or(false),
            "import" => {
                self.wardrobe.capes = value.get("cape").and_then(serde_json::Value::as_bool).unwrap_or(false);
                self.wardrobe.import();
            }
            "select" => {
                let id = value.get("id").and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "Missing wardrobe image".to_owned())?;
                self.wardrobe.select_id(id);
            }
            "use-cloud" => self.wardrobe.use_cloud(),
            "model" => self.wardrobe.model(),
            "hide-cape" => self.wardrobe.hide_cape(),
            "upload" => self.wardrobe.cloud(&client_id, Some(wardrobe::CloudAction::Upload)),
            "activate-cape" => {
                let id = value.get("id").and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "Missing cloud cape identifier".to_owned())?;
                self.wardrobe.activate_cloud_cape(id, &client_id);
            }
            _ => return Err("Unsupported wardrobe operation".into()),
        }
        Ok(())
    }

    fn web_command(&mut self, events: &ActiveEventLoop, action: i32, payload: &str)
        -> std::result::Result<(), String>
    {
        match action {
            1 => self.action(events).map_err(|error| error.to_string()),
            2 => {
                self.session_ready = false;
                self.prepared_session = None;
                self.session_worker = None;
                self.login.start(&self.catalog.config.microsoft_client_id);
                Ok(())
            }
            3 => {
                self.session_ready = false;
                self.prepared_session = None;
                self.session_worker = None;
                self.login.sign_out();
                self.refresh_wardrobe_account();
                Ok(())
            }
            4 => {
                let (kind_name, shader_mode) = if let Some(mode) = payload.strip_prefix("shaders:") {
                    ("shaders", if mode == "import" { crate::documents::ShaderImportMode::ImportOnly } else { crate::documents::ShaderImportMode::Convert })
                } else { (payload, crate::documents::ShaderImportMode::Convert) };
                let kind = match kind_name {
                    "mods" => horizon_client::content_files::Kind::Mods,
                    "resourcepacks" => horizon_client::content_files::Kind::Packs,
                    "shaders" => horizon_client::content_files::Kind::Shaders,
                    _ => return Err("Unsupported import type".into()),
                };
                crate::documents::begin_with_shader_mode(kind, self.catalog.config.game_home(&self.home), shader_mode)
            }
            5 => {
                let index = payload.parse::<usize>().map_err(|_| "Invalid content index".to_owned())?;
                self.catalog.toggle_local(&self.home, index);
                Ok(())
            }
            6 => {
                let index = payload.parse::<usize>().map_err(|_| "Invalid content index".to_owned())?;
                self.catalog.pending_delete = self.catalog.local.get(index).cloned();
                if self.catalog.pending_delete.is_some() { self.catalog.remove_local(&self.home); }
                Ok(())
            }
            7 => {
                self.catalog.refresh(&self.home);
                self.wardrobe.reload();
                Ok(())
            }
            8 => {
                let name = payload.trim();
                if !crate::account::valid_offline_name(name) {
                    return Err("Use 3–16 letters, numbers, or underscores for the offline name".into());
                }
                self.catalog.config.name = name.to_owned();
                self.catalog.config.save(&self.home).map_err(|error| error.to_string())?;
                self.wardrobe.open(&self.catalog.config.name, &self.catalog.config.microsoft_client_id);
                Ok(())
            }
            9 => crate::documents::resolve_web_conflict(payload),
            10 => self.web_setting(payload),
            11 => {
                let _ = crate::platform::open_documents("reports");
                Ok(())
            }
            12 => self.web_account_command(payload),
            13 => {
                let value: serde_json::Value = serde_json::from_str(payload)
                    .map_err(|_| "Invalid Ely.by sign-in request".to_owned())?;
                let username = value.get("username").and_then(serde_json::Value::as_str).unwrap_or("").trim();
                let password = value.get("password").and_then(serde_json::Value::as_str).unwrap_or("");
                let otp = value.get("otp").and_then(serde_json::Value::as_str).unwrap_or("");
                if username.is_empty() || username.len() > 256 || password.is_empty() || password.len() > 1024 || otp.len() > 64 {
                    return Err("Enter a valid Ely.by username and password. OTP is optional.".into());
                }
                self.session_ready = false;
                self.prepared_session = None;
                self.session_worker = None;
                self.login.sign_in_ely(username.to_owned(), password.to_owned(), otp.to_owned());
                Ok(())
            }
            14 => self.web_wardrobe_command(payload),
            _ => Err("Unsupported HTML launcher command".into()),
        }
    }

    fn web_status_json(&self) -> String {
        let accounts = crate::account::Accounts::load();
        let selected = accounts.selected().cloned();
        let profile = selected.as_ref().map(|account| account.name.clone());
        let provider = selected.as_ref().map(|account| match account.provider {
            crate::account::Provider::Microsoft => "Microsoft",
            crate::account::Provider::Ely => "Ely.by",
        });
        let saved_accounts: Vec<_> = accounts.accounts.iter().map(|account| serde_json::json!({
            "key": account.key(),
            "name": account.name,
            "provider": match account.provider {
                crate::account::Provider::Microsoft => "Microsoft",
                crate::account::Provider::Ely => "Ely.by",
            },
            "active": accounts.selected.as_deref() == Some(account.key().as_str())
        })).collect();
        let items: Vec<_> = self.catalog.local.iter().enumerate().map(|(index, item)| serde_json::json!({
            "index": index,
            "kind": match item.kind {
                horizon_client::content_files::Kind::Mods => "mods",
                horizon_client::content_files::Kind::Packs => "resourcepacks",
                horizon_client::content_files::Kind::Shaders => "shaders",
            },
            "file": item.file,
            "title": item.title,
            "enabled": item.enabled
        })).collect();
        let install = *self.install.lock().unwrap_or_else(|error| error.into_inner());
        let progress = crate::documents::progress_snapshot().map(|p| serde_json::json!({
            "kind": if p.kind == horizon_client::content_files::Kind::Shaders { "shaders" } else { "content" },
            "fileName": p.file_name,
            "stage": p.stage,
            "percent": p.percent,
            "active": p.active,
            "convert": p.convert
        }));
        let conflict = crate::documents::web_conflict().map(|(_, filename, message)| serde_json::json!({
            "filename": filename,
            "message": message,
            "canKeepBoth": true
        }));
        let worlds: Vec<_> = std::fs::read_dir(self.catalog.config.game_home(&self.home).join("saves"))
            .into_iter().flatten().flatten().filter_map(|entry| {
                let path = entry.path();
                if !entry.file_type().ok()?.is_dir() || !path.join("level.dat").is_file() { return None; }
                let modified = entry.metadata().ok()?.modified().ok()?
                    .duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
                Some(serde_json::json!({"name":entry.file_name().to_string_lossy(),"modified":modified}))
            }).collect();
        let status = if let Some(error) = &self.error { error.clone() }
            else if !self.login.message().is_empty() { self.login.message().to_owned() }
            else if !self.catalog.status.is_empty() { self.catalog.status.clone() }
            else { self.login.status(&self.catalog.config.name) };
        let settings = serde_json::json!({
            "renderer": match self.catalog.config.renderer {
                crate::catalog::RendererMode::Vulkan => "vulkan",
                crate::catalog::RendererMode::OpenGl => "open-gl",
                crate::catalog::RendererMode::VulkanCompatibility => "vulkan-compatibility"
            },
            "renderScalePercent": self.catalog.config.render_scale.percent(),
            "displayHz": self.catalog.config.device.display_hz,
            "performanceHints": self.catalog.config.device.performance_hints,
            "gpuProfile": self.catalog.config.device.gpu_profile,
            "recordPerformance": self.catalog.config.record_performance,
            "launcherOrientation": serde_json::to_value(self.catalog.config.launcher_orientation).unwrap_or_default(),
            "gameOrientation": serde_json::to_value(self.catalog.config.game_orientation).unwrap_or_default(),
            "immersive": self.catalog.config.screen.immersive,
            "avoidCutout": self.catalog.config.screen.avoid_cutout,
            "aspect": serde_json::to_value(self.catalog.config.screen.aspect).unwrap_or_default()
        });
        serde_json::json!({
            "connected": true,
            "installed": horizon_client::official_install::ready(&self.home),
            "installing": self.worker.is_some(),
            "busy": crate::documents::busy() || self.catalog.busy(),
            "install": {"stage":install.stage.label(),"done":install.done,"total":install.total},
            "status": status,
            "error": self.error,
            "version": horizon_core::VANILLA_VERSION,
            "account": {
                "profile": profile,
                "provider": provider,
                "signedIn": self.login.signed_in,
                "pending": self.login.pending(),
                "offlineName": self.catalog.config.name,
                "status": self.login.status(&self.catalog.config.name),
                "message": self.login.message(),
                "accounts": saved_accounts,
                "auth": self.login.web_auth_status()
            },
            "wardrobe": self.wardrobe.web_snapshot(),
            "items": items,
            "worlds": worlds,
            "settings": settings,
            "contentProgress": progress,
            "conflict": conflict
        }).to_string()
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
        crate::diagnostics::heartbeat("suspended", || serde_json::json!({}));
        if let Some(game) = &mut self.game {
            return game.suspended(events);
        }
        self.drag = None;
        if let Some(renderer) = &mut self.renderer {
            renderer.suspend_surface();
        }
    }
    fn window_event(&mut self, events: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if crate::documents::handle_event(&event) {
            return;
        }
        if matches!(event, WindowEvent::Resized(_)) {
            let _ = crate::native_ime::viewport_bottom();
        }
        if matches!(event, WindowEvent::Focused(true)) {
            self.configure_screen();
        }
        if let Some(game) = &mut self.game {
            game.window_event(events, id, event);
            self.restore_launcher(events);
            return;
        }
        let result = match event {
            WindowEvent::CloseRequested => {
                self.request_back();
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
                crate::diagnostics::heartbeat("launcher_background", || serde_json::json!({}));
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
                if self.exit_confirmation {
                    return;
                }
                let y = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 42.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32 / self.ui_density(),
                };
                if let Some(chooser) = &mut self.chooser {
                    chooser.scroll = (chooser.scroll - y).max(0.0);
                } else {
                    self.catalog.scroll = (self.catalog.scroll - y).max(0.0);
                }
                Ok(())
            }
            WindowEvent::RedrawRequested => self.draw_pages(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if event.physical_key
                    == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Escape)
                    || event.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::BrowserBack)
                {
                    self.type_key(event);
                    Ok(())
                } else if self.navigate(&event) {
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
            return;
        }
        for (action, payload) in crate::take_web_commands() {
            if let Err(error) = self.web_command(events, action, &payload) {
                self.catalog.status = error;
            }
        }
        crate::diagnostics::heartbeat("launcher", || serde_json::json!({}));
        if let Some(result) = self
            .diagnostic_export
            .as_ref()
            .and_then(|r| r.try_recv().ok())
        {
            self.diagnostic_export = None;
            self.catalog.status = match result {
                Ok(name) => {
                    let _ = crate::platform::open_documents("reports");
                    format!(
                        "Saved {name} in Reports. Review the ZIP before sharing; nothing was uploaded."
                    )
                }
                Err(error) => format!("Diagnostic export failed: {error}"),
            };
        }
        crate::platform::poll_gamepad_connections();
        if let Err(error) = self.controller_frame(events) {
            self.fail(error);
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
                let message = error.to_string();
                if message.contains("Dns Failed") || message.contains("lookup address") {
                    self.fail("Minecraft download interrupted: DNS could not resolve the download host. Check your connection, VPN or Private DNS, then retry Download. Completed files are kept.");
                } else {
                    self.fail(error);
                }
            }
        }
        for out in crate::platform::poll() {
            if matches!(out, horizon_client::osk::Output::Enter) {
                crate::platform::hide();
            }
            self.edit_text(out);
        }
        if let Some(audio) = &mut self.audio {
            audio.tick(false, &Default::default());
        }
        self.catalog.poll(&self.home);
        self.wardrobe.poll();
        if let Some(notice) = crate::documents::poll() {
            self.wardrobe.reload();
            self.catalog.status = notice.text;
            if notice.changed {
                self.catalog.refresh(&self.home);
                self.catalog.show_installed(notice.kind);
            }
        }
        for result in crate::gallery::poll() {
            self.catalog.status = result.unwrap_or_else(|e| format!("File export failed: {e}"));
        }
        if self.login.poll() {
            self.session_worker = None;
            self.prepared_session = None;
            self.session_ready = false;
            self.refresh_wardrobe_account();
        }
        if let Some(result) = self.session_worker.as_ref().and_then(|w| w.try_recv().ok()) {
            self.session_worker = None;
            match result {
                Ok(prepared) => {
                    // Only the account still selected after session refresh may be handed
                    // to the game; a changed selection invalidates this result.
                    if crate::account::Accounts::load()
                        .selected()
                        .is_none_or(|account| account.key() != prepared.key)
                    {
                        let _ = std::fs::remove_file(prepared.file);
                        self.catalog.status =
                            "The selected account changed. Choose Play again.".into();
                    } else {
                        let root = self.home.join("wardrobe");
                        let cloud = prepared.appearance.skin.as_deref().map(|path| {
                            (
                                path,
                                prepared.appearance.slim,
                                prepared.appearance.cape.as_deref(),
                            )
                        });
                        let appearance =
                            crate::skins::library::Library::load(&root).and_then(|library| {
                                library.prepare_game(
                                    &root,
                                    &prepared.key,
                                    &self.catalog.config.game_home(&self.home),
                                    cloud,
                                )
                            });
                        match appearance {
                            Ok(()) => {
                                self.prepared_session = Some(prepared.file);
                                self.session_ready = true;
                                if let Err(error) = self.action(events) {
                                    self.fail(error);
                                }
                            }
                            Err(error) => {
                                let _ = std::fs::remove_file(prepared.file);
                                self.fail(error);
                            }
                        }
                    }
                }
                Err(e) => self.fail(e),
            }
        }

        if self.web_status_at.is_none_or(|last| last.elapsed() >= Duration::from_millis(500)) {
            set_web_status(self.web_status_json());
            self.web_status_at = Some(Instant::now());
        }
        if let Some(window) = &self.window {
            window.request_redraw();
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
