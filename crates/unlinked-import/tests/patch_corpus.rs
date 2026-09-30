//! Patching corpus models: re-importing a patched file must give exactly
//! the IR obtained by applying the same edits to the imported model.

use std::path::{Path, PathBuf};
use unlinked_model::edit::Edit;
use unlinked_model::{Model, Rect};

fn corpus_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("UNLINKED_TEST_CASES") {
        Some(d) => PathBuf::from(d),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../unlinked-test-cases"),
    };
    dir.is_dir().then_some(dir)
}

fn models(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            models(&p, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("slx" | "mdl")) {
            out.push(p);
        }
    }
}

/// Move, re-parameterize and rename the first root block, and delete the
/// first root block that has a line attached.
fn edits_for(model: &Model) -> Vec<Edit> {
    let mut edits = Vec::new();
    let Some(first) = model.root.blocks.first() else {
        return edits;
    };
    let p = first.position;
    edits.push(Edit::MoveBlock {
        system: vec![],
        id: first.id.clone(),
        position: Rect::new(p.left + 20.0, p.top + 10.0, p.right + 20.0, p.bottom + 10.0),
    });
    edits.push(Edit::SetParameter {
        system: vec![],
        id: first.id.clone(),
        name: "Description".into(),
        value: "edited \"here\" & <there>\nline two".into(),
    });
    edits.push(Edit::RenameBlock {
        system: vec![],
        id: first.id.clone(),
        name: format!("{} renamed", first.name),
    });
    let connected = model.root.blocks.iter().skip(1).find(|b| {
        model
            .root
            .lines
            .iter()
            .any(|l| unlinked_model::edit::touches(l, &b.id))
    });
    if let Some(b) = connected {
        edits.push(Edit::DeleteBlock {
            system: vec![],
            id: b.id.clone(),
        });
    }
    edits
}

#[test]
fn patched_corpus_models_reimport_to_the_edited_ir() {
    let Some(dir) = corpus_dir() else {
        eprintln!("corpus not found; set UNLINKED_TEST_CASES to run");
        return;
    };
    let mut files = Vec::new();
    models(&dir, &mut files);
    files.sort();
    let mut failures = Vec::new();
    for f in &files {
        let name = f.display().to_string();
        let bytes = std::fs::read(f).unwrap();
        let original = unlinked_import::import(&name, &bytes).unwrap();

        // No edits: the content is unchanged.
        let same = unlinked_import::patch::apply_edits(&name, &bytes, &[]).unwrap();
        if unlinked_import::import(&name, &same).unwrap() != original {
            failures.push(format!("{name}: empty patch changed the model"));
        }

        let edits = edits_for(&original);
        let mut expected = original.clone();
        for e in &edits {
            e.apply(&mut expected).unwrap();
        }
        match unlinked_import::patch::apply_edits(&name, &bytes, &edits) {
            Ok(patched) => match unlinked_import::import(&name, &patched) {
                Ok(actual) if actual.root == expected.root => {}
                Ok(actual) => {
                    let diff = unlinked_model::diff::diff(&expected, &actual);
                    failures.push(format!("{name}: patched model differs: {:?}", diff));
                }
                Err(e) => failures.push(format!("{name}: patched file does not import: {e}")),
            },
            Err(e) => failures.push(format!("{name}: patch failed: {e}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
