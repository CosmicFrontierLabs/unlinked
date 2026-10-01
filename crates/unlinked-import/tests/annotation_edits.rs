//! Annotation edits preserve the surrounding model and unmodeled metadata.
use std::io::{Cursor, Write};
use unlinked_model::edit::{apply_batch, next_sid, AnnotationTarget, Edit};
use unlinked_model::{Model, Rect};

fn zip(xml: &str) -> Vec<u8> {
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    out.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    out.write_all(xml.as_bytes()).unwrap();
    out.start_file("opaque.dat", zip::write::SimpleFileOptions::default())
        .unwrap();
    out.write_all(b"preserve unknown bytes").unwrap();
    out.finish().unwrap().into_inner()
}
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.mdl", b"Model {\r\n Name m\r\n System {\r\n Annotation {\r\n SID 10\r\n Name \"plain\"\r\n Position [1, 2]\r\n FontSize 13\r\n }\r\n Annotation {\r\n SID 11\r\n Name \"<html>rich</html>\"\r\n Position [3, 4, 30, 40]\r\n Interpreter rich\r\n DropShadow on\r\n }\r\n }\r\n}\r\n".to_vec()),
        ("a.slx", zip("<ModelInformation><Model Name=\"m\"><System><Annotation SID=\"10\"><P Name=\"Name\"><![CDATA[plain]]></P><P Name=\"Position\">[1,2]</P><P Name=\"FontSize\">13</P></Annotation><Annotation SID=\"11\"><P Name=\"Name\">&lt;html&gt;rich&lt;/html&gt;</P><P Name=\"Position\">[3,4,30,40]</P><P Name=\"Interpreter\">rich</P><P Name=\"DropShadow\">on</P><Unknown flag=\"preserve\"/></Annotation></System></Model></ModelInformation>")),
    ]
}
fn target(m: &Model, index: usize) -> AnnotationTarget {
    AnnotationTarget {
        index,
        expected: m.root.annotations[index].clone(),
    }
}
fn roundtrip(name: &str, bytes: &[u8], edits: &[Edit]) -> Vec<u8> {
    let mut expected = unlinked_import::import(name, bytes).unwrap();
    apply_batch(&mut expected, edits).unwrap();
    let out = unlinked_import::patch::apply_edits(name, bytes, edits).unwrap();
    assert_eq!(
        unlinked_import::import(name, &out).unwrap(),
        expected,
        "{name}"
    );
    out
}
#[test]
fn annotation_operations_roundtrip_sequential_targets_and_metadata() {
    for (name, bytes) in fixtures() {
        let mut model = unlinked_import::import(name, &bytes).unwrap();
        let mut edits = Vec::new();
        let mut push = |edit: Edit| {
            edit.apply(&mut model).unwrap();
            edits.push(edit);
        };
        let initial = unlinked_import::import(name, &bytes).unwrap();
        push(Edit::SetAnnotationText {
            system: vec![],
            target: target(&initial, 0),
            text: "quoted \"text\" & <markup>\nsecond line".into(),
        });
        let current = unlinked_import::import(name, &roundtrip(name, &bytes, &edits)).unwrap();
        let movement = Edit::MoveAnnotation {
            system: vec![],
            target: target(&current, 1),
            position: Rect::new(10., 20., 50., 60.),
        };
        movement.apply(&mut model).unwrap();
        edits.push(movement);
        let addition = Edit::AddAnnotation {
            system: vec![],
            id: next_sid(&model).unwrap().to_string(),
            text: "new plain annotation".into(),
            position: Rect::new(50., 60., 50., 60.),
        };
        addition.apply(&mut model).unwrap();
        edits.push(addition);
        let removal = Edit::DeleteAnnotation {
            system: vec![],
            target: target(&model, 0),
        };
        removal.apply(&mut model).unwrap();
        edits.push(removal);
        let movement = Edit::MoveAnnotation {
            system: vec![],
            target: target(&model, 1),
            position: Rect::new(70., 80., 90., 100.),
        };
        edits.push(movement);
        let out = roundtrip(name, &bytes, &edits);
        if name.ends_with("mdl") {
            let text = String::from_utf8(out).unwrap();
            assert!(text.contains("DropShadow on\r\n"));
            assert!(!text.replace("\r\n", "").contains('\n'));
        } else {
            let mut archive = zip::ZipArchive::new(Cursor::new(out)).unwrap();
            let mut raw = String::new();
            std::io::Read::read_to_string(
                &mut archive.by_name("simulink/blockdiagram.xml").unwrap(),
                &mut raw,
            )
            .unwrap();
            assert!(raw.contains("<Unknown flag=\"preserve\"/>"));
            let mut opaque = String::new();
            std::io::Read::read_to_string(&mut archive.by_name("opaque.dat").unwrap(), &mut opaque)
                .unwrap();
            assert_eq!(opaque, "preserve unknown bytes");
        }
    }
}
#[test]
fn rich_text_and_stale_targets_fail_without_writing() {
    for (name, bytes) in fixtures() {
        let model = unlinked_import::import(name, &bytes).unwrap();
        let rich = Edit::SetAnnotationText {
            system: vec![],
            target: target(&model, 1),
            text: "replace".into(),
        };
        assert!(unlinked_import::patch::apply_edits(name, &bytes, &[rich]).is_err());
        let remove = Edit::DeleteAnnotation {
            system: vec![],
            target: target(&model, 0),
        };
        let stale = Edit::DeleteAnnotation {
            system: vec![],
            target: target(&model, 1),
        };
        assert!(unlinked_import::patch::apply_edits(name, &bytes, &[remove, stale]).is_err());
    }
}

