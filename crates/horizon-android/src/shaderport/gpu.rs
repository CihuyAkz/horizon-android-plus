//! What the game does after the loader and the offline load does not: write each stage as
//! SPIR-V (wgpu's Vulkan path) and build the render pipeline. Three failures seen in game
//! (nightly 7aa0e28) pass the loader:
//!
//! - naga's SPIR-V writer panics ("Expression [N] is not cached") on a call whose `out`/`inout`
//!   argument is part of a variable (`f(v.z)`, `f(s.field)`).
//! - a fragment output with fewer components than its buffer's format (`vec3` into RGBA):
//!   wgpu refuses the pipeline (`ColorState(n, IncompatibleFormat)`).
//! - a gbuffers or shadow program with an output location past its `RENDERTARGETS`: Horizon
//!   gives the pipeline an empty target there, its geometry passes have no attachment for it, and
//!   wgpu's error is fatal (Horizon crashes; seen with Photon's `gbuffers_terrain`).
//! - a `sampler3D` the program uses (`WrongTextureViewDimension { dim: D3 }`): the game binds
//!   every pack texture as 2D (`pack_post.rs` `texture_entry`, nightly 7aa0e28) and loads no
//!   custom texture files. Rule `stand-in-3d` replaces them.

use horizon_shaders::{Config, Format, Group, Kind, PostProgram};
use naga::{Expression as E, Statement as S};

/// The reasons the game would skip this program, empty when it would draw.
pub fn check(p: &PostProgram, config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for (stage, m) in [("vertex", &p.program.vertex), ("fragment", &p.program.fragment)] {
        if let Some(why) = spirv(m) {
            out.push(format!("GPU compile ({stage}): {why}"));
        }
    }
    let unbuffered = unbuffered_outputs(p);
    if !unbuffered.is_empty() {
        out.push(format!(
            "GPU draw: outputs without a buffer: {} (it writes {} buffer(s)); the game's geometry pass then crashes Horizon (\"Incompatible color attachments\")",
            unbuffered.iter().map(u32::to_string).collect::<Vec<_>>().join(","),
            p.buffers.len()
        ));
    }
    let t3 = textures_3d(p);
    if !t3.is_empty() {
        out.push(format!(
            "GPU compile: samples 3D texture(s) {}; the game binds pack textures as 2D (wgpu: WrongTextureViewDimension)",
            t3.join(", ")
        ));
    }
    for (loc, have, need) in narrow_outputs(p, config) {
        out.push(format!(
            "GPU compile: fragment output {loc} has {have} component(s), its buffer needs {need} (wgpu: IncompatibleFormat)"
        ));
    }
    out
}

/// 3D textures a stage's entry point uses (declared but unused ones the game leaves alone).
pub fn textures_3d(p: &PostProgram) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for m in [&p.program.vertex, &p.program.fragment] {
        let Ok(info) = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(m)
        else {
            continue;
        };
        for (h, g) in m.global_variables.iter() {
            let naga::TypeInner::Image { dim: naga::ImageDimension::D3, .. } = m.types[g.ty].inner else { continue };
            if (0..m.entry_points.len()).any(|i| !info.get_entry_point(i)[h].is_empty()) {
                names.insert(g.name.clone().unwrap_or_else(|| "?".into()));
            }
        }
    }
    names.into_iter().collect()
}

fn spirv(m: &naga::Module) -> Option<String> {
    let info = match naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(m)
    {
        Ok(i) => i,
        Err(e) => return Some(format!("naga validation failed: {e}")),
    };
    // WebAssembly can't catch a panic (it stops the whole program), so the in-browser build
    // checks for the known cause first and reports it without calling the writer.
    #[cfg(target_family = "wasm")]
    {
        let calls = part_of_variable_args(m);
        if !calls.is_empty() {
            return Some(format!(
                "naga's SPIR-V writer panicked (\"is not cached\"): out/inout argument is part of a variable in call(s) to {}",
                calls.join(", ")
            ));
        }
    }
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        naga::back::spv::write_vec(m, &info, &naga::back::spv::Options::default(), None)
    }));
    match r {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(format!("SPIR-V writer error: {e}")),
        Err(_) => {
            let calls = part_of_variable_args(m);
            Some(if calls.is_empty() {
                "naga's SPIR-V writer panicked".into()
            } else {
                format!(
                    "naga's SPIR-V writer panicked (\"is not cached\"): out/inout argument is part of a variable in call(s) to {}",
                    calls.join(", ")
                )
            })
        }
    }
}

