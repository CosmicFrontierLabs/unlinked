//! Patching corpus models: re-importing a patched file must give exactly
//! the IR obtained by applying the same edits to the imported model.

use std::path::{Path, PathBuf};
use unlinked_model::edit::{apply_batch, next_sid, touches, DisconnectPolicy, Edit};
use unlinked_model::geometry::{flipped, rotated};
use unlinked_model::{BlockId, Branch, Endpoint, Line, Model, PortKind, PortRef, Rect};

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

/// Whether a root-level block contains (or is) a Stateflow chart; those
/// cannot be renamed or deleted.
fn owns_chart(model: &Model, name: &str) -> bool {
    model.charts.iter().any(|c| {
        unlinked_model::stateflow::split_path(&c.name)
            .first()
            .map(String::as_str)
            == Some(name)
    })
}

/// Move, re-parameterize and rename the first root block without charts,
/// and delete the next chart-free root block that has a line attached.
fn edits_for(model: &Model) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut free = model
        .root
        .blocks
        .iter()
        .filter(|b| !owns_chart(model, &b.name));
    let Some(first) = free.next() else {
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
    let connected = free.find(|b| model.root.lines.iter().any(|l| touches(l, &b.id)));
    if let Some(b) = connected {
        edits.push(Edit::DeleteBlock {
            system: vec![],
            id: b.id.clone(),
            disconnect: DisconnectPolicy::Disconnect,
        });
    }
    // Inside a chart-free subsystem, renamed earlier in the same batch:
    // the nested edit must still find it by ID.
    let sub = model.root.blocks.iter().find(|b| {
        !owns_chart(model, &b.name)
            && b.subsystem.as_ref().is_some_and(|s| !s.blocks.is_empty())
            && !edits.iter().any(|e| e.block() == Some(&b.id))
    });
    if let Some(sub) = sub {
        let inner = &sub.subsystem.as_ref().unwrap().blocks[0];
        edits.push(Edit::RenameBlock {
            system: vec![],
            id: sub.id.clone(),
            name: format!("{} (edited)", sub.name),
        });
        edits.push(Edit::SetParameter {
            system: vec![sub.id.clone()],
            id: inner.id.clone(),
            name: "Description".into(),
            value: "nested edit".into(),
        });
    }
    structural_edits(model, &mut edits);
    edits
}

