//! Native grouping must preserve executable wiring and opaque source records.
use std::io::{Cursor, Write};
use unlinked_model::edit::{apply_batch, next_sid, Edit};
use unlinked_model::{BlockId, Model};

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
    out.write_all(b"unchanged opaque part").unwrap();
    out.finish().unwrap().into_inner()
}
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    let mut mdl = String::from("Model {\n Name m\n System {\n");
    let mut xml = String::from("<ModelInformation><Model Name=\"m\"><System>");
    for (id, ty, name, x, y, ports, param, value) in [
        (1, "Constant", "c", 0, 0, "[0,1]", "Value", "3"),
        (2, "Gain", "g", 100, 0, "[1,1]", "Gain", "2"),
        (3, "Outport", "out", 200, 0, "[1,0]", "Port", "1"),
        (4, "Scope", "scope", 100, 100, "[1,0]", "NumInputPorts", "1"),
    ] {
        mdl+=&format!(" Block {{\n BlockType {ty}\n Name {name}\n SID {id}\n Position [{x},{y},{},{}]\n Ports {ports}\n {param} {value}\n Object {{\n ClassName OpaqueMetadata\n Value preserved\n }}\n }}\n",x+30,y+30);
        xml+=&format!("<Block BlockType=\"{ty}\" Name=\"{name}\" SID=\"{id}\"><P Name=\"Position\">[{x},{y},{},{}]</P><P Name=\"Ports\">{ports}</P><P Name=\"{param}\">{value}</P><Unknown flag=\"preserved\"/></Block>",x+30,y+30);
    }
    mdl+=" Line {\n SrcBlock c\n SrcPort 1\n UserData preserve_line\n Points [20,0]\n Branch {\n DstBlock g\n DstPort 1\n UserData preserve_branch\n }\n Branch {\n DstBlock scope\n DstPort 1\n }\n }\n Line {\n SrcBlock g\n SrcPort 1\n DstBlock out\n DstPort 1\n }\n }\n}\n";
    xml+="<Line><P Name=\"Src\">1#out:1</P><P Name=\"UserData\">preserve_line</P><P Name=\"Points\">[20,0]</P><Branch><P Name=\"Dst\">2#in:1</P><P Name=\"UserData\">preserve_branch</P></Branch><Branch><P Name=\"Dst\">4#in:1</P></Branch></Line><Line><P Name=\"Src\">2#out:1</P><P Name=\"Dst\">3#in:1</P></Line></System></Model></ModelInformation>";
    let legacy = mdl
        .lines()
        .filter(|line| !line.trim_start().starts_with("SID "))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    vec![
        ("m.mdl", mdl.into_bytes()),
        ("legacy.mdl", legacy.into_bytes()),
        ("m.slx", zip(&xml)),
    ]
}
fn group(m: &Model) -> Edit {
    Edit::CreateSubsystem {
        system: vec![],
        ids: vec![m.root.block_by_name("g").unwrap().id.clone()],
        id: BlockId(next_sid(m).unwrap().to_string()),
        name: "controller".into(),
    }
}
#[test]
fn group_roundtrips_fanout_and_legacy_ids_with_opaque_metadata() {
    for (name, bytes) in fixtures() {
        let original = unlinked_import::import(name, &bytes).unwrap();
        let mut expected = original.clone();
        let edit = group(&original);
        apply_batch(&mut expected, std::slice::from_ref(&edit)).unwrap();
        let output = unlinked_import::patch::apply_edits(name, &bytes, &[edit]).unwrap();
        let parsed = unlinked_import::import(name, &output).unwrap();
        assert_eq!(parsed, expected, "{name}");
        if name.ends_with("mdl") {
            let text = String::from_utf8(output).unwrap();
            assert_eq!(text.matches("ClassName OpaqueMetadata").count(), 4);
            assert!(text.contains("preserve_branch"));
            assert!(text.contains("preserve_line"));
        } else {
            let mut archive = zip::ZipArchive::new(Cursor::new(output)).unwrap();
            let mut text = String::new();
            std::io::Read::read_to_string(
                &mut archive.by_name("simulink/blockdiagram.xml").unwrap(),
                &mut text,
            )
            .unwrap();
            assert_eq!(text.matches("<Unknown flag=\"preserved\"/>").count(), 4);
            assert!(text.contains("preserve_branch"));
            let mut opaque = String::new();
            std::io::Read::read_to_string(&mut archive.by_name("opaque.dat").unwrap(), &mut opaque)
                .unwrap();
            assert_eq!(opaque, "unchanged opaque part");
        }
    }
}
#[test]
fn a_grouped_block_remains_editable_in_the_same_batch() {
    for (name, bytes) in fixtures() {
        let mut model = unlinked_import::import(name, &bytes).unwrap();
        let grouped = group(&model);
        grouped.apply(&mut model).unwrap();
        let wrapper = model.root.block_by_name("controller").unwrap();
        let id = wrapper
            .subsystem
            .as_ref()
            .unwrap()
            .block_by_name("g")
            .unwrap()
            .id
            .clone();
        let modify = Edit::SetParameter {
            system: vec![wrapper.id.clone()],
            id,
            name: "Gain".into(),
            value: "7".into(),
        };
        modify.apply(&mut model).unwrap();
        let output = unlinked_import::patch::apply_edits(name, &bytes, &[grouped, modify]).unwrap();
        assert_eq!(
            unlinked_import::import(name, &output).unwrap(),
            model,
            "{name}"
        );
    }
}

