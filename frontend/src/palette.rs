//! Adding blocks: the library palette beside the diagram and the quick
//! insert search that opens on the canvas, both over the native catalog.

use crate::icons;
use std::collections::HashMap;
use std::rc::Rc;
use unlinked_model::catalog::{self, BlockDescriptor};
use unlinked_model::edit::Edit;
use unlinked_model::{BlockId, Model, Rect, SourceFormat};
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
        catalog::PALETTE_CATEGORIES
            .iter()
            .map(|category| {
                html! {
                    <details class="palette-group" open=true>
                        <summary>{ *category }</summary>
                        <ul>{ for catalog::blocks_in_category(category).map(tile) }</ul>
                    </details>
                }
            })
            .collect::<Html>()
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
}

/// What the quick insert offers for `query`: the best matching blocks, then
/// an annotation when nothing is typed yet or the query names one.
fn quick_picks(query: &str) -> Vec<Pick> {
    let q = query.trim().to_lowercase();
    let annotation = ["annotation", "text", "note", "comment"]
        .iter()
        .any(|word| word.starts_with(&q));
    search(query)
        .into_iter()
        .take(MAX_MATCHES)
        .map(|b| Pick::Block(b.type_key))
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
    let picks = quick_picks(&query);
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
        assert_eq!(quick_picks("").last(), Some(&Pick::Annotation));
        assert_eq!(quick_picks("ann"), vec![Pick::Annotation]);
        assert!(quick_picks("text").contains(&Pick::Annotation));
        assert!(!quick_picks("gain").contains(&Pick::Annotation));
        assert!(quick_picks("zzzz").is_empty());
    }

    #[test]
    fn every_creatable_block_has_a_thumbnail() {
        for b in catalog::BLOCKS.iter().filter(|b| b.creatable) {
            let svg = thumbnail(b.type_key, Theme::Dark).unwrap_or_default();
            assert!(svg.starts_with("<svg"), "{}", b.type_key);
        }
    }
}
