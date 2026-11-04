//! Pass `sky-blend`. Vanilla draws the sky disc without blending (only the sunset fan and the
//! stars blend), so a pack's `gbuffers_skybasic` overwrites the sky there. Horizon (nightly
//! 7aa0e28) blends the whole sky pass with the translucent default, so an output with alpha 0
//! changes nothing. Mellow writes `vec4(0)` from it to clear `colortex0`, which it never clears
//! otherwise, then adds its sky in `deferred1`: in Horizon each frame adds onto the last and the
//! sky goes white.
//!
//! When every output of the sky program has a constant alpha of 0 and goes to a buffer the pack
//! never clears, `blend.<program>=off` is added to `shaders.properties`. Under blending such a
//! pass draws nothing at all, so the only difference is where vanilla also overwrites.

use horizon_shaders::{Macros, PostProgram, Properties, dimension_folder, load_post};
use naga::{Expression as E, Handle, Statement as S};

use crate::shaderport::files::Files;
use crate::shaderport::fix::Change;

/// Whether `e` is a constant whose last vector component (or the scalar) is zero.
fn alpha_zero(m: &naga::Module, exprs: &naga::Arena<E>, e: Handle<E>, global: bool) -> bool {
    let next = |h| alpha_zero(m, if global { &m.global_expressions } else { exprs }, h, global);
    match &exprs[e] {
        E::ZeroValue(_) => true,
        E::Literal(l) => match *l {
            naga::Literal::F32(v) => v == 0.0,
            naga::Literal::F64(v) => v == 0.0,
            naga::Literal::I32(v) => v == 0,
            naga::Literal::U32(v) => v == 0,
            naga::Literal::AbstractFloat(v) => v == 0.0,
            naga::Literal::AbstractInt(v) => v == 0,
            _ => false,
        },
        E::Splat { value, .. } => next(*value),
        E::Compose { components, .. } => components.last().is_some_and(|c| next(*c)),
        E::As { expr, .. } => next(*expr),
        E::Constant(c) => alpha_zero(m, &m.global_expressions, m.constants[*c].init, true),
        _ => false,
    }
}

/// The global variable an output member is loaded from in the entry point's return.
fn returned_globals(m: &naga::Module, ep: &naga::EntryPoint) -> Option<Vec<(u32, Handle<naga::GlobalVariable>)>> {
    let f = &ep.function;
    let result = f.result.as_ref()?;
    let ret = f.body.iter().find_map(|s| match s {
        S::Return { value: Some(v) } => Some(*v),
        _ => None,
    })?;
    let global_of = |e: Handle<E>| match f.expressions[e] {
        E::Load { pointer } => match f.expressions[pointer] {
            E::GlobalVariable(g) => Some(g),
            _ => None,
        },
        _ => None,
    };
    match (&m.types[result.ty].inner, &result.binding) {
        (naga::TypeInner::Struct { members, .. }, _) => {
            let E::Compose { components, .. } = &f.expressions[ret] else { return None };
            members
                .iter()
                .zip(components)
                .filter_map(|(mem, c)| match mem.binding {
                    Some(naga::Binding::Location { location, .. }) => Some(global_of(*c).map(|g| (location, g))),
                    _ => None,
                })
                .collect()
        }
        (_, Some(naga::Binding::Location { location, .. })) => Some(vec![(*location, global_of(ret)?)]),
        _ => None,
    }
}

/// Whether every store into `g`, in any function, leaves its alpha a constant 0 (and there is one).
fn stores_zero_alpha(m: &naga::Module, ep: &naga::EntryPoint, g: Handle<naga::GlobalVariable>) -> bool {
    fn walk(m: &naga::Module, f: &naga::Function, block: &naga::Block, g: Handle<naga::GlobalVariable>, n: &mut u32, ok: &mut bool) {
        for s in block.iter() {
            match s {
                S::Store { pointer, value } => match f.expressions[*pointer] {
                    E::GlobalVariable(h) if h == g => {
                        *n += 1;
                        *ok &= alpha_zero(m, &f.expressions, *value, false);
                    }
                    E::AccessIndex { base, index } if matches!(f.expressions[base], E::GlobalVariable(h) if h == g) => {
                        if index == 3 {
                            *n += 1;
                            *ok &= alpha_zero(m, &f.expressions, *value, false);
                        }
                    }
                    E::Access { base, .. } if matches!(f.expressions[base], E::GlobalVariable(h) if h == g) => *ok = false,
                    _ => {}
                },
                S::Block(b) => walk(m, f, b, g, n, ok),
                S::If { accept, reject, .. } => {
                    walk(m, f, accept, g, n, ok);
                    walk(m, f, reject, g, n, ok);
                }
                S::Loop { body, continuing, .. } => {
                    walk(m, f, body, g, n, ok);
                    walk(m, f, continuing, g, n, ok);
                }
                S::Switch { cases, .. } => cases.iter().for_each(|c| walk(m, f, &c.body, g, n, ok)),
                // A pointer to the output handed to a function: not followed.
                S::Call { arguments, .. } => {
                    if arguments.iter().any(|a| matches!(f.expressions[*a], E::GlobalVariable(h) if h == g)) {
                        *ok = false;
                    }
                }
                _ => {}
            }
        }
    }
    let (mut n, mut ok) = (0, true);
    for f in m.functions.iter().map(|(_, f)| f).chain(std::iter::once(&ep.function)) {
        walk(m, f, &f.body, g, &mut n, &mut ok);
    }
    ok && n > 0
}

