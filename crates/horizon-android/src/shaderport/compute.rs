//! Rule `compute-to-fragment`. Horizon runs no compute programs (`*.csh`), so whatever a pack
//! computes there is never written (Noble works out its sun and sky light in `prepare.csh`: without
//! it the world is black). A small compute program that writes one `colorimgN` with `imageStore`
//! becomes the fragment program of the same name, writing buffer N:
//!
//! - its code (preprocessed, includes resolved) keeps its `main` as `hz_cs_main`;
//! - each pixel of a corner as large as the work (`B`×`B`) runs every invocation in turn, and keeps
//!   the `imageStore` aimed at itself; other pixels are discarded, so the rest of the buffer stays;
//! - everything runs twice and only the second run stores: `shared` variables become plain globals
//!   filled by the first run, standing in for `barrier()` (enough for the usual "one invocation
//!   prepares, the others read" use).
//!
//! Only when the fragment program's slot is free, the work is at most 256 invocations, one image is
//! written, and nothing is read back with `imageLoad`.

use crate::shaderport::files::Files;
use crate::shaderport::fix::Edits;
use crate::shaderport::glsl::{find_word, matching, split_args};
use crate::shaderport::load::Eval;
use horizon_shaders::{Macros, preprocess};

fn ints(s: &str) -> Option<Vec<u32>> {
    s.split(',').map(|v| v.trim().trim_end_matches('u').parse().ok()).collect()
}

/// `layout (local_size_x = 10, local_size_y = 1) in;` → [10, 1, 1] and the statement's range.
fn local_size(code: &str) -> Option<([u32; 3], std::ops::Range<usize>)> {
    for at in find_word(code, "local_size_x") {
        let start = code[..at].rfind("layout")?;
        let end = start + code[start..].find(';')? + 1;
        let mut size = [1u32; 3];
        for (i, k) in ["local_size_x", "local_size_y", "local_size_z"].iter().enumerate() {
            if let Some(p) = code[start..end].find(k) {
                let rest = &code[start + p + k.len()..end];
                let rest = rest.trim_start().strip_prefix('=')?.trim_start();
                let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                size[i] = n.parse().ok()?;
            }
        }
        return Some((size, start..end));
    }
    None
}

