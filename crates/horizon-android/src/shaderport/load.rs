//! Loads a pack the way `game/shader_packs.rs` does, for each dimension, and sorts the loader's
//! notes into programs loaded and programs skipped with their errors.

use std::collections::{BTreeMap, BTreeSet};

use horizon_shaders::{GameInputs, GbuffersInputs, Macros, PostProgram, Properties, dimension_folder, load_post};

use crate::shaderport::files::Files;

/// One error the loader gave for a skipped program, at a line of an original file.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Problem {
    /// The file's path inside `shaders/` (as the preprocessor names it).
    pub file: String,
    pub line: u32,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Skip {
    /// The program's path inside `shaders/`, without extension (`world0/composite2`).
    pub program: String,
    pub geometry: bool,
    pub problems: Vec<Problem>,
}

#[derive(Clone, Debug, Default)]
pub struct DimEval {
    pub id: i32,
    pub folder: String,
    pub loaded: BTreeSet<String>,
    pub skipped: Vec<Skip>,
    pub notes: Vec<String>,
    /// The loader panicked (the game turns such a pack off).
    pub panicked: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Eval {
    pub dims: Vec<DimEval>,
    /// Stage files no loaded or skipped program accounts for: program kinds Horizon does not draw
    /// (yet), or programs the pack's options turn off.
    pub unused: BTreeSet<String>,
    pub properties_notes: Vec<String>,
}

/// Vertex inputs like the game's (its renderer writes the real ones; these are the ones
/// Horizon's own `real_packs` test uses).
pub(crate) fn inputs() -> GameInputs {
    let terrain = GbuffersInputs {
        declarations: "layout(location = 0) in vec3 hz_v_pos;".into(),
        body: "    hz_Vertex = vec4(hz_v_pos, 1.0);\n    hz_Color = vec4(1.0);\n".into(),
        attributes: vec![("mc_Entity".into(), "vec4(-1.0)".into())],
        ..Default::default()
    };
    GameInputs {
        terrain: Some(terrain.clone()),
        sky: Some(terrain.clone()),
        celestial: Some(terrain.clone()),
        entities: Some(terrain.clone()),
        clouds: Some(terrain.clone()),
        weather: Some(terrain.clone()),
        hand: Some(terrain),
    }
}

pub fn evaluate(files: &Files) -> Result<Eval, String> {
    let pack = files.pack()?;
    let macros = Macros::standard();
    let text = pack.read_text("shaders.properties").unwrap_or_default();
    let properties = Properties::parse(&text, &macros);
    let mut eval = Eval {
        properties_notes: properties
            .notes
            .iter()
            .map(|n| format!("shaders.properties:{}: {}", n.line, n.message))
            .collect(),
        ..Default::default()
    };
    // Android and WebAssembly load dimensions in sequence to limit memory and thread pressure.
    // Desktop builds load them in parallel, each from its own copy of the pack.
    #[cfg(any(target_family = "wasm", target_os = "android"))]
    let dims: Vec<Result<DimEval, String>> = [0, -1, 1]
        .into_iter()
        .map(|id| -> Result<DimEval, String> {
            let pack = files.pack()?;
            Ok(load_dimension(&pack, id, &properties))
        })
        .collect();
    #[cfg(not(any(target_family = "wasm", target_os = "android")))]
    let dims: Vec<Result<DimEval, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = [0, -1, 1]
            .into_iter()
            .map(|id| {
                let properties = &properties;
                scope.spawn(move || -> Result<DimEval, String> {
                    let pack = files.pack()?;
                    Ok(load_dimension(&pack, id, properties))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("thread panicked".into()))).collect()
    });
    let mut seen_folders = BTreeSet::new();
    for d in dims {
        let d = d?;
        seen_folders.insert(d.folder.clone());
        eval.dims.push(d);
    }
    unused_files(&pack, &mut eval, seen_folders);
    Ok(eval)
}

fn load_dimension(pack: &horizon_shaders::Pack, id: i32, properties: &Properties) -> DimEval {
    let macros = Macros::standard();
    let inputs = inputs();
    {
        let folder = dimension_folder(pack, id);
        let mut dim = DimEval {
            id,
            folder: folder.clone(),
            ..Default::default()
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_post(pack, &folder, properties, &macros, &inputs)
        }));
        match result {
            Ok(post) => {
                let prefix = |n: &str| {
                    if folder.is_empty() {
                        n.to_owned()
                    } else {
                        format!("{folder}/{n}")
                    }
                };
                let singles: [&Option<PostProgram>; 16] = [
                    &post.terrain,
                    &post.water,
                    &post.sky,
                    &post.sky_textured,
                    &post.entities,
                    &post.block,
                    &post.entities_translucent,
                    &post.particles,
                    &post.clouds,
                    &post.weather,
                    &post.hand,
                    &post.shadow,
                    &post.shadow_water,
                    &post.shadow_entities,
                    &post.shadow_block,
                    &None,
                ];
                for p in post
                    .programs
                    .iter()
                    .chain(post.shadow_comp.iter())
                    .chain(singles.iter().filter_map(|p| p.as_ref()))
                {
                    dim.loaded.insert(prefix(&p.name));
                }
                sort_notes(
                    &post.notes,
                    &prefix,
                    &mut dim,
                );
                // What the game's GPU compile refuses (see gpu.rs): skipped there too.
                for p in post
                    .programs
                    .iter()
                    .chain(post.shadow_comp.iter())
                    .chain(singles.iter().filter_map(|p| p.as_ref()))
                {
                    let why = crate::shaderport::gpu::check(p, &post.config);
                    if why.is_empty() {
                        continue;
                    }
                    let name = prefix(&p.name);
                    if dim.skipped.iter().any(|s| s.program == name) {
                        continue; // one program can stand for two kinds of geometry
                    }
                    dim.loaded.remove(&name);
                    dim.skipped.push(Skip {
                        program: name.clone(),
                        geometry: false,
                        problems: why
                            .into_iter()
                            .map(|message| Problem { file: name.clone(), line: 0, message })
                            .collect(),
                    });
                }
                if std::env::var_os("HZ_BUFFERS").is_some() {
                    for (i, b) in post.config.buffers.iter().enumerate() {
                        eprintln!("  [buffers] {}: colortex{i} {:?} clear {} mipmap {}", dim.folder, b.format, b.clear, b.mipmap);
                    }
                }
                dim.notes.extend(post.config.notes.iter().cloned());
            }
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into());
                dim.panicked = Some(msg);
            }
        }
        dim
    }
}

