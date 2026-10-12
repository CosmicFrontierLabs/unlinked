//! Adding blocks: the library palette beside the diagram and the quick
//! insert search that opens on the canvas, both over the native catalog.

use crate::diagram::{pref, set_pref};
use crate::icons;
use std::collections::HashMap;
use std::rc::Rc;
use unlinked_model::catalog::{self, BlockDescriptor, PortResolution};
use unlinked_model::edit::Edit;
use unlinked_model::{BlockId, Model, PortKind, Rect, SourceFormat};
use unlinked_render::{render_system_svg, RenderOptions, Theme};
use web_sys::{DragEvent, HtmlInputElement, KeyboardEvent};
use yew::prelude::*;

/// MIME type of a palette block being dragged onto the diagram.
pub const PALETTE_DRAG: &str = "application/x-unlinked-block";

/// Matches the quick insert list shows at most.
const MAX_MATCHES: usize = 12;

/// Creatable catalog blocks matching `query`, best first: label prefix, then
/// a word of the label starting with it, then the label, block type or
/// category containing it. An empty query lists every block in palette
/// order.
pub fn search(query: &str) -> Vec<&'static BlockDescriptor> {
    let q = query.trim().to_lowercase();
    let mut ranked: Vec<(u8, usize, &'static BlockDescriptor)> = catalog::BLOCKS
        .iter()
        .filter(|b| b.creatable)
        .enumerate()
        .filter_map(|(i, b)| {
            let label = b.label.to_lowercase();
            let rank = if q.is_empty() || label.starts_with(&q) {
                0
            } else if label.split([' ', '-']).any(|w| w.starts_with(&q)) {
                1
            } else if label.contains(&q) || b.type_key.to_lowercase().contains(&q) {
                2
            } else if b.category.to_lowercase().starts_with(&q) {
                3
            } else {
                return None;
            };
            Some((rank, i, b))
        })
        .collect();
    ranked.sort_by_key(|(rank, i, _)| (*rank, *i));
    ranked.into_iter().map(|(.., b)| b).collect()
}

/// Blocks remembered as recently used, at most.
const RECENT: usize = 8;

/// The blocks added most recently, newest first, as kept between visits.
pub fn recent_blocks() -> Vec<&'static str> {
    pref("recent")
        .unwrap_or_default()
        .split(',')
        .filter_map(|key| catalog::find(key).filter(|b| b.creatable))
        .map(|b| b.type_key)
        .take(RECENT)
        .collect()
}

/// Remember that a block of `type_key` was just added; the new list.
pub fn remember_block(type_key: &'static str) -> Vec<&'static str> {
    let mut recent = recent_blocks();
    recent.retain(|k| *k != type_key);
    recent.insert(0, type_key);
    recent.truncate(RECENT);
    set_pref("recent", &recent.join(","));
    recent
}

/// The diagram of one block of `type_key` alone, without its name, as an
/// SVG for a palette tile.
fn thumbnail(type_key: &str, theme: Theme) -> Option<String> {
    let descriptor = catalog::find(type_key)?;
    let mut model = Model {
        name: String::new(),
        source: SourceFormat::Slx,
        simulink_version: None,
        config: Default::default(),
        root: Default::default(),
        workspace: Default::default(),
        charts: Vec::new(),
        type_defaults: Default::default(),
    };
    let [w, h] = descriptor.default_size;
    Edit::AddBlock {
        system: Vec::new(),
        id: BlockId("1".into()),
        block_type: type_key.into(),
        name: descriptor.label.into(),
        position: Rect::new(0.0, 0.0, w, h),
    }
    .apply(&mut model)
    .ok()?;
    for b in &mut model.root.blocks {
        b.name.clear();
    }
    let opts = RenderOptions {
        theme,
        margin: 4.0,
        ..Default::default()
    };
    render_system_svg(&model.root, &opts).ok()
}

/// Thumbnails of every creatable block, rendered once, dark like the panels
/// they show in whatever the canvas theme.
#[hook]
fn use_thumbnails() -> Rc<HashMap<&'static str, String>> {
    use_memo((), |_| {
        catalog::BLOCKS
            .iter()
            .filter(|b| b.creatable)
            .filter_map(|b| Some((b.type_key, thumbnail(b.type_key, Theme::Dark)?)))
            .collect()
    })
}

