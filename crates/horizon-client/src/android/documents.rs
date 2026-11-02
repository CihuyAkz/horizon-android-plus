//! SAF selection, bounded background imports and explicit collision choices.
use horizon_client::{
    Result,
    content_files::{self, Kind},
    hud::{HudAtlas, HudBatch, rgba},
    ui_theme::{Face, Ui, color},
};
use jni::{
    JNIEnv,
    objects::{JObject, JString},
};
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShaderImportMode {
    Convert,
    ImportOnly,
}
struct Request {
    kind: Kind,
    home: PathBuf,
    cache: PathBuf,
    shader_mode: ShaderImportMode,
}
struct Conflict {
    kind: Kind,
    staged: PathBuf,
    target: PathBuf,
    message: String,
}
#[derive(Clone)]
pub struct Notice {
    pub text: String,
    pub changed: bool,
    pub kind: Option<Kind>,
}
#[derive(Clone)]
pub struct ProgressSnapshot {
    pub kind: Kind,
    pub file_name: String,
    pub stage: String,
    pub percent: u8,
    pub active: bool,
    pub convert: bool,
    pub waiting_for_choice: bool,
    finished_at: Option<Instant>,
}
#[derive(Default)]
struct State {
    request: Option<Request>,
    working: bool,
    working_kind: Option<Kind>,
    conflict: Option<Conflict>,
    notice: Option<Notice>,
    hits: Vec<([f32; 4], Choice)>,
    toast: Option<(String, std::time::Instant)>,
    cursor: (f32, f32),
    web_result_id: u64,
    web_result: Option<(Kind, String)>,
    progress: Option<ProgressSnapshot>,
}
#[derive(Clone, Copy)]
pub enum Choice {
    KeepBoth,
    Replace,
    Cancel,
}
static STATE: Mutex<State> = Mutex::new(State {
    request: None,
    working: false,
    working_kind: None,
    conflict: None,
    notice: None,
    hits: Vec::new(),
    toast: None,
    cursor: (0.0, 0.0),
    web_result_id: 0,
    web_result: None,
    progress: None,
});
static CANCEL: AtomicBool = AtomicBool::new(false);
pub fn busy() -> bool {
    let s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    s.request.is_some() || s.working || s.conflict.is_some()
}
pub fn conflict() -> bool {
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .conflict
        .is_some()
}
pub fn shader_conflict() -> bool {
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .conflict
        .as_ref()
        .is_some_and(|conflict| conflict.kind == Kind::Shaders)
}
pub fn shader_operation_pending() -> bool {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state
        .request
        .as_ref()
        .is_some_and(|request| request.kind == Kind::Shaders)
        || state.working_kind == Some(Kind::Shaders)
        || state
            .progress
            .as_ref()
            .is_some_and(|progress| {
                progress.kind == Kind::Shaders && (progress.active || progress.waiting_for_choice)
            })
        || state
            .conflict
            .as_ref()
            .is_some_and(|conflict| conflict.kind == Kind::Shaders)
}
fn notice_for_kind(text: impl Into<String>, changed: bool, kind: Option<Kind>) {
    let text = text.into();
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    s.toast = Some((text.clone(), std::time::Instant::now()));
    s.notice = Some(Notice { text, changed, kind });
}
fn notice(text: impl Into<String>, changed: bool) {
    notice_for_kind(text, changed, None);
}
fn remove_path(path: &std::path::Path) {
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}
fn imported(text: impl Into<String>, kind: Kind) {
    let text = text.into();
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    s.web_result_id = s.web_result_id.wrapping_add(1);
    s.web_result = Some((kind, text.clone()));
    s.toast = Some((text.clone(), std::time::Instant::now()));
    s.notice = Some(Notice {
        text: text.clone(),
        changed: true,
        kind: Some(kind),
    });
    if kind == Kind::Shaders {
        if let Some(progress) = s.progress.as_mut() {
            progress.stage = "Completed".into();
            progress.percent = 100;
            progress.active = false;
            progress.waiting_for_choice = false;
            progress.finished_at = Some(Instant::now());
        }
    }
}

