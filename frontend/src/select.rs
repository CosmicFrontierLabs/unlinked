//! A select whose displayed choice follows application state, including undo.

use web_sys::HtmlSelectElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct SelectProps {
    pub value: AttrValue,
    pub children: Children,
    #[prop_or_default]
    pub onchange: Callback<Event>,
    #[prop_or_default]
    pub disabled: bool,
    #[prop_or_default]
    pub id: Option<AttrValue>,
    #[prop_or_default]
    pub aria_invalid: Option<AttrValue>,
    #[prop_or_default]
    pub aria_label: Option<AttrValue>,
    #[prop_or_default]
    pub aria_describedby: Option<AttrValue>,
}

#[function_component(Select)]
pub fn select(props: &SelectProps) -> Html {
    let node = use_node_ref();
    {
        let (node, value) = (node.clone(), props.value.clone());
        // Option `selected` attributes set defaults. After interaction they
        // need not update the live selection (notably in Firefox). Reconcile
        // after children render, even if options changed without a new value.
        use_effect(move || {
            if let Some(select) = node.cast::<HtmlSelectElement>() {
                if select.value() != value.as_str() {
                    select.set_value(&value);
                }
            }
        });
    }
    let onchange = {
        let (node, value, changed) = (node.clone(), props.value.clone(), props.onchange.clone());
        Callback::from(move |event: Event| {
            changed.emit(event);
            // A rejected edit may leave props equal, skipping another render.
            // Restore the committed value now; an accepted edit's render will
            // apply its new value through the effect above.
            if let Some(select) = node.cast::<HtmlSelectElement>() {
                select.set_value(&value);
            }
        })
    };
    html! {
        <select ref={node} id={props.id.clone()} disabled={props.disabled}
            {onchange} aria-label={props.aria_label.clone()} aria-invalid={props.aria_invalid.clone()}
            aria-describedby={props.aria_describedby.clone()}>
            {props.children.clone()}
        </select>
    }
}