fn convert(code: &str) -> Result<(String, u32), String> {
    let mut code = code.to_owned();
    // The work: `const ivec3 workGroups = ivec3(a, b, c);`
    let wg = find_word(&code, "workGroups")
        .into_iter()
        .find_map(|at| {
            let rest = &code[at..];
            let open = rest.find("ivec3(")? + 5;
            let close = matching(rest, open)?;
            ints(&rest[open + 1..close])
        })
        .ok_or("no `const ivec3 workGroups` (workGroupsRender is not supported)")?;
    if wg.len() != 3 {
        return Err("workGroups is not three numbers".into());
    }
    let (ls, range) = local_size(&code).ok_or("no local_size layout")?;
    code.replace_range(range, "");
    let groups = wg[0] * wg[1] * wg[2];
    let locals = ls[0] * ls[1] * ls[2];
    if groups * locals > 256 {
        return Err(format!("{} invocations (at most 256)", groups * locals));
    }
    if !find_word(&code, "imageLoad").is_empty() {
        return Err("reads an image back (imageLoad)".into());
    }
    // Image declarations: `layout (...) uniform image2D colorimgN;`
    let mut buffer = None;
    for at in find_word(&code, "image2D").into_iter().rev() {
        let line_start = code[..at].rfind(|c| c == ';' || c == '}' || c == '\n').map_or(0, |i| i + 1);
        let Some(semi) = code[at..].find(';').map(|i| at + i) else { continue };
        if !code[line_start..at].contains("uniform") {
            continue;
        }
        let name = code[at + 7..semi].trim().to_owned();
        let n: u32 = name.strip_prefix("colorimg").and_then(|n| n.parse().ok()).ok_or(format!("writes `{name}` (only colorimgN)"))?;
        if buffer.is_some_and(|b| b != n) {
            return Err("writes more than one image".into());
        }
        buffer = Some(n);
        code.replace_range(line_start..semi + 1, "");
    }
    let buffer = buffer.ok_or("writes no colorimg")?;
    // imageStore(img, coord, value) → hz_store(coord, value)
    for at in find_word(&code, "imageStore").into_iter().rev() {
        let open = at + 10;
        if code.as_bytes().get(open) != Some(&b'(') {
            continue;
        }
        let close = matching(&code, open).ok_or("unbalanced imageStore")?;
        let args = split_args(&code, open, close);
        if args.len() != 3 {
            return Err("imageStore with other than 3 arguments".into());
        }
        code.replace_range(at..close + 1, &format!("hz_store(ivec2({}), vec4({}))", args[1], args[2]));
    }
    // main → hz_cs_main
    let main_at = find_word(&code, "main")
        .into_iter()
        .find(|&at| code[..at].trim_end().ends_with("void") && code[at + 4..].trim_start().starts_with('('))
        .ok_or("no main")?;
    code.replace_range(main_at..main_at + 4, "hz_cs_main");
    for (word, with) in [
        ("shared", ""),
        ("gl_LocalInvocationIndex", "hz_lindex"),
        ("gl_LocalInvocationID", "hz_lid"),
        ("gl_WorkGroupID", "hz_wid"),
        ("gl_GlobalInvocationID", "(hz_wid * gl_WorkGroupSize + hz_lid)"),
        ("gl_NumWorkGroups", &format!("uvec3({}u, {}u, {}u)", wg[0], wg[1], wg[2])),
        ("gl_WorkGroupSize", &format!("uvec3({}u, {}u, {}u)", ls[0], ls[1], ls[2])),
    ] {
        for at in find_word(&code, word).into_iter().rev() {
            code.replace_range(at..at + word.len(), with);
        }
    }
    for word in ["barrier", "memoryBarrierShared", "memoryBarrier", "groupMemoryBarrier", "memoryBarrierImage", "memoryBarrierBuffer"] {
        for at in find_word(&code, word).into_iter().rev() {
            let open = at + word.len();
            if code[open..].starts_with("()") {
                code.replace_range(at..open + 2, "");
            }
        }
    }
    let b = (wg[0] * ls[0]).max(wg[1] * ls[1]).max(groups * locals).max(1);
    let head = "uvec3 hz_lid; uvec3 hz_wid; uint hz_lindex; ivec2 hz_px; bool hz_write; bool hz_hit; vec4 hz_val;\n\
                void hz_store(ivec2 c, vec4 v) { if (hz_write && c == hz_px) { hz_val = v; hz_hit = true; } }\n";
    let tail = format!(
        "\n/* RENDERTARGETS: {buffer} */\nlayout(location = 0) out vec4 hz_out0;\n\
         void main() {{\n    hz_px = ivec2(gl_FragCoord.xy);\n    if (hz_px.x >= {b} || hz_px.y >= {b}) discard;\n    hz_hit = false; hz_val = vec4(0.0);\n    \
         for (int hz_run = 0; hz_run < 2; hz_run++) {{\n        hz_write = hz_run == 1;\n        \
         for (uint g = 0u; g < {groups}u; g++) {{\n            hz_wid = uvec3(g % {gx}u, (g / {gx}u) % {gy}u, g / ({gx}u * {gy}u));\n            \
         for (uint l = 0u; l < {locals}u; l++) {{\n                hz_lid = uvec3(l % {lx}u, (l / {lx}u) % {ly}u, l / ({lx}u * {ly}u));\n                hz_lindex = l;\n                hz_cs_main();\n            }}\n        }}\n    }}\n    \
         if (!hz_hit) discard;\n    hz_out0 = hz_val;\n}}\n",
        gx = wg[0], gy = wg[1], lx = ls[0], ly = ls[1]
    );
    let at = crate::shaderport::glsl::after_version(&code).unwrap_or(0);
    Ok((format!("{}{head}{}{tail}", &code[..at], &code[at..]), buffer))
}

const VERTEX: &str = "#version 430 compatibility\n// horizon-shaderport compute-to-fragment: full-screen pass\nvoid main() { gl_Position = ftransform(); }\n";