/// In the root: feed an existing signal into a new gain, connect that to a
/// new scope, and remove a connection from another line.
fn structural_edits(model: &Model, edits: &mut Vec<Edit>) {
    let Some(sid) = next_sid(model) else {
        return;
    };
    let deleted: Vec<BlockId> = edits
        .iter()
        .filter_map(|e| match e {
            Edit::DeleteBlock { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut lines = model.root.lines.iter().filter(|l| {
        l.src.as_ref().is_some_and(|s| s.port.kind == PortKind::Out)
            && !deleted.iter().any(|id| touches(l, id))
    });
    let feed = lines.next().and_then(|l| l.src.clone());
    let cut = lines
        .find_map(first_destination)
        .filter(|d| d.port.kind == PortKind::In);

    let right = model
        .root
        .blocks
        .iter()
        .map(|b| b.position.right)
        .fold(0.0, f64::max);
    let ids = [sid.to_string(), (sid + 1).to_string()];
    let port = |id: &str, kind| Endpoint {
        block: id.into(),
        port: PortRef { kind, index: 1 },
    };
    for (i, (id, block_type)) in ids.iter().zip(["Gain", "Scope"]).enumerate() {
        let left = right + 100.0 + 80.0 * i as f64;
        edits.push(Edit::AddBlock {
            system: vec![],
            id: id.as_str().into(),
            block_type: block_type.into(),
            name: format!("Added {block_type}"),
            position: Rect::new(left, 20.0, left + 30.0, 50.0),
        });
    }
    if let Some(src) = feed {
        edits.push(Edit::Connect {
            system: vec![],
            src,
            dst: port(&ids[0], PortKind::In),
        });
    }
    edits.push(Edit::Connect {
        system: vec![],
        src: port(&ids[0], PortKind::Out),
        dst: port(&ids[1], PortKind::In),
    });
    edits.push(Edit::SetRoute {
        system: vec![],
        dst: port(&ids[1], PortKind::In),
        points: vec![unlinked_model::Point::new(right + 150.0, 65.0)],
    });
    edits.push(Edit::SetTrunkRoute {
        system: vec![],
        src: port(&ids[0], PortKind::Out),
        points: vec![unlinked_model::Point::new(right + 155.0, 75.0)],
    });
    if let Some(dst) = cut {
        edits.push(Edit::Disconnect {
            system: vec![],
            dst,
        });
    }
    // Turn and flip a block the batch has not touched.
    let untouched = model
        .root
        .blocks
        .iter()
        .find(|b| !edits.iter().any(|e| e.block() == Some(&b.id)) && !deleted.contains(&b.id));
    if let Some(b) = untouched {
        let (o, m) = rotated(b.orientation, b.mirrored);
        let (orientation, mirrored) = flipped(o, m);
        edits.push(Edit::SetOrientation {
            system: vec![],
            id: b.id.clone(),
            orientation,
            mirrored,
        });
    }
}

fn first_destination(line: &Line) -> Option<Endpoint> {
    fn branches(bs: &[Branch]) -> Option<Endpoint> {
        bs.iter()
            .find_map(|b| b.dst.clone().or_else(|| branches(&b.branches)))
    }
    line.dst.clone().or_else(|| branches(&line.branches))
}

/// MDL type defaults (`BlockParameterDefaults`) give re-imported new blocks
/// parameters they were not created with; the created ones must survive.
fn adopt_defaults(expected: &mut Model, actual: &Model, edits: &[Edit]) {
    for edit in edits {
        let Edit::AddBlock { id, .. } = edit else {
            continue;
        };
        let (Some(want), Some(got)) = (
            expected.root.blocks.iter_mut().find(|b| &b.id == id),
            actual.root.block(id),
        ) else {
            continue;
        };
        if want
            .parameters
            .iter()
            .all(|(k, v)| got.parameters.get(k) == Some(v))
        {
            want.parameters = got.parameters.clone();
        }
    }
}

/// What differs between two versions of a model file: zip entry names for
/// SLX (entries added, removed or with different content), or `"file"` for
/// MDL text.
fn changed_content(old: &[u8], new: &[u8]) -> Vec<String> {
    if !old.starts_with(b"PK\x03\x04") {
        return if old == new {
            vec![]
        } else {
            vec!["file".into()]
        };
    }
    let entries = |b: &[u8]| {
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(b.to_vec())).unwrap();
        (0..a.len())
            .map(|i| {
                let mut f = a.by_index(i).unwrap();
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut f, &mut data).unwrap();
                (f.name().to_string(), data)
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let (a, b) = (entries(old), entries(new));
    a.keys()
        .chain(b.keys())
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
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
    let mut exercised = std::collections::BTreeMap::new();
    for f in &files {
        let name = f.display().to_string();
        let bytes = std::fs::read(f).unwrap();
        let original = unlinked_import::import(&name, &bytes).unwrap();

        // No edits: every byte of content is unchanged.
        let same = unlinked_import::patch::apply_edits(&name, &bytes, &[]).unwrap();
        if let Some(changed) = changed_content(&bytes, &same).into_iter().next() {
            failures.push(format!("{name}: empty patch changed {changed}"));
        }

        // Renaming a chart owner would orphan its chart records.
        if let Some(owner) = original
            .root
            .blocks
            .iter()
            .find(|b| owns_chart(&original, &b.name))
        {
            let rename = Edit::RenameBlock {
                system: vec![],
                id: owner.id.clone(),
                name: format!("{} renamed", owner.name),
            };
            if unlinked_import::patch::apply_edits(&name, &bytes, &[rename]).is_ok() {
                failures.push(format!("{name}: renamed chart owner {:?}", owner.name));
            }
        }

        let edits = edits_for(&original);
        let mut expected = original.clone();
        for edit in &edits {
            *exercised
                .entry(match edit {
                    Edit::Connect { src, .. } if original.root.block(&src.block).is_some() => {
                        "connect into an existing line"
                    }
                    Edit::Disconnect { .. } => "disconnect",
                    _ => "other",
                })
                .or_insert(0) += 1;
        }
        if let Err(e) = apply_batch(&mut expected, &edits) {
            failures.push(format!("{name}: {e} ({:?})", edits[e.index]));
            continue;
        }

        // Deleting a connected block without consent to disconnect fails
        // and writes nothing.
        if let Some(Edit::DeleteBlock { system, id, .. }) =
            edits.iter().find(|e| matches!(e, Edit::DeleteBlock { .. }))
        {
            let reject = Edit::DeleteBlock {
                system: system.clone(),
                id: id.clone(),
                disconnect: DisconnectPolicy::Reject,
            };
            if unlinked_import::patch::apply_edits(&name, &bytes, &[reject]).is_ok() {
                failures.push(format!(
                    "{name}: rejected delete of connected block succeeded"
                ));
            }
        }
        match unlinked_import::patch::apply_edits(&name, &bytes, &edits) {
            Ok(patched) => match unlinked_import::import(&name, &patched) {
                // Edits touch only diagram parts; everything else is raw.
                _ if changed_content(&bytes, &patched).iter().any(|p| {
                    !p.starts_with("simulink/blockdiagram.xml")
                        && !p.starts_with("simulink/systems/")
                        && p != "file"
                }) =>
                {
                    failures.push(format!(
                        "{name}: edit changed non-diagram content {:?}",
                        changed_content(&bytes, &patched)
                    ));
                }
                Ok(actual) => {
                    let mut expected = expected.clone();
                    adopt_defaults(&mut expected, &actual, &edits);
                    if actual.root != expected.root {
                        let diff = unlinked_model::diff::diff(&expected, &actual);
                        failures.push(format!("{name}: patched model differs: {:?}", diff));
                    }
                }
                Err(e) => failures.push(format!("{name}: patched file does not import: {e}")),
            },
            Err(e) => failures.push(format!("{name}: patch failed: {e}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    for kind in ["connect into an existing line", "disconnect"] {
        assert!(exercised.get(kind).copied().unwrap_or(0) > 0, "no {kind}");
    }
    eprintln!("structural edits exercised: {exercised:?}");
}
