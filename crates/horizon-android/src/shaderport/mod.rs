//! Android integration of Horizon Translator's classic shader-pack converter.
//! Runs the same Horizon shader loader offline, applies conservative compatibility
//! rewrites, checks the result, and returns a detailed local report.

mod bufsize;
mod cloudsoff;
mod compute;
mod files;
mod fix;
mod fragdepth;
mod glsl;
mod gpu;
mod gpu_fix;
mod load;
mod noise2d;
mod normals;
mod pbr;
mod postkeep;
mod skyblend;
mod tex2d;
mod tex3d;
mod uniforms;

use std::path::Path;

#[derive(Debug)]
pub struct ConversionResult {
    pub before_summary: String,
    pub after_summary: String,
    pub rules_applied: usize,
    pub report: String,
}

/// Convert an extracted Iris/OptiFine shader pack into a Horizon-compatible ZIP.
/// The caller is responsible for safely extracting the source archive and for
/// installing the returned ZIP only after validating/extracting it again.
pub fn convert(
    input: &Path,
    output_zip: &Path,
    pack_name: &str,
) -> Result<ConversionResult, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        convert_inner(input, output_zip, pack_name)
    }))
    .map_err(|_| "The shader converter stopped unexpectedly".to_owned())?
}

fn convert_inner(
    input: &Path,
    output_zip: &Path,
    pack_name: &str,
) -> Result<ConversionResult, String> {
    let mut files = files::Files::read(input)?;
    files.name = pack_name.to_owned();
    let before = load::evaluate(&files)?;
    let mut log = fix::fix(&mut files, &before);

    // These rules repair renderer behaviour, so they also run on packs the loader
    // already accepts. Each rule remains guarded by the same post-conversion check.
    compute::compute_off(&mut files, &mut log);
    normals::fix(&mut files, &mut log);
    uniforms::fix(&mut files, &mut log);
    uniforms::matrix_order(&mut files, &mut log);
    pbr::fix(&mut files, &mut log);
    postkeep::fix(&mut files, &mut log);
    fragdepth::fix(&mut files, &mut log);
    tex3d::fix(&mut files, &mut log);
    noise2d::fix(&mut files, &mut log);
    tex2d::fix(&mut files, &mut log);
    bufsize::fix(&mut files, &mut log);
    skyblend::fix(&mut files, &mut log);
    cloudsoff::fix(&mut files, &mut log);

    let after = load::evaluate(&files)?;
    let report = fix::report(pack_name, &before, &after, &log);
    files.write_zip(output_zip)?;

    Ok(ConversionResult {
        before_summary: before.summary(),
        after_summary: after.summary(),
        rules_applied: log.iter().filter(|change| !change.files.is_empty()).count(),
        report,
    })
}