pub fn progress_snapshot() -> Option<ProgressSnapshot> {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.progress.as_ref().is_some_and(|progress| {
        !progress.active
            && !progress.waiting_for_choice
            && progress
                .finished_at
                .is_some_and(|time| time.elapsed() > Duration::from_secs(12))
    }) {
        state.progress = None;
    }
    state.progress.clone()
}

fn start_shader_progress(file_name: &str, convert: bool) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.progress = Some(ProgressSnapshot {
        kind: Kind::Shaders,
        file_name: file_name.to_owned(),
        stage: "Preparing shader archive".into(),
        percent: 0,
        active: true,
        convert,
        waiting_for_choice: false,
        finished_at: None,
    });
}

fn update_shader_progress(stage: &str, percent: u8) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(progress) = state.progress.as_mut() {
        if progress.active {
            progress.stage = stage.to_owned();
            progress.percent = percent.min(99);
        }
    }
}

fn update_shader_progress_range(stage: &str, start: u8, span: u8, done: u64, total: u64) {
    let ratio = if total == 0 {
        1.0
    } else {
        done.min(total) as f32 / total as f32
    };
    update_shader_progress(
        stage,
        start.saturating_add((f32::from(span) * ratio).round() as u8),
    );
}

fn wait_for_shader_conflict() {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(progress) = state.progress.as_mut() {
        progress.stage = "Waiting for conflict choice".into();
        progress.percent = 98;
        progress.active = false;
        progress.waiting_for_choice = true;
        progress.finished_at = None;
    }
}

fn finish_shader_failure(error: &str) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(progress) = state.progress.as_mut() else {
        return;
    };
    if !progress.active && !progress.waiting_for_choice {
        return;
    }
    let cancelled = error.to_ascii_lowercase().contains("cancelled");
    progress.stage = if cancelled { "Cancelled" } else { "Failed" }.into();
    progress.active = false;
    progress.waiting_for_choice = false;
    progress.finished_at = Some(Instant::now());
}
fn web_result(kind: Kind, text: impl Into<String>) {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    s.web_result_id = s.web_result_id.wrapping_add(1);
    s.web_result = Some((kind, text.into()));
}
pub fn take_web_result() -> Option<(u64, Kind, String)> {
    let s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    s.web_result
        .as_ref()
        .map(|(kind, message)| (s.web_result_id, *kind, message.clone()))
}
pub fn poll() -> Option<Notice> {
    if crate::document_provider::take_changes() {
        return Some(Notice {
            text: "Content files changed. Reload packs to apply.".into(),
            changed: true,
            kind: None,
        });
    }
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .notice
        .take()
}
pub fn begin(kind: Kind, home: PathBuf) -> std::result::Result<(), String> {
    begin_with_shader_mode(kind, home, ShaderImportMode::Convert)
}
pub fn begin_with_shader_mode(
    kind: Kind,
    home: PathBuf,
    shader_mode: ShaderImportMode,
) -> std::result::Result<(), String> {
    if busy() {
        return Err("Finish or cancel the current import first".into());
    }
    CANCEL.store(false, Ordering::Relaxed);
    crate::platform::hide();
    let cache = crate::platform::cache_dir()
        .ok_or("Could not access the Android import cache")?
        .join("content-imports");
    let shader_mode = if kind == Kind::Shaders {
        shader_mode
    } else {
        ShaderImportMode::Convert
    };
    STATE.lock().unwrap_or_else(|e| e.into_inner()).request = Some(Request {
        kind,
        home,
        cache,
        shader_mode,
    });
    if !crate::native_ime::pick_document() {
        picker_failed("Android document picker is unavailable");
        return Err("Android document picker is unavailable".into());
    }
    let label = match (kind, shader_mode) {
        (Kind::Shaders, ShaderImportMode::ImportOnly) => "shader without conversion",
        (Kind::Shaders, ShaderImportMode::Convert) => "shader with conversion",
        _ => kind.label(),
    };
    notice_for_kind(format!("Choose a {label} file"), false, Some(kind));
    Ok(())
}
pub fn picker_failed(error: &str) {
    let kind = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .request
        .take()
        .map(|request| request.kind);
    if let Some(kind) = kind {
        web_result(kind, format!("Import failed: {error}"));
    }
    notice_for_kind(error, false, kind);
}