pub fn compute_to_fragment(files: &Files, _: &Eval) -> Edits {
    let Ok(pack) = files.pack() else { return Edits::new() };
    let macros = Macros::standard();
    let mut edits = Edits::new();
    let paths: Vec<String> = files
        .entries
        .keys()
        .filter_map(|k| k.strip_prefix("shaders/"))
        .filter(|p| p.ends_with(".csh"))
        .map(str::to_owned)
        .collect();
    for path in paths {
        let stem = path.trim_end_matches(".csh");
        let name = stem.rsplit('/').next().unwrap_or(stem);
        let stage_ok = ["prepare", "deferred", "composite"].iter().any(|s| {
            name.strip_prefix(s).is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
        });
        if !stage_ok || files.text(&format!("{stem}.fsh")).is_some() {
            continue;
        }
        let code = preprocess(&pack, &path, &macros).code;
        match convert(&code) {
            Ok((fsh, buffer)) => {
                edits.insert(format!("{stem}.fsh"), (fsh, format!("from {path}: writes colortex{buffer}")));
                if files.text(&format!("{stem}.vsh")).is_none() {
                    edits.insert(format!("{stem}.vsh"), (VERTEX.into(), "full-screen pass".into()));
                }
            }
            Err(why) => {
                if std::env::var_os("HZ_TRACE").is_some() {
                    eprintln!("  [trace] compute-to-fragment {path}: {why}");
                }
            }
        }
    }
    edits
}

/// Pass `compute-off`: a compute program Horizon cannot run (not converted above) that the pack
/// turns on with one option (`program.world0/deferred4_a.enabled = SH_SKYLIGHT`): that option is
/// turned off, so the pack takes the path it has without it (Photon's sky light without its
/// spherical-harmonics pass; without this, everything in shade is black). A FEATURE IS LOST.
pub fn compute_off(files: &mut Files, log: &mut Vec<crate::shaderport::fix::Change>) {
    let Some(props) = files.text("shaders.properties") else { return };
    let mut names = std::collections::BTreeSet::new();
    for line in props.lines() {
        let Some(rest) = line.trim().strip_prefix("program.") else { continue };
        let Some((key, value)) = rest.split_once('=') else { continue };
        let Some(path) = key.trim().strip_suffix(".enabled") else { continue };
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(crate::shaderport::glsl::is_ident_byte) || value.as_bytes()[0].is_ascii_digit() {
            continue;
        }
        if files.text(&format!("{path}.csh")).is_some() && files.text(&format!("{path}.fsh")).is_none() {
            names.insert(value.to_owned());
        }
    }
    if names.is_empty() {
        return;
    }
    let mut edits = Edits::new();
    for (path, text) in crate::shaderport::fix::sources(files) {
        if crate::shaderport::fix::is_stage(&path) {
            continue;
        }
        let mut n = 0;
        let lines: Vec<String> = text
            .split('\n')
            .map(|l| {
                let t = l.trim_start();
                let Some(rest) = t.strip_prefix('#').and_then(|d| d.trim_start().strip_prefix("define")) else { return l.to_owned() };
                let mut parts = rest.split_whitespace();
                let Some(name) = parts.next() else { return l.to_owned() };
                let value = parts.next();
                if names.contains(name) && (matches!(value, None | Some("1")) || value.is_some_and(|v| v.starts_with("//"))) {
                    n += 1;
                    format!("// horizon-shaderport: {name} turned off (its compute program cannot run in Horizon): {}", t)
                } else {
                    l.to_owned()
                }
            })
            .collect();
        if n > 0 {
            edits.insert(path, (lines.join("\n"), String::new()));
        }
    }
    if edits.is_empty() {
        return;
    }
    let Ok(best) = crate::shaderport::load::evaluate(files) else { return };
    let kept = crate::shaderport::pbr::keep(files, &best, edits, 0);
    let list: Vec<String> = names.into_iter().collect();
    println!("  compute-off: {} turned off in {} file(s)", list.join(", "), kept.len());
    log.push(crate::shaderport::fix::Change {
        rule: "compute-off",
        files: kept.into_iter().collect(),
        outcome: format!("turned off `{}`: its compute program cannot run in Horizon (A FEATURE IS LOST)", list.join("`, `")),
    });
}
