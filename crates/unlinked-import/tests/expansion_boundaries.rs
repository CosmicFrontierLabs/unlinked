//! Regression fixtures from independent hierarchy review.
use std::io::{Cursor, Write};
use unlinked_model::edit::{apply_batch, Edit};
fn blk(ty: &str, name: &str, sid: Option<u32>, pos: [i32; 4], ports: &str, extra: &str) -> String {
    let sid = sid.map(|s| format!("   SID {s}\n")).unwrap_or_default();
    format!("  Block {{\n   BlockType {ty}\n   Name \"{name}\"\n{sid}   Position [{}, {}, {}, {}]\n   Ports {ports}\n{extra}  }}\n", pos[0], pos[1], pos[2], pos[3])
}
fn mdl(wrapper: &str, inner_sum: &str, root_out2: &str, sids: bool) -> String {
    let s = |n| if sids { Some(n) } else { None };
    let mut t = String::from("Model {\n Name m\n System {\n  Name m\n");
    t += &blk(
        "Constant",
        "c",
        s(1),
        [0, 0, 30, 30],
        "[0, 1]",
        "   Value 1\n",
    );
    t += &blk("SubSystem", wrapper, s(2), [200, 0, 260, 60], "[2, 2]", "");
    // child system
    let mut c = String::from("   System {\n");
    c += &blk("Inport", "In1", s(10), [0, 0, 30, 14], "[0, 1]", "");
    c += &blk(
        "Inport",
        "In2",
        s(11),
        [0, 50, 30, 64],
        "[0, 1]",
        "   Port \"2\"\n",
    );
    c += &blk(
        "Gain",
        "g1",
        s(12),
        [100, 0, 130, 30],
        "[1, 1]",
        "   Gain 2\n",
    );
    c += &blk(
        "Gain",
        "g2",
        s(13),
        [100, 100, 130, 130],
        "[1, 1]",
        "   Gain 3\n",
    );
    c += &blk(
        "Sum",
        inner_sum,
        s(14),
        [200, 0, 230, 30],
        "[2, 1]",
        "   Inputs \"++\"\n",
    );
    c += &blk("Outport", "Out1", s(15), [300, 0, 330, 14], "[1, 0]", "");
    c += &blk(
        "Outport",
        "Out2",
        s(16),
        [300, 100, 330, 114],
        "[1, 0]",
        "   Port \"2\"\n",
    );
    c += "  Line {\n SrcBlock In1\n SrcPort 1\n Points [10, 0]\n Branch {\n DstBlock g1\n DstPort 1\n }\n Branch {\n Points [0, 50]\n DstBlock g2\n DstPort 1\n }\n }\n";
    c += &format!(
        "  Line {{\n SrcBlock In2\n SrcPort 1\n DstBlock \"{inner_sum}\"\n DstPort 2\n }}\n"
    );
    c += &format!("  Line {{\n Name \"inner_named\"\n SrcBlock g1\n SrcPort 1\n DstBlock \"{inner_sum}\"\n DstPort 1\n }}\n");
    c += &format!("  Line {{\n SrcBlock \"{inner_sum}\"\n SrcPort 1\n Branch {{\n DstBlock Out1\n DstPort 1\n }}\n Branch {{\n DstBlock Out2\n DstPort 1\n }}\n }}\n");

    c += "   }\n";
    // put child system into wrapper block
    t = t.replacen("   Ports [2, 2]\n", &format!("   Ports [2, 2]\n{c}"), 1);
    t += &blk(
        "Scope",
        "scope",
        s(3),
        [100, 200, 130, 230],
        "[1]",
        "   NumInputPorts \"1\"\n",
    );
    t += &blk("Terminator", "t1", s(4), [400, 0, 420, 20], "[1]", "");
    t += &blk("Terminator", "t2", s(5), [400, 50, 420, 70], "[1]", "");
    t += &blk(
        "Outport",
        root_out2,
        s(6),
        [400, 100, 430, 114],
        "[1, 0]",
        "",
    );
    t += &format!(" Line {{\n SrcBlock c\n SrcPort 1\n Points [50, 0]\n Branch {{\n DstBlock \"{wrapper}\"\n DstPort 1\n }}\n Branch {{\n Points [0, 30]\n Branch {{\n DstBlock \"{wrapper}\"\n DstPort 2\n }}\n Branch {{\n DstBlock scope\n DstPort 1\n }}\n }}\n }}\n");
    t += &format!(" Line {{\n SrcBlock \"{wrapper}\"\n SrcPort 1\n Points [30, 0]\n Branch {{\n DstBlock t1\n DstPort 1\n }}\n Branch {{\n DstBlock \"{root_out2}\"\n DstPort 1\n }}\n }}\n");
    t += &format!(" Line {{\n SrcBlock \"{wrapper}\"\n SrcPort 2\n DstBlock t2\n DstPort 1\n }}\n");
    t += " }\n}\n";
    t
}
fn zip(xml: &str) -> Vec<u8> {
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    out.start_file(
        "simulink/blockdiagram.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    out.write_all(xml.as_bytes()).unwrap();
    out.finish().unwrap().into_inner()
}
fn xb(ty: &str, name: &str, sid: u32, pos: [i32; 4], ports: &str, extra: &str) -> String {
    format!("<Block BlockType=\"{ty}\" Name=\"{name}\" SID=\"{sid}\"><P Name=\"Position\">[{}, {}, {}, {}]</P><P Name=\"Ports\">{ports}</P>{extra}</Block>", pos[0], pos[1], pos[2], pos[3])
}
fn slx(zorder: bool, child_props: &str) -> Vec<u8> {
    let z = |n: u32| {
        if zorder {
            format!("<P Name=\"ZOrder\">{n}</P>")
        } else {
            String::new()
        }
    };
    let mut c = format!("<System>{child_props}");
    c += &xb("Inport", "In1", 10, [0, 0, 30, 14], "[0, 1]", "");
    c += &xb(
        "Gain",
        "g1",
        12,
        [100, 0, 130, 30],
        "[1, 1]",
        "<P Name=\"Gain\">2</P>",
    );
    c += &xb("Outport", "Out1", 15, [300, 0, 330, 14], "[1, 0]", "");
    c += &format!(
        "<Line>{}<P Name=\"Src\">10#out:1</P><P Name=\"Dst\">12#in:1</P></Line>",
        z(1)
    );
    c += &format!(
        "<Line>{}<P Name=\"Src\">12#out:1</P><P Name=\"Dst\">15#in:1</P></Line>",
        z(2)
    );
    c += "</System>";
    let mut x = String::from("<ModelInformation><Model Name=\"m\"><System>");
    x += &xb(
        "Constant",
        "c",
        1,
        [0, 0, 30, 30],
        "[0, 1]",
        "<P Name=\"Value\">1</P>",
    );
    x += &xb("SubSystem", "S", 2, [200, 0, 260, 60], "[1, 1]", &c);
    x += &xb("Terminator", "t1", 4, [400, 0, 420, 20], "[1]", "");
    x += &format!(
        "<Line>{}<P Name=\"Src\">1#out:1</P><P Name=\"Dst\">2#in:1</P></Line>",
        z(5)
    );
    x += &format!(
        "<Line>{}<P Name=\"Src\">2#out:1</P><P Name=\"Dst\">4#in:1</P></Line>",
        z(6)
    );
    x += "</System></Model></ModelInformation>";
    zip(&x)
}

fn roundtrip(name: &str, bytes: &[u8]) {
    let mut expected = unlinked_import::import(name, bytes).unwrap();
    let id = expected
        .root
        .blocks
        .iter()
        .find(|b| b.block_type == "SubSystem")
        .unwrap()
        .id
        .clone();
    let edit = Edit::ExpandSubsystem { system: vec![], id };
    apply_batch(&mut expected, std::slice::from_ref(&edit)).unwrap();
    let output = unlinked_import::patch::apply_edits(name, bytes, &[edit]).unwrap();
    assert_eq!(
        unlinked_import::import(name, &output).unwrap(),
        expected,
        "{name}"
    );
}
#[test]
fn graft_targets_are_resolved_before_cross_scope_names_are_inserted() {
    for sids in [true, false] {
        let text = mdl("S", "sum", "Out2", sids).replace(
            "   System {\n",
            "   System {\n Name S\n Location [0,0,100,100]\n ZoomFactor 100\n Open off\n",
        );
        roundtrip("scope.mdl", text.as_bytes());
    }
}
#[test]
fn cosmetic_line_order_and_child_view_state_do_not_block_expansion() {
    roundtrip(
        "zorder.slx",
        &slx(
            true,
            "<P Name=\"Location\">[0,0,100,100]</P><P Name=\"ZoomFactor\">100</P>",
        ),
    );
}

#[test]
fn ordinary_factory_parameters_do_not_prevent_expansion() {
    let text = mdl("S", "sum", "Out2", true)
        .replacen("   Ports [2, 2]\n", "   Ports [2, 2]\n   Permissions ReadWrite\n   RequestExecContextInheritance off\n   RTWSystemCode Auto\n   Variant off\n   VariantControlMode expression\n   VariantActivationTime \"update diagram\"\n", 1)
        .replace("   BlockType Inport\n", "   BlockType Inport\n   IconDisplay \"Port number\"\n   Interpolate on\n   LockScale off\n")
        .replace("   BlockType Outport\n", "   BlockType Outport\n   InitialOutput 0\n   SourceOfInitialOutputValue Dialog\n   OutputWhenDisabled held\n   MustResolveToSignalObject off\n");
    roundtrip("defaults.mdl", text.as_bytes());
}