/// The buffers a sky program writes with alpha 0 everywhere, or `None` if some output does not.
fn zero_alpha_buffers(p: &PostProgram) -> Option<Vec<u32>> {
    let m = &p.program.fragment;
    let ep = m.entry_points.iter().find(|e| e.stage == naga::ShaderStage::Fragment)?;
    let outs = returned_globals(m, ep)?;
    let mut bufs = Vec::new();
    for (loc, g) in outs {
        if !stores_zero_alpha(m, ep, g) {
            return None;
        }
        bufs.push(*p.buffers.get(loc as usize)?);
    }
    (!bufs.is_empty()).then_some(bufs)
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let Ok(pack) = files.pack() else { return };
    let ptext = files.text("shaders.properties").unwrap_or_default();
    let properties = Properties::parse(&ptext, &Macros::standard());
    let inputs = crate::shaderport::load::inputs();
    let mut add: Vec<(String, Vec<u32>)> = Vec::new();
    for id in [0, -1, 1] {
        let folder = dimension_folder(&pack, id);
        let Ok(post) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_post(&pack, &folder, &properties, &Macros::standard(), &inputs)
        })) else {
            continue;
        };
        let Some(sky) = &post.sky else { continue };
        if properties.get(&format!("blend.{}", sky.name)).is_some() || add.iter().any(|(n, _)| *n == sky.name) {
            continue;
        }
        let Some(bufs) = zero_alpha_buffers(sky) else { continue };
        if bufs.iter().any(|&b| post.config.buffers.get(b as usize).is_some_and(|c| !c.clear)) {
            add.push((sky.name.clone(), bufs));
        }
    }
    if add.is_empty() {
        return;
    }
    // Where the sky disc does not reach (just above the horizon, past the terrain) vanilla's
    // pixels keep the last frame too, but Java draws far terrain there; Horizon's shorter view
    // leaves them adding up to white. When no program drawn before the scene reads the buffer
    // (a history read), it is cleared every frame instead.
    let cleared = clear_buffers(files, &add);
    let mut out = ptext;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("# horizon-shaderport sky-blend: vanilla draws the sky disc unblended; this pass clears with alpha 0\n");
    for (name, _) in &add {
        out.push_str(&format!("blend.{name}=off\n"));
    }
    files.set_text("shaders.properties", out);
    let what: Vec<String> = add.iter().map(|(n, b)| format!("{n} (colortex{b:?})")).collect();
    let (bufs, changed) = cleared;
    let also = if bufs.is_empty() {
        String::new()
    } else {
        format!("; colortex{bufs:?} cleared every frame (no program before the scene reads it)")
    };
    println!("  sky-blend: blending off for {}{also}", what.join(", "));
    let mut touched = vec!["shaders.properties".to_owned()];
    touched.extend(changed);
    log.push(Change {
        rule: "sky-blend",
        files: touched,
        outcome: format!(
            "`blend.<program>=off` for {}: it writes alpha 0 into a buffer the pack never clears, which vanilla's unblended sky disc overwrites and Horizon's blended sky pass leaves alone{also}",
            what.join(", ")
        ),
    });
}

/// The names a buffer goes by.
fn names(b: u32) -> Vec<String> {
    let mut v = vec![format!("colortex{b}")];
    let alias = ["gcolor", "gdepth", "gnormal", "composite", "gaux1", "gaux2", "gaux3", "gaux4"];
    if let Some(a) = alias.get(b as usize) {
        v.push((*a).to_owned());
    }
    v
}

/// Programs drawn before deferred (their stage files and the includes named like them).
fn early(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    ["gbuffers_", "shadow", "prepare", "begin", "setup", "dh_", "clrwl_", "voxy_"].iter().any(|p| base.starts_with(p))
}

/// Turns `<buffer>Clear = false` to true for the sky's buffers no early program reads.
/// Returns (buffers, files changed).
fn clear_buffers(files: &mut Files, add: &[(String, Vec<u32>)]) -> (Vec<u32>, Vec<String>) {
    let srcs = crate::shaderport::fix::sources(files);
    let mut bufs: Vec<u32> = add.iter().flat_map(|(_, b)| b.iter().copied()).collect();
    bufs.sort_unstable();
    bufs.dedup();
    // Only with a clear colour of its own: the default for colortex0 is the fog colour, which
    // the pack would then add its sky onto.
    bufs.retain(|&b| srcs.iter().any(|(_, t)| names(b).iter().any(|n| t.contains(&format!("{n}ClearColor")))));
    bufs.retain(|&b| {
        !srcs.iter().any(|(path, text)| {
            early(path)
                && names(b).iter().any(|n| {
                    let comments = crate::shaderport::glsl::comment_ranges(text);
                    crate::shaderport::glsl::find_word(text, n).into_iter().any(|at| {
                        let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
                        let line = text[ls..].lines().next().unwrap_or("");
                        !crate::shaderport::glsl::in_comment(&comments, at) && !line.trim_start().starts_with("uniform ")
                    })
                })
        })
    });
    let mut changed = Vec::new();
    let mut done = Vec::new();
    for (path, text) in &srcs {
        let mut out = text.clone();
        for &b in &bufs {
            for n in names(b) {
                let key = format!("{n}Clear");
                let mut from = 0;
                while let Some(i) = out[from..].find(&key).map(|i| from + i) {
                    from = i + key.len();
                    let before_ok = i == 0 || !out.as_bytes()[i - 1].is_ascii_alphanumeric();
                    let rest = &out[from..];
                    let t = rest.trim_start();
                    if !before_ok || !t.starts_with('=') {
                        continue;
                    }
                    let v = t[1..].trim_start();
                    if v.starts_with("false") && !v[5..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                        let at = from + (rest.len() - v.len());
                        out.replace_range(at..at + 5, "true");
                        if !done.contains(&b) {
                            done.push(b);
                        }
                    }
                }
            }
        }
        if out != *text {
            files.set_text(path, out);
            changed.push(path.clone());
        }
    }
    done.sort_unstable();
    (done, changed)
}
