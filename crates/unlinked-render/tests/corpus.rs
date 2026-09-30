//! Renders every system of every corpus model and checks the SVG is
//! well-formed with one group per block. Set `UNLINKED_RENDER_OUT` to a
//! directory to also write the SVGs for visual inspection.

use quick_xml::events::Event;
use quick_xml::Reader;
use std::path::{Path, PathBuf};
use unlinked_render::{render_system_svg, RenderOptions};

fn corpus_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("UNLINKED_TEST_CASES") {
        Some(d) => PathBuf::from(d),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../unlinked-test-cases"),
    };
    dir.is_dir().then_some(dir)
}

fn models(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            models(&p, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("slx" | "mdl")) {
            out.push(p);
        }
    }
}

/// Parse the SVG and count `<g class="block">` elements.
fn block_groups(svg: &str) -> Result<usize, String> {
    let mut reader = Reader::from_str(svg);
    let mut depth = 0i64;
    let mut blocks = 0;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => {
                depth += 1;
                if e.name().as_ref() == "g"
                    && e.attributes()
                        .flatten()
                        .any(|a| a.key.as_ref() == "class" && &*a.value == "block")
                {
                    blocks += 1;
                }
            }
            Event::End(_) => depth -= 1,
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0 {
        return Err("unbalanced elements".into());
    }
    Ok(blocks)
}

#[test]
fn corpus_renders_well_formed_svg() {
    let Some(dir) = corpus_dir() else {
        eprintln!("corpus not found; set UNLINKED_TEST_CASES to run");
        return;
    };
    let out_dir = std::env::var_os("UNLINKED_RENDER_OUT").map(PathBuf::from);
    let mut files = Vec::new();
    models(&dir, &mut files);
    files.sort();
    let mut rendered = 0;
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let model = unlinked_import::import(&f.display().to_string(), &bytes).unwrap();
        for (i, (path, sys)) in model.walk().into_iter().enumerate() {
            let svg = render_system_svg(sys, &RenderOptions::default()).unwrap();
            let groups = block_groups(&svg).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(groups, sys.blocks.len(), "{path}: block groups");
            if let Some(out) = &out_dir {
                let stem = f.file_stem().unwrap().to_string_lossy();
                std::fs::write(out.join(format!("{stem}-{i:03}.svg")), &svg).unwrap();
            }
            rendered += 1;
        }
    }
    eprintln!("rendered {rendered} systems");
}