fn tile_image(thumbs: &HashMap<&'static str, String>, type_key: &str) -> Html {
    match thumbs.get(type_key) {
        Some(svg) => html! {
            <span class="thumb">{ Html::from_html_unchecked(AttrValue::from(svg.clone())) }</span>
        },
        None => html! { <span class="thumb" /> },
    }
}

#[derive(Properties, PartialEq)]
pub struct PaletteProps {
    /// Adds a block of the given type where the diagram is shown.
    pub on_add: Callback<&'static str>,
    /// Blocks added recently, newest first, shown before the categories.
    pub recent: Rc<Vec<&'static str>>,
}

/// Native blocks by category, filtered by a search box. Blocks are dragged
/// onto the diagram, or double-clicked to add one in view.
#[function_component(BlockPalette)]
pub fn block_palette(props: &PaletteProps) -> Html {
    let query = use_state(String::new);
    let thumbs = use_thumbnails();
    let tile = |b: &'static BlockDescriptor| {
        let type_key = b.type_key;
        let ondragstart = Callback::from(move |e: DragEvent| {
            if let Some(d) = e.data_transfer() {
                let _ = d.set_data(PALETTE_DRAG, type_key);
                d.set_effect_allowed("copy");
            }
        });
        let ondblclick = props.on_add.reform(move |_: MouseEvent| type_key);
        html! {
            <li class="palette-block" draggable="true" {ondragstart} {ondblclick}
                title={format!("{} ({}): drag onto the diagram, or double-click to add", b.label, b.type_key)}>
                { tile_image(&thumbs, type_key) }
                <span class="palette-label">{ b.label }</span>
            </li>
        }
    };
    let oninput = {
        let query = query.clone();
        Callback::from(move |e: InputEvent| {
            query.set(e.target_unchecked_into::<HtmlInputElement>().value())
        })
    };
    let body = if query.trim().is_empty() {
        let recent = (!props.recent.is_empty()).then(|| {
            html! {
                <details class="palette-group" open=true>
                    <summary>{ "Recently used" }</summary>
                    <ul>{ for props.recent.iter().filter_map(|k| catalog::find(k)).map(tile) }</ul>
                </details>
            }
        });
        let categories = catalog::PALETTE_CATEGORIES
            .iter()
            .map(|category| {
                html! {
                    <details class="palette-group" open=true>
                        <summary>{ *category }</summary>
                        <ul>{ for catalog::blocks_in_category(category).map(tile) }</ul>
                    </details>
                }
            })
            .collect::<Html>();
        html! { <>{ for recent }{ categories }</> }
    } else {
        let found = search(&query);
        if found.is_empty() {
            html! { <p class="muted">{ "No block matches." }</p> }
        } else {
            html! { <ul>{ for found.into_iter().map(tile) }</ul> }
        }
    };
    html! {
        <div class="palette">
            <div class="palette-search">
                { icons::search() }
                <input type="search" placeholder="Search blocks" value={(*query).clone()} {oninput} />
            </div>
            { body }
        </div>
    }
}

/// What the quick insert adds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pick {
    Block(&'static str),
    Annotation,
}

#[derive(Properties, PartialEq)]
pub struct QuickInsertProps {
    /// Position inside the diagram container, in pixels.
    pub x: f64,
    pub y: f64,
    pub on_pick: Callback<Pick>,
    pub on_close: Callback<()>,
    /// Blocks added recently, offered first before anything is typed.
    pub recent: Rc<Vec<&'static str>>,
    /// Ending a dropped connection: the kind of port a pick must have.
    #[prop_or_default]
    pub needs: Option<PortKind>,
}

/// What the quick insert offers for `query`: the best matching blocks, then
/// an annotation when nothing is typed yet or the query names one.
/// Whether a new block of type `type_key`, with its default parameters,
/// has a port of `kind` (numbered 1).
pub(crate) fn has_port(type_key: &str, kind: PortKind) -> bool {
    let Some(descriptor) = catalog::find(type_key) else {
        return false;
    };
    match descriptor.resolve_ports(&descriptor.creation_parameters()) {
        PortResolution::Known(counts) => match kind {
            PortKind::In => counts.inputs > 0,
            PortKind::Out => counts.outputs > 0,
            _ => false,
        },
        _ => false,
    }
}