#[test]
fn eligible_corpus_groups_roundtrip() {
    let Some(root) = std::env::var_os("UNLINKED_TEST_CASES") else {
        return;
    };
    fn files(path: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("mdl" | "slx")
            ) {
                out.push(path);
            }
        }
    }
    fn candidates(
        sys: &unlinked_model::System,
        path: &mut Vec<BlockId>,
        out: &mut Vec<(Vec<BlockId>, BlockId)>,
    ) {
        for block in &sys.blocks {
            out.push((path.clone(), block.id.clone()));
            if let Some(child) = &block.subsystem {
                path.push(block.id.clone());
                candidates(child, path, out);
                path.pop();
            }
        }
    }
    let mut paths = Vec::new();
    files(std::path::Path::new(&root), &mut paths);
    paths.sort();
    let mut checked = 0;
    for path in paths {
        let name = path.to_str().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let model = unlinked_import::import(name, &bytes).unwrap();
        let mut choices = Vec::new();
        candidates(&model.root, &mut vec![], &mut choices);
        let mut count = 0;
        for (system, block) in choices {
            let edit = Edit::CreateSubsystem {
                system,
                ids: vec![block],
                id: BlockId(next_sid(&model).unwrap().to_string()),
                name: "Grouped for roundtrip".into(),
            };
            let mut expected = model.clone();
            if apply_batch(&mut expected, std::slice::from_ref(&edit)).is_err() {
                continue;
            }
            let output = unlinked_import::patch::apply_edits(name, &bytes, &[edit])
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let imported = unlinked_import::import(name, &output).unwrap();
            assert_eq!(imported, expected, "{name}");
            count += 1;
            checked += 1;
            if count == 3 {
                break;
            }
        }
        eprintln!(
            "{}: {count} eligible groups exercised",
            path.file_name().unwrap().to_string_lossy()
        );
    }
    assert!(checked > 0, "corpus should contain groupable native blocks");
}

