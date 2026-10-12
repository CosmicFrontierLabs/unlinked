//! Finding blocks anywhere in a model by name or type, as Simulink's Find
//! does, to open the system they are in with them selected.

use crate::icons;
use std::rc::Rc;
use unlinked_model::{Model, System};
use web_sys::{HtmlInputElement, KeyboardEvent};
use yew::prelude::*;

/// Matches listed at most.
const MAX_FOUND: usize = 50;
/// Blocks visited at most, so a huge model cannot stall typing.
const MAX_VISITED: usize = 200_000;

/// A block found, with the names of the subsystems leading to its system.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub path: Vec<String>,
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// Blocks of `model` whose name or type contains `query`, ignoring case:
/// names starting with it first, then names containing it, then types,
/// shallower levels before deeper ones. An empty query finds nothing.
pub fn find_blocks(model: &Model, query: &str) -> Vec<Found> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut ranked: Vec<(u8, usize, usize, Found)> = Vec::new();
    let mut visited = 0usize;
    let mut pending: Vec<(&System, Vec<String>)> = vec![(&model.root, Vec::new())];
    while let Some((sys, path)) = pending.pop() {
        for b in &sys.blocks {
            visited += 1;
            if visited > MAX_VISITED {
                break;
            }
            let name = b.name.replace('\n', " ");
            let kind = b.stateflow_type().unwrap_or_else(|| b.display_type());
            let lower = name.to_lowercase();
            let rank = if lower.starts_with(&q) {
                Some(0)
            } else if lower.contains(&q) {
                Some(1)
            } else if kind.to_lowercase().contains(&q) {
                Some(2)
            } else {
                None
            };
            if let Some(rank) = rank {
                ranked.push((
                    rank,
                    path.len(),
                    ranked.len(),
                    Found {
                        path: path.clone(),
                        id: b.id.0.clone(),
                        name,
                        kind: kind.to_string(),
                    },
                ));
            }
            if let Some(sub) = &b.subsystem {
                let mut p = path.clone();
                p.push(b.name.clone());
                pending.push((sub, p));
            }
        }
    }
    ranked.sort_by_key(|(rank, depth, order, _)| (*rank, *depth, *order));
    ranked
        .into_iter()
        .take(MAX_FOUND)
        .map(|(.., f)| f)
        .collect()
}

#[derive(Properties, PartialEq)]
pub struct FindProps {
    pub model: Rc<Model>,
    /// Opens the system at the path with the block selected.
    pub on_pick: Callback<Found>,
    pub on_close: Callback<()>,
}

/// Type to find a block anywhere in the model: arrows choose, Enter opens
/// it, Escape closes.
#[function_component(FindBlocks)]
pub fn find_blocks_box(props: &FindProps) -> Html {
    let query = use_state(String::new);
    let chosen = use_state(|| 0usize);
    let input = use_node_ref();
    let list = use_node_ref();
    {
        let input = input.clone();
        use_effect_with((), move |_| {
            if let Some(el) = input.cast::<HtmlInputElement>() {
                let _ = el.focus();
            }
        });
    }
    let found = {
        let model = props.model.clone();
        use_memo((*query).clone(), move |q| find_blocks(&model, q))
    };
    let current = (*chosen).min(found.len().saturating_sub(1));
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
        let (chosen, found) = (chosen.clone(), found.clone());
        let (on_pick, on_close) = (props.on_pick.clone(), props.on_close.clone());
        Callback::from(move |e: KeyboardEvent| {
            // Keys typed here are not diagram shortcuts.
            e.stop_propagation();
            let n = found.len();
            match e.key().as_str() {
                "ArrowDown" if n > 0 => chosen.set((current + 1) % n),
                "ArrowUp" if n > 0 => chosen.set((current + n - 1) % n),
                "Enter" => {
                    if let Some(f) = found.get(current) {
                        on_pick.emit(f.clone());
                    }
                }
                "Escape" => on_close.emit(()),
                _ => return,
            }
            e.prevent_default();
        })
    };
    let stop = Callback::from(|e: MouseEvent| e.stop_propagation());
    let rows = found.iter().enumerate().map(|(i, f)| {
        let onclick = {
            let (on_pick, f) = (props.on_pick.clone(), f.clone());
            Callback::from(move |e: MouseEvent| {
                e.stop_propagation();
                on_pick.emit(f.clone())
            })
        };
        let onmouseenter = {
            let chosen = chosen.clone();
            Callback::from(move |_: MouseEvent| chosen.set(i))
        };
        let place = if f.path.is_empty() {
            props.model.name.clone()
        } else {
            f.path.join(" / ").replace('\n', " ")
        };
        html! {
            <li id={format!("find-option-{i}")} role="option" aria-selected={(i == current).to_string()}
                class={classes!("quick-row", (i == current).then_some("chosen"))} {onclick} {onmouseenter}>
                <span class="quick-label">{ &f.name }<span class="quick-detail">{ format!("  {}", f.kind) }</span></span>
                <span class="quick-detail">{ place }</span>
            </li>
        }
    });
    let typed = !query.trim().is_empty();
    html! {
        <div class="quick-insert find-blocks" onmousedown={stop.clone()} ondblclick={stop.clone()} onclick={stop}>
            <div class="palette-search">
                { icons::search() }
                <input ref={input} placeholder="Find blocks by name or type…" value={(*query).clone()}
                    {oninput} {onkeydown} role="combobox" aria-label="Find blocks"
                    aria-autocomplete="list" aria-expanded={typed.to_string()} aria-controls="find-options"
                    aria-activedescendant={(!found.is_empty()).then(|| format!("find-option-{current}"))} />
            </div>
            if typed {
                <ul id="find-options" role="listbox" ref={list}>
                    { for rows }
                    if found.is_empty() {
                        <li class="quick-empty muted">{ "No block matches." }</li>
                    }
                </ul>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        let mdl = "Model {\n Name m\n System {\n Name m\n Block {\n BlockType Gain\n Name \"Plant gain\"\n SID 1\n Position [0, 0, 30, 30]\n }\n Block {\n BlockType SubSystem\n Name Ctrl\n SID 2\n Position [100, 0, 130, 30]\n System {\n Name Ctrl\n Block {\n BlockType Gain\n Name Kp\n SID 3\n Position [0, 0, 30, 30]\n }\n }\n }\n }\n}\n";
        unlinked_import::import_mdl("m.mdl", mdl).unwrap()
    }

    #[test]
    fn finds_names_then_types_across_levels() {
        let m = model();
        assert!(find_blocks(&m, "  ").is_empty());
        let kp = find_blocks(&m, "kp");
        assert_eq!(kp.len(), 1);
        assert_eq!(
            (kp[0].path.clone(), kp[0].id.as_str()),
            (vec!["Ctrl".to_string()], "3")
        );
        // Both gains by type; the root one first.
        let gains = find_blocks(&m, "gain");
        assert_eq!(gains[0].name, "Plant gain");
        assert!(gains.iter().any(|f| f.name == "Kp"));
    }
}