/// Stage files of the folders the game reads that nothing accounts for.
fn unused_files(pack: &horizon_shaders::Pack, eval: &mut Eval, seen_folders: BTreeSet<String>) {
    let accounted: BTreeSet<String> = eval
        .dims
        .iter()
        .flat_map(|d| d.loaded.iter().cloned().chain(d.skipped.iter().map(|s| s.program.clone())))
        .collect();
    for folder in seen_folders {
        for file in pack.list(&folder) {
            let Some((stem, ext)) = file.rsplit_once('.') else { continue };
            if !["vsh", "fsh", "gsh", "csh"].contains(&ext) {
                continue;
            }
            let program = if folder.is_empty() {
                stem.to_owned()
            } else {
                format!("{folder}/{stem}")
            };
            // Horizon's loader has no compute stage: every `.csh` goes unrun.
            if ext == "csh" {
                eval.unused.insert(format!("{program}.csh"));
            } else if !accounted.contains(&program) {
                eval.unused.insert(program);
            }
        }
    }
}

/// Notes come as `file:line: message` lines followed by `name: skipped`, or
/// `name: uses a geometry stage (not supported); skipped`, or stand alone.
fn sort_notes(notes: &[String], prefix: &dyn Fn(&str) -> String, dim: &mut DimEval) {
    let mut pending: Vec<Problem> = Vec::new();
    for note in notes {
        if let Some(name) = note.strip_suffix(": uses a geometry stage (not supported); skipped") {
            dim.skipped.push(Skip {
                program: prefix(name),
                geometry: true,
                problems: std::mem::take(&mut pending),
            });
        } else if let Some(name) = note.strip_suffix(": skipped")
            && !name.contains(' ')
        {
            dim.skipped.push(Skip {
                program: prefix(name),
                geometry: false,
                problems: std::mem::take(&mut pending),
            });
        } else if let Some(p) = parse_problem(note) {
            pending.push(p);
        } else {
            dim.notes.push(note.clone());
        }
    }
    dim.notes.extend(pending.into_iter().map(|p| format!("{}:{}: {}", p.file, p.line, p.message)));
}