#[test]
fn document_defaults_cannot_change_generated_boundary_semantics() {
    let bytes = fixtures().remove(0).1;
    let text = String::from_utf8(bytes).unwrap().replacen(
        "Model {",
        r#"Model {
 BlockParameterDefaults {
  Block {
   BlockType Inport
   SampleTime "0.1"
   OutDataTypeStr "boolean"
  }
  Block {
   BlockType SubSystem
   TreatAsAtomicUnit on
   Variant off
   VariantControlMode expression
   VariantActivationTime "update diagram"
  }
 }
"#,
        1,
    );
    let model = unlinked_import::import("m.mdl", text.as_bytes()).unwrap();
    let edit = group(&model);
    let mut expected = model.clone();
    apply_batch(&mut expected, std::slice::from_ref(&edit)).unwrap();
    let wrapper = expected.root.block_by_name("controller").unwrap();
    assert_eq!(wrapper.parameters["TreatAsAtomicUnit"], "off");
    let input = wrapper
        .subsystem
        .as_ref()
        .unwrap()
        .blocks
        .iter()
        .find(|b| b.block_type == "Inport")
        .unwrap();
    assert_eq!(input.parameters["SampleTime"], "-1");
    assert_eq!(input.parameters["OutDataTypeStr"], "Inherit: auto");
    let output = unlinked_import::patch::apply_edits("m.mdl", text.as_bytes(), &[edit]).unwrap();
    assert_eq!(unlinked_import::import("m.mdl", &output).unwrap(), expected);
    let text = text.replacen(
        "SampleTime",
        "InitFcn \"do_side_effects\"\n   SampleTime",
        1,
    );
    let model = unlinked_import::import("m.mdl", text.as_bytes()).unwrap();
    assert!(
        unlinked_import::patch::apply_edits("m.mdl", text.as_bytes(), &[group(&model)]).is_err()
    );
}

#[test]
fn split_root_grouping_preserves_other_parts() {
    let mut source = fixtures().pop().unwrap().1;
    let mut archive = zip::ZipArchive::new(Cursor::new(&source)).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("simulink/blockdiagram.xml").unwrap(),
        &mut xml,
    )
    .unwrap();
    let start = xml.find("<System>").unwrap();
    let end = xml.rfind("</System>").unwrap() + "</System>".len();
    let system = &xml[start..end];
    let outer = format!(
        "{}<System Ref=\"system_root\"/>{}",
        &xml[..start],
        &xml[end..]
    );
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in [
        ("simulink/blockdiagram.xml", outer.as_str()),
        ("simulink/systems/system_root.xml", system),
        ("opaque.dat", "unchanged"),
    ] {
        out.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        out.write_all(content.as_bytes()).unwrap();
    }
    drop(archive);
    source = out.finish().unwrap().into_inner();
    let model = unlinked_import::import("split.slx", &source).unwrap();
    let edit = group(&model);
    let mut expected = model.clone();
    apply_batch(&mut expected, std::slice::from_ref(&edit)).unwrap();
    let output = unlinked_import::patch::apply_edits("split.slx", &source, &[edit]).unwrap();
    assert_eq!(
        unlinked_import::import("split.slx", &output).unwrap(),
        expected
    );
    let expand = Edit::ExpandSubsystem {
        system: vec![],
        id: expected
            .root
            .block_by_name("controller")
            .unwrap()
            .id
            .clone(),
    };
    apply_batch(&mut expected, std::slice::from_ref(&expand)).unwrap();
    let output = unlinked_import::patch::apply_edits("split.slx", &output, &[expand]).unwrap();
    assert_eq!(
        unlinked_import::import("split.slx", &output).unwrap(),
        expected
    );
    let mut archive = zip::ZipArchive::new(Cursor::new(output)).unwrap();
    let mut after = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("simulink/blockdiagram.xml").unwrap(),
        &mut after,
    )
    .unwrap();
    assert_eq!(after, outer);
}

#[test]
fn grouping_remaps_explicit_legacy_sid_endpoints() {
    let (name, bytes) = fixtures().remove(0);
    let text = String::from_utf8(bytes)
        .unwrap()
        .replace(" SID 2\n", " SID legacyGain\n")
        .replace(" DstBlock g\n DstPort 1\n", " Dst \"legacyGain#in:1\"\n")
        .replace(" SrcBlock g\n SrcPort 1\n", " Src \"legacyGain#out:1\"\n");
    let model = unlinked_import::import(name, text.as_bytes()).unwrap();
    for names in [vec!["g"], vec!["c", "g"]] {
        let edit = Edit::CreateSubsystem {
            system: vec![],
            ids: names
                .iter()
                .map(|n| model.root.block_by_name(n).unwrap().id.clone())
                .collect(),
            id: BlockId(next_sid(&model).unwrap().to_string()),
            name: "controller".into(),
        };
        let mut expected = model.clone();
        apply_batch(&mut expected, std::slice::from_ref(&edit)).unwrap();
        let output = unlinked_import::patch::apply_edits(name, text.as_bytes(), &[edit]).unwrap();
        assert_eq!(unlinked_import::import(name, &output).unwrap(), expected);
        assert!(!String::from_utf8(output).unwrap().contains("legacyGain#"));
    }
}