pub fn web_conflict() -> Option<(Kind, String, String)> {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.conflict.as_ref().map(|conflict| {
        (
            conflict.kind,
            conflict
                .target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            conflict.message.clone(),
        )
    })
}

pub fn resolve_web_conflict(choice: &str) -> std::result::Result<(), String> {
    let choice = match choice {
        "keep-both" => Choice::KeepBoth,
        "replace" => Choice::Replace,
        "cancel" => Choice::Cancel,
        _ => return Err("Unsupported file-conflict option".into()),
    };
    resolve(choice);
    Ok(())
}
/// Called by the approved Activity result bridge, never from a game tick.
/// Keep a conventional JNI export as well as explicit registration: Android
/// may restore a picker result before the IME bridge has been initialized.
#[unsafe(export_name = "Java_dev_horizon_android_HorizonActivity_nativeDocumentResult")]
pub extern "system" fn document_result(
    mut env: JNIEnv<'_>,
    activity: JObject<'_>,
    request: i32,
    result: i32,
    uri: JString<'_>,
) {
    if request != 4101 {
        return;
    }
    let pending = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .request
        .take();
    let Some(pending) = pending else {
        notice("Import interrupted. Choose the file again.", false);
        return;
    };
    if result != -1 || uri.is_null() {
        web_result(pending.kind, "Import cancelled. No files were changed.");
        notice_for_kind("Import cancelled", false, Some(pending.kind));
        return;
    }
    let setup = (|| -> jni::errors::Result<_> {
        let text: String = env.get_string(&uri)?.into();
        let activity = env.new_global_ref(activity)?;
        Ok((env.get_java_vm()?, activity, text))
    })();
    let Ok((vm, activity, uri)) = setup else {
        let _ = env.exception_clear();
        web_result(pending.kind, "Import failed: could not read the selected document.");
        notice_for_kind("Could not read the selected document", false, Some(pending.kind));
        return;
    };
    if !uri.starts_with("content://") || uri.len() > 8192 {
        web_result(pending.kind, "Import failed: unsupported document URI.");
        notice_for_kind("Unsupported document URI", false, Some(pending.kind));
        return;
    }
    let operation_kind = pending.kind;
    let operation_is_shader = pending.kind == Kind::Shaders;
    {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.working = true;
        state.working_kind = Some(operation_kind);
    }
    let spawned = std::thread::Builder::new()
        .name("Content import".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let mut env = vm.attach_current_thread()?;
                let fallback = if pending.kind == Kind::Mods {
                    "import.hmod"
                } else {
                    "import.zip"
                };
                let opened =
                    crate::document_bridge::open(&mut env, activity.as_obj(), &uri, fallback);
                if opened.is_err() {
                    let _ = env.exception_clear();
                }
                let (input, name) = opened.map_err(|e| e.to_string())?;
                stage(pending, input, name)
            })();
            {
                let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
                state.working = false;
                state.working_kind = None;
            }
            if let Err(error) = result {
                if operation_is_shader {
                    finish_shader_failure(&error.to_string());
                }
                let message = format!("Import failed: {error}");
                web_result(operation_kind, message.clone());
                notice_for_kind(message, false, Some(operation_kind));
            }
        });
    if let Err(error) = spawned {
        {
            let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
            state.working = false;
            state.working_kind = None;
        }
        if operation_is_shader {
            finish_shader_failure(&error.to_string());
        }
        let message = format!("Import failed: {error}");
        web_result(operation_kind, message.clone());
        notice_for_kind(message, false, Some(operation_kind));
    }
}
fn stage(request: Request, mut input: std::fs::File, name: String) -> Result<()> {
    if !content_files::plain_name(&name) || !request.kind.accepts(&name) {
        return Err("Selected file has the wrong extension for this folder".into());
    }
    let is_shader = request.kind == Kind::Shaders;
    if is_shader {
        start_shader_progress(&name, request.shader_mode == ShaderImportMode::Convert);
    }
    let input_size = input.metadata().ok().map(|metadata| metadata.len()).unwrap_or(0);
    let dir = request.home.join(request.kind.folder());
    std::fs::create_dir_all(&dir)?;
    if std::fs::symlink_metadata(&dir)?.file_type().is_symlink() {
        return Err("Refusing a redirected content directory".into());
    }
    std::fs::create_dir_all(&request.cache)?;
    if std::fs::symlink_metadata(&request.cache)?
        .file_type()
        .is_symlink()
    {
        return Err("Refusing a redirected import cache".into());
    }
    for entry in std::fs::read_dir(&request.cache)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".import-") && name.ends_with(".part") && entry.file_type()?.is_file() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let staged = request.cache.join(format!(".import-{stamp}.part"));
    let result = (|| -> Result<()> {
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        content_files::copy_bounded(&mut input, &mut output, |bytes| {
            notice_for_kind(
                format!("Importing {}: {} KiB", request.kind.label(), bytes / 1024),
                false,
                Some(request.kind),
            );
            if is_shader {
                let percent = if input_size > 0 {
                    ((bytes.min(input_size) as f64 / input_size as f64) * 20.0).round() as u8
                } else {
                    0
                };
                update_shader_progress("Copying shader archive", percent);
            }
            !CANCEL.load(Ordering::Relaxed)
        })?;
        output.sync_all()?;
        drop(output);
        notice_for_kind("Checking archive...", false, Some(request.kind));
        if is_shader {
            update_shader_progress("Validating shader archive", 20);
        }
        let native_shader = content_files::validate_with_progress(
            &staged,
            request.kind,
            &name,
            || CANCEL.load(Ordering::Relaxed),
            |done, total| {
                if is_shader {
                    update_shader_progress_range(
                        "Validating shader archive",
                        20,
                        35,
                        done,
                        total,
                    );
                }
            },
        )?;
        if is_shader {
            update_shader_progress("Preparing shader files", 55);
        }
        let name = if request.kind == Kind::Mods {
            format!("{}.hmod", horizon_mods::Package::read(&staged)?.manifest.id)
        } else if native_shader || name.to_ascii_lowercase().ends_with(".hshader") {
            let pack = horizon_shader_api::Pack::read(&staged)?;
            horizon_shader_api::check(&pack).map_err(|e| format!("Invalid shader pack: {e:?}"))?;
            let mut path = PathBuf::from(name);
            path.set_extension("hshader");
            path.to_string_lossy().into_owned()
        } else {
            name
        };
        if !content_files::plain_name(&name) {
            return Err("Package has an invalid ID or filename".into());
        }
        if CANCEL.load(Ordering::Relaxed) {
            return Err("Import cancelled".into());
        }
        let classic_shader = request.kind == Kind::Shaders
            && name.to_ascii_lowercase().ends_with(".zip")
            && !native_shader;
        let mut shader_compatibility = None;
        let (target, installed_path) = if classic_shader {
            let stem = PathBuf::from(&name)
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or("Shader ZIP has an invalid filename")?
                .to_owned();
            if !content_files::plain_name(&stem) {
                return Err("Shader ZIP has an invalid folder name".into());
            }
            let extracted = dir.join(format!(".import-{stamp}"));
            if is_shader {
                update_shader_progress("Extracting shader files", 55);
            }
            content_files::extract_shader_archive_with_progress(
                &staged,
                &extracted,
                || CANCEL.load(Ordering::Relaxed),
                |done, total| {
                    if is_shader {
                        update_shader_progress_range(
                            "Extracting shader files",
                            55,
                            30,
                            done,
                            total,
                        );
                    }
                },
            )?;
            if request.shader_mode == ShaderImportMode::Convert {
                if is_shader {
                    update_shader_progress("Checking Horizon compatibility", 88);
                }
                shader_compatibility = content_files::check_and_convert_classic_shader(&extracted)
                    .map_err(|error| {
                        remove_path(&extracted);
                        error
                    })?;
                if is_shader {
                    update_shader_progress("Applying Horizon shader fix", 93);
                }
            } else if is_shader {
                update_shader_progress("Preparing imported shader files", 93);
            }
            let _ = std::fs::remove_file(&staged);
            (dir.join(stem), extracted)
        } else {
            (dir.join(&name), staged.clone())
        };
        let outcome = if classic_shader {
            if request.shader_mode == ShaderImportMode::ImportOnly {
                format!(
                    "Imported {name} without conversion. Shader files were extracted unchanged."
                )
            } else {
                match shader_compatibility {
                    Some(true) => format!(
                        "Converted {name} for Horizon. Only shaders/lib/common.glsl was changed; other shader files were preserved."
                    ),
                    Some(false) => format!(
                        "{name} is already Horizon-compatible. No shader source files were changed."
                    ),
                    None => format!(
                        "{name} was imported unchanged. No verified Horizon conversion rule matched this shader ZIP."
                    ),
                }
            }
        } else {
            format!(
                "Imported {} into {}. Enable it in game settings.",
                target.file_name().unwrap_or_default().to_string_lossy(),
                request.kind.label()
            )
        };
        if is_shader {
            update_shader_progress("Installing shader pack", 97);
        }
        if std::fs::symlink_metadata(&target).is_ok() {
            STATE.lock().unwrap_or_else(|e| e.into_inner()).conflict = Some(Conflict {
                kind: request.kind,
                staged: installed_path,
                target,
                message: outcome,
            });
            if is_shader {
                wait_for_shader_conflict();
            }
            notice_for_kind(
                "Pack already exists: choose Keep both, Replace or Cancel",
                false,
                Some(request.kind),
            );
        } else {
            if let Err(error) = std::fs::rename(&installed_path, &target) {
                remove_path(&installed_path);
                return Err(error.into());
            }
            imported(outcome, request.kind);
        }
        Ok(())
    })();
    if result.is_err() {
        remove_path(&staged);
    }
    result
}
pub fn resolve(choice: Choice) {
    let c = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .conflict
        .take();
    let Some(mut c) = c else {
        return;
    };
    if matches!(choice, Choice::Cancel) {
        remove_path(&c.staged);
        if c.kind == Kind::Shaders {
            finish_shader_failure("Import cancelled");
        }
        web_result(c.kind, "Import cancelled. The existing content was left unchanged.");
        notice_for_kind("Import cancelled", false, Some(c.kind));
        return;
    }
    let result = (|| -> Result<()> {
        if matches!(choice, Choice::Replace) {
            let staged_dir = std::fs::symlink_metadata(&c.staged)
                .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink());
            let target_meta = std::fs::symlink_metadata(&c.target)
                .map_err(|_| "The existing pack is no longer available")?;
            if target_meta.file_type().is_symlink()
                || target_meta.is_dir() != staged_dir
                || (!staged_dir && !target_meta.is_file())
            {
                return Err("Cannot replace a different file type or symbolic link".into());
            }
            if staged_dir {
                let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
                let backup = c
                    .target
                    .parent()
                    .ok_or("Invalid shader folder")?
                    .join(format!(".replace-{stamp}"));
                std::fs::rename(&c.target, &backup)?;
                if let Err(error) = std::fs::rename(&c.staged, &c.target) {
                    let _ = std::fs::rename(&backup, &c.target);
                    return Err(error.into());
                }
                remove_path(&backup);
                return Ok(());
            }
        }
        if matches!(choice, Choice::KeepBoth) {
            if c.kind == Kind::Mods {
                return Err(
                    "A mod ID cannot be installed twice; choose Replace to update it".into(),
                );
            }
            let mut stem = c
                .target
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            while stem.len() > 150 {
                stem.pop();
            }
            let ext = c
                .target
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let dir = c
                .target
                .parent()
                .ok_or("Invalid content folder")?
                .to_owned();
            for i in 1..=10000 {
                let candidate = dir.join(if ext.is_empty() {
                    format!("{stem} ({i})")
                } else {
                    format!("{stem} ({i}).{ext}")
                });
                if std::fs::symlink_metadata(&candidate).is_err() {
                    c.target = candidate;
                    break;
                }
                if i == 10000 {
                    return Err("Too many filename conflicts".into());
                }
            }
        }
        std::fs::rename(&c.staged, &c.target)?;
        Ok(())
    })();
    match result {
        Ok(()) => imported(c.message, c.kind),
        Err(e) => {
            remove_path(&c.staged);
            if c.kind == Kind::Shaders {
                finish_shader_failure(&e.to_string());
            }
            let message = format!("Import failed: {e}");
            web_result(c.kind, message.clone());
            notice_for_kind(message, false, Some(c.kind));
        }
    }
}
pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
    let kind = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(request) = state.request.take() {
            Some(request.kind)
        } else if let Some(conflict) = state.conflict.as_ref() {
            Some(conflict.kind)
        } else {
            state
                .progress
                .as_ref()
                .filter(|progress| progress.active || progress.waiting_for_choice)
                .map(|progress| progress.kind)
                .or(state.working_kind)
        }
    };
    if conflict() {
        resolve(Choice::Cancel);
    } else {
        notice_for_kind("Import cancelled", false, kind);
    }
}
pub fn tap(x: f32, y: f32) -> bool {
    if !busy() {
        return false;
    }
    let choice = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .hits
        .iter()
        .find(|(r, _)| x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3])
        .map(|(_, c)| *c);
    if let Some(choice) = choice {
        if matches!(choice, Choice::Cancel) {
            cancel();
        } else {
            resolve(choice);
        }
    }
    true
}
pub fn draw(batch: &mut HudBatch, atlas: &HudAtlas) {
    if shader_operation_pending() {
        return;
    }
    let mut ui = Ui::mobile(atlas, batch, (0.0, 0.0));
    draw_ui(&mut ui);
    ui.finish();
}
/// Launcher overlays must use its bundled font, including before game fonts exist.
pub fn draw_ui(ui: &mut Ui<'_>) {
    static ANIMATION_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    s.hits.clear();
    if s.conflict.is_none() && !s.working {
        // The launcher already shows the result in its status line. Keep only
        // actionable overlays (progress/cancel and a name-conflict choice).
        return;
    }
    let (name, choices) = if let Some(c) = &s.conflict {
        (
            c.target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            if c.kind == Kind::Mods {
                vec![("Replace", Choice::Replace), ("Cancel", Choice::Cancel)]
            } else {
                vec![
                    ("Keep both", Choice::KeepBoth),
                    ("Replace", Choice::Replace),
                    ("Cancel", Choice::Cancel),
                ]
            },
        )
    } else {
        (
            s.toast.as_ref().map(|v| v.0.clone()).unwrap_or_default(),
            vec![("Cancel", Choice::Cancel)],
        )
    };
    let w = 440.0f32.min(ui.w - 24.0);
    let vertical = w < 320.0;
    let height = if vertical {
        92.0 + choices.len() as f32 * 44.0
    } else {
        144.0
    };
    let x = (ui.w - w) / 2.0;
    let y = ((ui.h - height) / 2.0).max(0.0);
    ui.rounded(0.0, 0.0, ui.w, ui.h, 0.0, rgba(0, 0, 0, 180));
    ui.rounded(x, y, w, height, 2.0, color::bg());
    ui.text(
        Face::Medium,
        20.0,
        x + 12.0,
        y + 12.0,
        color::text(),
        if s.conflict.is_some() {
            "File exists"
        } else {
            "Importing file"
        },
    );
    let name = ui.fit_text(Face::Regular, 16.0, &name, w - 24.0);
    ui.text(Face::Regular, 16.0, x + 12.0, y + 46.0, color::muted(), &name);
    if s.conflict.is_none() {
        let track_x = x + 12.0;
        let track_width = w - 24.0;
        let track_y = y + 76.0;
        ui.rounded(track_x, track_y, track_width, 3.0, 1.5, color::border());
        let elapsed = ANIMATION_START.get_or_init(Instant::now).elapsed().as_secs_f32();
        let shuttle_width = (track_width * 0.28).clamp(22.0, 72.0);
        let shuttle_x = track_x
            + (elapsed * 150.0).rem_euclid(track_width + shuttle_width)
            - shuttle_width;
        let shuttle_left = shuttle_x.max(track_x);
        let shuttle_right = (shuttle_x + shuttle_width).min(track_x + track_width);
        if shuttle_right > shuttle_left {
            ui.rounded(
                shuttle_left,
                track_y,
                shuttle_right - shuttle_left,
                3.0,
                1.5,
                color::accent(),
            );
        }
    }
    let width = (w - 24.0) / choices.len() as f32;
    for (i, (label, choice)) in choices.into_iter().enumerate() {
        let rect = if vertical {
            [x + 12.0, y + 86.0 + i as f32 * 44.0, w - 24.0, 38.0]
        } else {
            [x + 12.0 + i as f32 * width, y + 92.0, width - 4.0, 38.0]
        };
        ui.rounded(rect[0], rect[1], rect[2], rect[3], 2.0, color::raised());
        ui.text(
            Face::Medium,
            16.0,
            rect[0] + 8.0,
            rect[1] + 7.0,
            color::text(),
            label,
        );
        s.hits.push((
            [
                rect[0] * ui.density,
                rect[1] * ui.density,
                rect[2] * ui.density,
                rect[3] * ui.density,
            ],
            choice,
        ));
    }
}
pub fn handle_event(event: &winit::event::WindowEvent) -> bool {
    use winit::{
        event::{ElementState, MouseButton, TouchPhase, WindowEvent},
        keyboard::{Key, KeyCode, NamedKey, PhysicalKey},
    };
    if !busy() {
        return false;
    }
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            STATE.lock().unwrap_or_else(|e| e.into_inner()).cursor =
                (position.x as f32, position.y as f32);
            true
        }
        WindowEvent::MouseInput {
            button: MouseButton::Left,
            state,
            ..
        } => {
            if *state == ElementState::Released {
                let (x, y) = STATE.lock().unwrap_or_else(|e| e.into_inner()).cursor;
                tap(x, y);
            }
            true
        }
        WindowEvent::Touch(t) => {
            if t.phase == TouchPhase::Ended {
                tap(t.location.x as f32, t.location.y as f32);
            }
            true
        }
        WindowEvent::KeyboardInput { event, .. } => {
            if event.state == ElementState::Pressed {
                if event.physical_key == PhysicalKey::Code(KeyCode::Escape)
                    || event.logical_key == Key::Named(NamedKey::BrowserBack)
                {
                    cancel();
                } else if event.physical_key == PhysicalKey::Code(KeyCode::Enter) && conflict() {
                    let native = STATE
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .conflict
                        .as_ref()
                        .is_some_and(|c| c.kind == Kind::Mods);
                    resolve(if native {
                        Choice::Cancel
                    } else {
                        Choice::KeepBoth
                    });
                }
            }
            true
        }
        _ => false,
    }
}