/// What the quick insert offers for `query`; ending a dropped connection,
/// only blocks with a port of kind `needs`.
fn quick_picks(query: &str, recent: &[&'static str], needs: Option<PortKind>) -> Vec<Pick> {
    let q = query.trim().to_lowercase();
    let annotation = needs.is_none()
        && ["annotation", "text", "note", "comment"]
            .iter()
            .any(|word| word.starts_with(&q));
    // Before anything is typed, the recently used come first.
    let first: &[&'static str] = if q.is_empty() { recent } else { &[] };
    let found = search(query).into_iter().map(|b| b.type_key);
    let mut keys: Vec<&'static str> = first.to_vec();
    keys.extend(found.filter(|k| !first.contains(k)));
    keys.into_iter()
        .filter(|k| needs.is_none_or(|kind| has_port(k, kind)))
        .take(MAX_MATCHES)
        .map(Pick::Block)
        .chain(annotation.then_some(Pick::Annotation))
        .collect()
}

/// Type to find a block, as in Simulink's quick insert: arrows choose,
/// Enter adds, Escape closes. An annotation can be added from it too.
#[function_component(QuickInsert)]
pub fn quick_insert(props: &QuickInsertProps) -> Html {
    let query = use_state(String::new);
    let chosen = use_state(|| 0usize);
    let input = use_node_ref();
    let list = use_node_ref();
    let thumbs = use_thumbnails();
    {
        let input = input.clone();
        use_effect_with((), move |_| {
            if let Some(el) = input.cast::<HtmlInputElement>() {
                let _ = el.focus();
            }
        });
    }
    let picks = quick_picks(&query, &props.recent, props.needs);
    let current = (*chosen).min(picks.len().saturating_sub(1));
    // Keep the chosen row in view as the arrows move through a long list.
    {
        let list = list.clone();
        use_effect_with((current, (*query).clone()), move |_| {
            let row = list
                .cast::<web_sys::Element>()
                .and_then(|l| l.query_selector(".quick-row.chosen").ok().flatten());
            if let Some(row) = row {
                row.scroll_into_view_with_bool(false);
            }
        });
    }
    let oninput = {
        let (query, chosen) = (query.clone(), chosen.clone());
        Callback::from(move |e: InputEvent| {
            query.set(e.target_unchecked_into::<HtmlInputElement>().value());
            chosen.set(0);
        })
    };
    let onkeydown = {
        let (chosen, picks) = (chosen.clone(), picks.clone());
        let (on_pick, on_close) = (props.on_pick.clone(), props.on_close.clone());
        Callback::from(move |e: KeyboardEvent| {
            // Keys typed here are not diagram shortcuts.
            e.stop_propagation();
            let n = picks.len();
            match e.key().as_str() {
                "ArrowDown" if n > 0 => chosen.set((current + 1) % n),
                "ArrowUp" if n > 0 => chosen.set((current + n - 1) % n),
                "Enter" => {
                    if let Some(pick) = picks.get(current) {
                        on_pick.emit(*pick);
                    }
                }
                "Escape" => on_close.emit(()),
                _ => return,
            }
            e.prevent_default();
        })
    };
    let stop = Callback::from(|e: MouseEvent| e.stop_propagation());
    let rows = picks.iter().enumerate().map(|(i, pick)| {
        let onclick = {
            let (on_pick, pick) = (props.on_pick.clone(), *pick);
            Callback::from(move |e: MouseEvent| {
                e.stop_propagation();
                on_pick.emit(pick)
            })
        };
        let onmouseenter = {
            let chosen = chosen.clone();
            Callback::from(move |_: MouseEvent| chosen.set(i))
        };
        let (image, label, detail) = match pick {
            Pick::Block(key) => {
                let b = catalog::find(key).expect("searched from the catalog");
                (tile_image(&thumbs, key), b.label, b.category)
            }
            Pick::Annotation => (
                html! { <span class="thumb">{ icons::annotation() }</span> },
                "Annotation",
                "Text on the diagram",
            ),
        };
        html! {
            <li id={format!("quick-option-{i}")} role="option" aria-selected={(i == current).to_string()}
                class={classes!("quick-row", (i == current).then_some("chosen"))} {onclick} {onmouseenter}>
                { image }
                <span class="quick-label">{ label }</span>
                <span class="quick-detail">{ detail }</span>
            </li>
        }
    });
    html! {
        <div class="quick-insert" style={format!("left: {}px; top: {}px;", props.x, props.y)}
            onmousedown={stop.clone()} ondblclick={stop.clone()} onclick={stop}>
            <div class="palette-search">
                { icons::search() }
                <input ref={input} placeholder="Add a block…" value={(*query).clone()} {oninput} {onkeydown}
                    role="combobox" aria-label="Block to add" aria-autocomplete="list"
                    aria-expanded="true" aria-controls="quick-options"
                    aria-activedescendant={(!picks.is_empty()).then(|| format!("quick-option-{current}"))} />
            </div>
            <ul id="quick-options" role="listbox" ref={list}>
                { for rows }
                if picks.is_empty() {
                    <li class="quick-empty muted">{ "No block matches." }</li>
                }
            </ul>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_lists_every_creatable_block() {
        let creatable = catalog::BLOCKS.iter().filter(|b| b.creatable).count();
        assert_eq!(search("").len(), creatable);
        assert_eq!(search("   ").len(), creatable);
    }

    #[test]
    fn label_prefix_ranks_first() {
        let found = search("gain");
        assert_eq!(found[0].label, "Gain");
        let found = search("int");
        assert_eq!(found[0].label, "Integrator");
    }

    #[test]
    fn matches_words_types_and_categories() {
        assert!(search("wave").iter().any(|b| b.label == "Sine Wave"));
        assert!(search("discrete").iter().any(|b| b.category == "Discrete"));
        assert!(search("zzzz").is_empty());
    }

    #[test]
    fn annotation_is_offered_only_when_asked_for() {
        assert_eq!(quick_picks("", &[], None).last(), Some(&Pick::Annotation));
        assert_eq!(quick_picks("ann", &[], None), vec![Pick::Annotation]);
        assert!(quick_picks("text", &[], None).contains(&Pick::Annotation));
        assert!(!quick_picks("gain", &[], None).contains(&Pick::Annotation));
        assert!(quick_picks("zzzz", &[], None).is_empty());
    }

    #[test]
    fn ending_a_connection_offers_only_blocks_that_can_end_it() {
        // After an output: blocks with an input, and no annotation.
        let after = quick_picks("", &["Constant", "Scope"], Some(PortKind::In));
        assert!(after.contains(&Pick::Block("Scope")));
        assert!(!after.contains(&Pick::Block("Constant")));
        assert!(!after.contains(&Pick::Annotation));
        // Before an input: blocks with an output.
        let before = quick_picks("", &["Constant", "Scope"], Some(PortKind::Out));
        assert!(before.contains(&Pick::Block("Constant")));
        assert!(!before.contains(&Pick::Block("Scope")));
        assert!(quick_picks("ann", &[], Some(PortKind::Out)).is_empty());
    }

    #[test]
    fn recent_blocks_come_first_until_something_is_typed() {
        let recent = ["Saturate", "Gain"];
        let picks = quick_picks("", &recent, None);
        assert_eq!(&picks[..2], &[Pick::Block("Saturate"), Pick::Block("Gain")]);
        assert_eq!(
            picks.iter().filter(|p| **p == Pick::Block("Gain")).count(),
            1
        );
        assert_ne!(
            quick_picks("con", &recent, None)[0],
            Pick::Block("Saturate")
        );
    }

    #[test]
    fn every_creatable_block_has_a_thumbnail() {
        for b in catalog::BLOCKS.iter().filter(|b| b.creatable) {
            let svg = thumbnail(b.type_key, Theme::Dark).unwrap_or_default();
            assert!(svg.starts_with("<svg"), "{}", b.type_key);
        }
    }
}