#[test]
fn old_files_use_factory_defaults_without_new_release_parameters() {
    let (name, bytes) = fixtures().remove(0);
    let model = unlinked_import::import(name, &bytes).unwrap();
    let output = unlinked_import::patch::apply_edits(name, &bytes, &[group(&model)]).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(!text.contains("VarSizeSig"));
    assert!(!text.contains("LatchByDelayingOutsideSignal"));
    assert!(!text.contains("SFBlockType"));
    assert_eq!(
        text.matches("Name\t\"controller\"").count(),
        2,
        "wrapper and child System names"
    );
}

#[test]
fn expansion_grafts_raw_branches_and_preserves_moved_block_metadata() {
    for (name, bytes) in fixtures() {
        let original = unlinked_import::import(name, &bytes).unwrap();
        for selected in [vec!["g"], vec!["c", "g"], vec!["c"], vec!["scope"]] {
            let create = Edit::CreateSubsystem {
                system: vec![],
                ids: selected
                    .iter()
                    .map(|name| original.root.block_by_name(name).unwrap().id.clone())
                    .collect(),
                id: BlockId(next_sid(&original).unwrap().to_string()),
                name: "controller".into(),
            };
            let mut expected = original.clone();
            create.apply(&mut expected).unwrap();
            let id = expected
                .root
                .block_by_name("controller")
                .unwrap()
                .id
                .clone();
            let expand = Edit::ExpandSubsystem { system: vec![], id };
            apply_batch(&mut expected, std::slice::from_ref(&expand)).unwrap();
            let output =
                unlinked_import::patch::apply_edits(name, &bytes, &[create, expand]).unwrap();
            assert_eq!(
                unlinked_import::import(name, &output).unwrap(),
                expected,
                "{name}"
            );
            if name.ends_with("mdl") {
                let text = String::from_utf8(output).unwrap();
                assert_eq!(text.matches("ClassName OpaqueMetadata").count(), 4);
                assert!(text.contains("preserve_branch"));
            } else {
                let mut archive = zip::ZipArchive::new(Cursor::new(output)).unwrap();
                let mut text = String::new();
                std::io::Read::read_to_string(
                    &mut archive.by_name("simulink/blockdiagram.xml").unwrap(),
                    &mut text,
                )
                .unwrap();
                assert_eq!(text.matches("<Unknown flag=\"preserved\"/>").count(), 4);
                assert!(text.contains("preserve_branch"));
            }
        }
    }
}

#[test]
fn expansion_refuses_conflicting_donor_and_discarded_wrapper_metadata() {
    let (name, bytes) = fixtures().remove(0);
    let model = unlinked_import::import(name, &bytes).unwrap();
    let grouped = unlinked_import::patch::apply_edits(name, &bytes, &[group(&model)]).unwrap();
    let text = String::from_utf8(grouped).unwrap();
    for edited in [
        text.replacen("preserve_line", "conflicting_metadata", 1),
        text.replacen(
            "Name\t\"controller\"",
            "Name\t\"controller\"\n UserData important_metadata",
            1,
        ),
    ] {
        assert_ne!(edited, text);
        let model = unlinked_import::import(name, edited.as_bytes()).unwrap();
        let edit = Edit::ExpandSubsystem {
            system: vec![],
            id: model.root.block_by_name("controller").unwrap().id.clone(),
        };
        assert!(unlinked_import::patch::apply_edits(name, edited.as_bytes(), &[edit]).is_err());
    }
}

