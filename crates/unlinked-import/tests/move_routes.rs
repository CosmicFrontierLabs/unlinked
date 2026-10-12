//! Moving a block keeps the stored routes of its lines: the patched file,
//! re-imported, must give the IR the edit produces.

use std::io::{Cursor, Read, Write};
use unlinked_import::{import, patch::apply_edits};
use unlinked_model::edit::{apply_batch, Edit};
use unlinked_model::{Model, Rect};

/// `b1` feeds `b2` straight across and `b3` below it through a branched
/// line; `extra` adds a second line from `b1`, which makes its source
/// ambiguous.
fn fixtures(extra: bool) -> Vec<(&'static str, Vec<u8>)> {
    let blocks = [("1", 0, 0), ("2", 100, 0), ("3", 100, 100), ("4", 200, 0)];
    let mut mdl = String::from("Model {\n Name m\n System {\n Name m\n");
    let mut xml = String::from("<ModelInformation><Model Name=\"m\"><System>");
    for (id, x, y) in blocks {
        let (r, b) = (x + 30, y + 30);
        mdl += &format!(" Block {{\n BlockType Gain\n Name b{id}\n SID {id}\n Ports [1, 1]\n Position [{x}, {y}, {r}, {b}]\n }}\n");
        xml += &format!("<Block BlockType=\"Gain\" Name=\"b{id}\" SID=\"{id}\"><P Name=\"Ports\">[1,1]</P><P Name=\"Position\">[{x},{y},{r},{b}]</P></Block>");
    }
    mdl += " Line {\n SrcBlock b1\n SrcPort 1\n Points [20, 0]\n Branch {\n DstBlock b2\n DstPort 1\n }\n Branch {\n Points [0, 100]\n DstBlock b3\n DstPort 1\n }\n }\n";
    xml += "<Line><P Name=\"Src\">1#out:1</P><P Name=\"Points\">[20, 0]</P><Branch><P Name=\"Dst\">2#in:1</P></Branch><Branch><P Name=\"Points\">[0, 100]</P><P Name=\"Dst\">3#in:1</P></Branch></Line>";
    if extra {
        mdl += " Line {\n SrcBlock b1\n SrcPort 1\n Points [10, 0; 0, -40; 150, 0; 0, 40]\n DstBlock b4\n DstPort 1\n }\n";
        xml += "<Line><P Name=\"Src\">1#out:1</P><P Name=\"Points\">[10, 0; 0, -40; 150, 0; 0, 40]</P><P Name=\"Dst\">4#in:1</P></Line>";
    }
    mdl += " }\n}\n";
    xml += "</System></Model></ModelInformation>";
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    vec![
        ("m.mdl", mdl.into_bytes()),
        ("m.slx", zip.finish().unwrap().into_inner()),
    ]
}

fn shift(model: &Model, id: &str, dx: f64, dy: f64) -> Edit {
    let p = model.root.block(&id.into()).unwrap().position;
    Edit::MoveBlock {
        system: vec![],
        id: id.into(),
        position: Rect::new(p.left + dx, p.top + dy, p.right + dx, p.bottom + dy),
    }
}

/// Apply `edit` to the file and to its IR; the two must agree. Returns the
/// edited IR and the text holding the lines.
fn moved(name: &str, bytes: &[u8], edit: Edit) -> (Model, String) {
    let mut expected = import(name, bytes).unwrap();
    apply_batch(&mut expected, std::slice::from_ref(&edit)).unwrap();
    let out = apply_edits(name, bytes, &[edit]).unwrap();
    assert_eq!(import(name, &out).unwrap(), expected, "{name}");
    let text = if name.ends_with(".slx") {
        let mut zip = zip::ZipArchive::new(Cursor::new(out)).unwrap();
        let mut s = String::new();
        zip.by_name("simulink/blockdiagram.xml")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        s
    } else {
        String::from_utf8(out).unwrap()
    };
    (expected, text)
}

