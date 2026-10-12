//! Protect a mounted workspace from accidental link and browser-history exits.
use gloo_events::{EventListener, EventListenerOptions, EventListenerPhase};
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use yew::prelude::*;

thread_local! {
    static LEAVE_GUARD: RefCell<Option<(String, String, wasm_bindgen::JsValue)>> = const { RefCell::new(None) };
}

/// Install before the router's history listener. Window-targeted popstate
/// listeners run in registration order, so a component-mounted listener is
/// too late to protect the old route from unmounting.
pub fn install_history_guard() {
    EventListener::new_with_options(
        &gloo_utils::window(),
        "popstate",
        EventListenerOptions::run_in_capture_phase(),
        move |event| {
            let guard = LEAVE_GUARD.with(|guard| guard.borrow().clone());
            let Some((message, current_url, history_state)) = guard else {
                return;
            };
            if gloo_utils::window().location().href().ok().as_deref() == Some(current_url.as_str())
            {
                return;
            }
            if !gloo_utils::window()
                .confirm_with_message(&message)
                .unwrap_or(false)
            {
                // popstate is not cancelable. Restore the current entry before
                // the router sees the traversal; the mounted draft never leaves.
                // The rejected forward branch is replaced by the restored entry.
                if let Ok(history) = gloo_utils::window().history() {
                    if history
                        .push_state_with_url(&history_state, "", Some(&current_url))
                        .is_ok()
                    {
                        event.stop_immediate_propagation();
                    }
                }
            }
        },
    )
    .forget();
}

#[hook]
pub fn use_navigation_guard(message: Option<String>) {
    use_effect_with(message, move |message| {
        let listeners = message.as_ref().map(|message| {
            let window = gloo_utils::window();
            let current_url = window.location().href().unwrap_or_default();
            let history_state = window.history().ok().and_then(|h| h.state().ok());
            let capture = EventListenerOptions {
                phase: EventListenerPhase::Capture,
                passive: false,
            };
            let click_message = message.clone();
            let click_url = current_url.clone();
            let click = EventListener::new_with_options(
                &gloo_utils::document(),
                "click",
                capture,
                move |event| {
                    let Some(mouse) = event.dyn_ref::<web_sys::MouseEvent>() else {
                        return;
                    };
                    if mouse.button() != 0
                        || mouse.ctrl_key()
                        || mouse.meta_key()
                        || mouse.shift_key()
                        || mouse.alt_key()
                    {
                        return;
                    }
                    let Some(anchor) = event
                        .target()
                        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                        .and_then(|target| target.closest("a[href]").ok().flatten())
                    else {
                        return;
                    };
                    if anchor.has_attribute("download")
                        || anchor
                            .get_attribute("target")
                            .is_some_and(|target| !target.is_empty() && target != "_self")
                    {
                        return;
                    }
                    let href = anchor.get_attribute("href").unwrap_or_default();
                    if href.starts_with('#')
                        || href.starts_with("data:")
                        || href.starts_with("blob:")
                    {
                        return;
                    }
                    if web_sys::Url::new_with_base(&href, &click_url)
                        .ok()
                        .is_some_and(|url| url.href() == click_url)
                    {
                        return;
                    }
                    if !gloo_utils::window()
                        .confirm_with_message(&click_message)
                        .unwrap_or(false)
                    {
                        event.prevent_default();
                        event.stop_immediate_propagation();
                    }
                },
            );
            LEAVE_GUARD.with(|guard| {
                *guard.borrow_mut() = Some((
                    message.clone(),
                    current_url,
                    history_state.unwrap_or(wasm_bindgen::JsValue::NULL),
                ))
            });
            click
        });
        move || {
            LEAVE_GUARD.with(|guard| *guard.borrow_mut() = None);
            drop(listeners);
        }
    });
}