/// Functions called with an argument that is an `Access`/`AccessIndex` chain into a variable:
/// a pointer to part of a variable, which naga's SPIR-V writer cannot pass.
fn part_of_variable_args(m: &naga::Module) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    let funs = m.functions.iter().map(|(_, f)| f).chain(m.entry_points.iter().map(|e| &e.function));
    for f in funs {
        let deep = |h: naga::Handle<naga::Expression>| {
            let mut r = &f.expressions[h];
            let mut chained = false;
            while let E::Access { base, .. } | E::AccessIndex { base, .. } = r {
                chained = true;
                r = &f.expressions[*base];
            }
            chained && matches!(r, E::LocalVariable(_) | E::GlobalVariable(_))
        };
        fn walk(b: &naga::Block, found: &mut dyn FnMut(&[naga::Handle<naga::Expression>], naga::Handle<naga::Function>)) {
            for s in b.iter() {
                match s {
                    S::Block(b) => walk(b, found),
                    S::If { accept, reject, .. } => {
                        walk(accept, found);
                        walk(reject, found);
                    }
                    S::Loop { body, continuing, .. } => {
                        walk(body, found);
                        walk(continuing, found);
                    }
                    S::Switch { cases, .. } => cases.iter().for_each(|c| walk(&c.body, found)),
                    S::Call { function, arguments, .. } => found(arguments, *function),
                    _ => {}
                }
            }
        }
        walk(&f.body, &mut |args, callee| {
            if args.iter().any(|a| deep(*a)) {
                names.insert(m.functions[callee].name.clone().unwrap_or_else(|| "?".into()));
            }
        });
    }
    names.into_iter().collect()
}

/// Components the wgpu format Horizon picks for `f` has (`pack_post.rs` `texture_format`).
fn components(f: Format) -> u32 {
    match f.channels {
        1 => 1,
        2 => 2,
        _ if f.kind == Kind::Float && f.bits == 11 => 3, // Rg11b10Ufloat
        _ => 4,
    }
}

/// Fragment outputs narrower than their buffer: (location, components, needed).
fn narrow_outputs(p: &PostProgram, config: &Config) -> Vec<(u32, u32, u32)> {
    let m = &p.program.fragment;
    let Some(ep) = m.entry_points.iter().find(|e| e.stage == naga::ShaderStage::Fragment) else {
        return Vec::new();
    };
    let Some(result) = &ep.function.result else {
        return Vec::new();
    };
    let width = |ty: naga::Handle<naga::Type>| match m.types[ty].inner {
        naga::TypeInner::Scalar(_) => 1,
        naga::TypeInner::Vector { size, .. } => size as u32,
        _ => 4,
    };
    let mut outs = Vec::new();
    match (&m.types[result.ty].inner, &result.binding) {
        (naga::TypeInner::Struct { members, .. }, _) => {
            for mem in members {
                if let Some(naga::Binding::Location { location, .. }) = mem.binding {
                    outs.push((location, width(mem.ty)));
                }
            }
        }
        (_, Some(naga::Binding::Location { location, .. })) => outs.push((*location, width(result.ty))),
        _ => {}
    }
    let mut bad = Vec::new();
    for (loc, have) in outs {
        let need = if p.group == Group::Final {
            4 // the window
        } else {
            let Some(&buf) = p.buffers.get(loc as usize) else { continue };
            let buf = buf as usize;
            let format = if p.group.shadow() {
                match config.shadow.colors.get(buf) {
                    Some(b) => b.format,
                    None => continue,
                }
            } else {
                match config.buffers.get(buf) {
                    Some(b) => b.format,
                    None => continue,
                }
            };
            components(format)
        };
        if have < need {
            bad.push((loc, have, need));
        }
    }
    bad
}

/// Output locations of a geometry-pass program that no buffer takes (crash, see above).
pub fn unbuffered_outputs(p: &PostProgram) -> Vec<u32> {
    if !matches!(p.group, Group::Gbuffers | Group::Translucent | Group::Shadow) {
        return Vec::new();
    }
    p.program
        .interface
        .outputs
        .iter()
        .copied()
        .filter(|&l| l as usize >= p.buffers.len())
        .collect()
}