/// The `Points` values in `text`, in order.
fn points(text: &str) -> Vec<&str> {
    text.split("Points")
        .skip(1)
        .map(|s| {
            let s = &s[s.find('[').unwrap()..];
            &s[..=s.find(']').unwrap()]
        })
        .collect()
}

#[test]
fn moving_the_source_rewrites_relative_points() {
    for (name, bytes) in fixtures(false) {
        let original = import(name, &bytes).unwrap();
        let (model, text) = moved(name, &bytes, shift(&original, "1", 10.0, 20.0));
        // The trunk vertex follows the port down and keeps its x, so the
        // branch to b3 starts 20 lower.
        assert_eq!(points(&text), ["[10, 0]", "[0, 80]"], "{name}");
        let (before, after) = (&original.root.lines[0], &model.root.lines[0]);
        assert_eq!(after.points[0].x, before.points[0].x);
        assert_eq!(after.points[0].y, before.points[0].y + 20.0);
        assert_eq!(after.branches[1].points, before.branches[1].points);
    }
}

#[test]
fn moving_a_branch_end_realigns_only_its_leg() {
    for (name, bytes) in fixtures(false) {
        let original = import(name, &bytes).unwrap();
        let (_, text) = moved(name, &bytes, shift(&original, "3", 0.0, 10.0));
        assert_eq!(points(&text), ["[20, 0]", "[0, 110]"], "{name}");
        // b2's branch has no vertices; the renderer elbows into it.
        let (model, text) = moved(name, &bytes, shift(&original, "2", 30.0, 40.0));
        assert_eq!(points(&text), ["[20, 0]", "[0, 100]"], "{name}");
        assert_eq!(model.root.lines, original.root.lines);
    }
}

#[test]
fn ambiguous_sources_lose_their_points_in_ir_and_file() {
    for (name, bytes) in fixtures(true) {
        let original = import(name, &bytes).unwrap();
        let (model, text) = moved(name, &bytes, shift(&original, "1", 0.0, 10.0));
        assert!(points(&text).is_empty(), "{name}: {text}");
        assert!(model.root.lines.iter().all(|l| l.points.is_empty()));
        // Lines not attached to the moved block keep theirs.
        let (_, text) = moved(name, &bytes, shift(&original, "3", 0.0, 10.0));
        assert_eq!(
            points(&text),
            ["[10, 0; 0, -40; 150, 0; 0, 40]"],
            "{name}: {text}"
        );
    }
}

#[test]
fn moving_the_plant_keeps_the_feedback_route() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/mass_spring_damper_pid.mdl"
    );
    let bytes = std::fs::read(path).unwrap();
    let original = import("m.mdl", &bytes).unwrap();
    let feedback = |m: &Model| {
        let line = m
            .root
            .lines
            .iter()
            .find(|l| l.src.as_ref().is_some_and(|s| s.block.0 == "8"))
            .unwrap();
        line.branches
            .iter()
            .find(|b| b.dst.as_ref().is_some_and(|d| d.block.0 == "2"))
            .unwrap()
            .points
            .clone()
    };
    for (dx, dy, trunk, branch) in [
        (10.0, 0.0, "[30, 0]", "[0, 90; -440, 0; 0, -80]"),
        (0.0, 20.0, "[40, 0]", "[0, 70; -440, 0; 0, -80]"),
    ] {
        let (model, text) = moved("m.mdl", &bytes, shift(&original, "8", dx, dy));
        assert_eq!(feedback(&model), feedback(&original));
        assert_eq!(model.root.lines.len(), original.root.lines.len());
        for (after, before) in model.root.lines.iter().zip(&original.root.lines) {
            assert_eq!(after.points.len(), before.points.len());
        }
        let points = points(&text);
        assert!(points.contains(&trunk), "{points:?}");
        assert!(points.contains(&branch), "{points:?}");
        assert!(points.contains(&"[20, 0]") && points.contains(&"[0, -70]"));
    }
}