fn parse_problem(note: &str) -> Option<Problem> {
    // `file:line: message`; files have no `:` (paths inside the pack).
    let (file, rest) = note.split_once(':')?;
    let (line, message) = rest.split_once(": ")?;
    let line: u32 = line.parse().ok()?;
    if file.contains(' ') {
        return None;
    }
    Some(Problem {
        file: file.to_owned(),
        line,
        message: message.to_owned(),
    })
}

impl Eval {
    pub fn loaded_count(&self) -> usize {
        self.dims.iter().map(|d| d.loaded.len()).sum()
    }
    pub fn skipped_count(&self) -> usize {
        self.dims.iter().map(|d| d.skipped.len()).sum()
    }
    pub fn panicked(&self) -> bool {
        self.dims.iter().any(|d| d.panicked.is_some())
    }
    /// Lower is better: programs skipped (a panic counts as everything skipped).
    pub fn score(&self) -> usize {
        self.skipped_count() + if self.panicked() { 10_000 } else { 0 }
    }
    /// Each skipped program's first problem: a rule that changes these moved a program on to
    /// its next error, which is progress even when it still does not load.
    pub fn signature(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .dims
            .iter()
            .flat_map(|d| d.skipped.iter())
            .map(|s| match s.problems.first() {
                Some(p) => format!("{} {}:{} {}", s.program, p.file, p.line, p.message),
                None => s.program.clone(),
            })
            .collect();
        v.sort();
        v
    }
    pub fn skipped_names(&self) -> Vec<String> {
        let set: BTreeSet<String> = self
            .dims
            .iter()
            .flat_map(|d| d.skipped.iter().map(|s| s.program.clone()))
            .collect();
        set.into_iter().collect()
    }
    /// Every skipped program once (dimensions often share the base folder's programs).
    pub fn unique_skips(&self) -> Vec<Skip> {
        let mut by: BTreeMap<String, Skip> = BTreeMap::new();
        for d in &self.dims {
            for s in &d.skipped {
                by.entry(s.program.clone()).or_insert_with(|| s.clone());
            }
        }
        by.into_values().collect()
    }
    pub fn summary(&self) -> String {
        format!(
            "{} programs load, {} skipped{}",
            self.loaded_count(),
            self.skipped_count(),
            if self.panicked() { ", LOADER PANICKED" } else { "" }
        )
    }
    pub fn describe(&self, name: &str) -> String {
        let mut s = format!("{name}: {}\n", self.summary());
        for n in &self.properties_notes {
            s += &format!("  {n}\n");
        }
        for d in &self.dims {
            s += &format!(
                "dimension {} (folder `{}`): {} loaded, {} skipped\n",
                d.id,
                d.folder,
                d.loaded.len(),
                d.skipped.len()
            );
            if let Some(p) = &d.panicked {
                s += &format!("  loader panicked: {p}\n");
            }
            s += &format!(
                "  loaded: {}\n",
                d.loaded.iter().cloned().collect::<Vec<_>>().join(" ")
            );
            for k in &d.skipped {
                s += &format!(
                    "  SKIPPED {}{}\n",
                    k.program,
                    if k.geometry { " (geometry stage)" } else { "" }
                );
                for p in k.problems.iter().take(4) {
                    s += &format!("    {}:{}: {}\n", p.file, p.line, p.message);
                }
            }
            for n in d.notes.iter().take(20) {
                s += &format!("  note: {n}\n");
            }
        }
        if !self.unused.is_empty() {
            s += &format!(
                "not drawn by Horizon (program kinds it has no pass for, or turned off by options): {}\n",
                self.unused.iter().cloned().collect::<Vec<_>>().join(" ")
            );
        }
        s
    }
}