#[test]
fn annotations_roundtrip_across_corpus() {
    let Some(dir) = std::env::var_os("UNLINKED_TEST_CASES") else {
        return;
    };
    let mut pending = vec![std::path::PathBuf::from(dir)];
    let mut paths = Vec::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(std::fs::read_dir(path).unwrap().map(|e| e.unwrap().path()));
        } else if matches!(
            path.extension().and_then(|x| x.to_str()),
            Some("mdl" | "slx")
        ) {
            paths.push(path);
        }
    }
    let mut moved = 0;
    for path in paths {
        let bytes = std::fs::read(&path).unwrap();
        let name = path.to_str().unwrap();
        let model = unlinked_import::import(name, &bytes).unwrap();
        let add = Edit::AddAnnotation {
            system: vec![],
            id: next_sid(&model).unwrap().to_string(),
            text: "Corpus annotation: \"quoted\" & <plain>\nsecond line".into(),
            position: Rect::new(20., 20., 100., 60.),
        };
        roundtrip(name, &bytes, &[add]);
        let mut systems = vec![(vec![], &model.root)];
        while let Some((system, sys)) = systems.pop() {
            for (index, annotation) in sys.annotations.iter().enumerate() {
                let p = annotation.position;
                let target = AnnotationTarget {
                    index,
                    expected: annotation.clone(),
                };
                let edit = Edit::MoveAnnotation {
                    system: system.clone(),
                    target: target.clone(),
                    position: Rect::new(p.left + 10., p.top + 10., p.right + 10., p.bottom + 10.),
                };
                roundtrip(name, &bytes, &[edit]);
                roundtrip(
                    name,
                    &bytes,
                    &[Edit::DeleteAnnotation {
                        system: system.clone(),
                        target: target.clone(),
                    }],
                );
                if !annotation.rich_text {
                    roundtrip(
                        name,
                        &bytes,
                        &[Edit::SetAnnotationText {
                            system: system.clone(),
                            target,
                            text: "Updated plain annotation".into(),
                        }],
                    );
                }
                moved += 1;
            }
            for block in &sys.blocks {
                if let Some(subsystem) = block.subsystem.as_deref() {
                    let mut child = system.clone();
                    child.push(block.id.clone());
                    systems.push((child, subsystem));
                }
            }
        }
    }
    assert!(
        moved > 0,
        "the configured corpus should contain annotations"
    );
}

#[test]
fn slx_text_precedence_and_body_fallback_are_preserved() {
    for annotation in [
        "<Annotation SID=\"1\"><P Name=\"Name\">shadowed</P><P Name=\"Text\"><![CDATA[effective]]></P><P Name=\"Position\">[1,2,99]</P></Annotation>",
        "<Annotation SID=\"1\" Text=\"effective\" Position=\"[1,2]\"><P Name=\"Name\">shadowed</P></Annotation>",
        "<Annotation SID=\"1\"><![CDATA[effective]]></Annotation>",
    ] {
        let bytes = zip(&format!("<ModelInformation><Model Name=\"m\"><System>{annotation}</System></Model></ModelInformation>"));
        let mut model = unlinked_import::import("a.slx", &bytes).unwrap();
        let text = Edit::SetAnnotationText { system: vec![], target: target(&model, 0), text: "new & <plain> text".into() };
        text.apply(&mut model).unwrap();
        let moved = Edit::MoveAnnotation { system: vec![], target: target(&model, 0), position: Rect::new(3., 4., 8., 9.) };
        roundtrip("a.slx", &bytes, &[text, moved]);
    }
}