#[test]
fn standalone_expansion_allocates_legacy_child_ids_and_preserves_parent_order() {
    let (name, bytes) = fixtures().remove(0);
    let original = unlinked_import::import(name, &bytes).unwrap();
    let grouped = unlinked_import::patch::apply_edits(name, &bytes, &[group(&original)]).unwrap();
    let mut text = String::from_utf8(grouped).unwrap().replace(" SID 2\n", "");
    // Append another unrelated block after the wrapper, exercising splice order.
    let at = text.rfind(" }\n}\n").unwrap();
    text.insert_str(at," Block {\n BlockType Constant\n Name tail\n SID 100\n Value 1\n Position [500,0,530,30]\n Ports [0,1]\n }\n");
    let model = unlinked_import::import(name, text.as_bytes()).unwrap();
    let edit = Edit::ExpandSubsystem {
        system: vec![],
        id: model.root.block_by_name("controller").unwrap().id.clone(),
    };
    let mut expected = model.clone();
    apply_batch(&mut expected, std::slice::from_ref(&edit)).unwrap();
    let bytes = unlinked_import::patch::apply_edits(name, text.as_bytes(), &[edit]).unwrap();
    assert_eq!(unlinked_import::import(name, &bytes).unwrap(), expected);
    assert!(
        expected
            .root
            .block_by_name("g")
            .unwrap()
            .id
            .0
            .parse::<u64>()
            .unwrap()
            > 100
    );
    assert_eq!(expected.root.blocks.last().unwrap().name, "tail");
}

#[test]
fn expansion_remaps_combined_legacy_sid_references() {
    let (name, bytes) = fixtures().remove(0);
    let original = unlinked_import::import(name, &bytes).unwrap();
    let grouped = unlinked_import::patch::apply_edits(name, &bytes, &[group(&original)]).unwrap();
    let text = String::from_utf8(grouped)
        .unwrap()
        .replace(" SID 2\n", " SID legacyGain\n")
        .replace(" DstBlock g\n DstPort 1\n", " Dst \"legacyGain#in:1\"\n")
        .replace("SrcBlock\t\"g\"\n   SrcPort\t1", "Src \"legacyGain#out:1\"");
    assert!(text.contains("legacyGain#in:1"));
    assert!(text.contains("legacyGain#out:1"));
    let model = unlinked_import::import(name, text.as_bytes()).unwrap();
    let edit = Edit::ExpandSubsystem {
        system: vec![],
        id: model.root.block_by_name("controller").unwrap().id.clone(),
    };
    let mut expected = model.clone();
    apply_batch(&mut expected, std::slice::from_ref(&edit)).unwrap();
    let output = unlinked_import::patch::apply_edits(name, text.as_bytes(), &[edit]).unwrap();
    assert_eq!(unlinked_import::import(name, &output).unwrap(), expected);
    assert!(!String::from_utf8(output).unwrap().contains("legacyGain#"));
}

#[test]
fn expansion_rejects_opaque_defaults_on_removed_interfaces() {
    let (name, bytes) = fixtures().remove(0);
    let original = unlinked_import::import(name, &bytes).unwrap();
    let grouped = unlinked_import::patch::apply_edits(name, &bytes, &[group(&original)]).unwrap();
    let text=String::from_utf8(grouped).unwrap().replacen("Model {", "Model {\n BlockParameterDefaults {\n Block {\n BlockType Inport\n UserData opaque_default\n }\n }",1);
    let model = unlinked_import::import(name, text.as_bytes()).unwrap();
    let edit = Edit::ExpandSubsystem {
        system: vec![],
        id: model.root.block_by_name("controller").unwrap().id.clone(),
    };
    let mut unchanged = model.clone();
    assert!(edit.apply(&mut unchanged).is_err());
    assert_eq!(unchanged, model);
    assert!(unlinked_import::patch::apply_edits(name, text.as_bytes(), &[edit]).is_err());
}
