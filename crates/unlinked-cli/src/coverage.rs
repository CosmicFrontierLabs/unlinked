//! Reproducible import/render/compile inventory. Compilation is not execution
//! or a claim of numerical equivalence.
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

fn collect(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0usize;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            entries += 1;
            if entries > 100_000 {
                bail!("coverage input exceeds 100,000 directory entries");
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && entry.path().extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("slx") || ext.eq_ignore_ascii_case("mdl")
                })
            {
                paths.push(entry.path());
                if paths.len() > 10_000 {
                    bail!("coverage input exceeds 10,000 model files");
                }
            }
        }
    }
    paths.sort();
    Ok(())
}

pub fn report(root: &Path) -> Result<serde_json::Value> {
    let mut paths = Vec::new();
    collect(root, &mut paths)?;
    if paths.is_empty() {
        bail!("no .slx or .mdl files found under {}", root.display());
    }
    let options = unlinked_sim::Options::default();
    let mut imported = 0usize;
    let mut rendered = 0usize;
    let mut compiled = 0usize;
    let mut models = Vec::new();
    for path in paths {
        let name = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        let model = match super::load_model(&path) {
            Ok(model) => model,
            Err(error) => {
                models.push(serde_json::json!({"path":name,"import_error":format!("{error:#}")}));
                continue;
            }
        };
        imported += 1;
        let mut render_errors = Vec::new();
        for (system_path, system) in model.walk() {
            if let Err(error) = unlinked_render::render_system_svg(
                system,
                &unlinked_render::RenderOptions::default(),
            ) {
                render_errors
                    .push(serde_json::json!({"system":system_path,"error":error.to_string()}));
            }
        }
        if render_errors.is_empty() {
            rendered += 1;
        }
        let compile_error = match unlinked_sim::compile(&model, &options) {
            Ok(_) => {
                compiled += 1;
                None
            }
            Err(error) => Some(error.to_string()),
        };
        models.push(serde_json::json!({
            "path":name,"name":model.name,"version":model.simulink_version,
            "blocks":model.block_count(),"block_types":model.block_type_counts(),
            "systems":model.walk().len(),"render_errors":render_errors,
            "simulation_compile_error":compile_error,
        }));
    }
    Ok(serde_json::json!({
        "description":"Import, all-system SVG render and simulation compilation only. No simulation is run; compilation does not establish numerical equivalence. No workspace overrides or callbacks are evaluated.",
        "simulation_options":options,
        "summary":{"models":models.len(),"imported":imported,"all_systems_rendered":rendered,"simulation_compiled":compiled},
        "models":models,
    }))
}
