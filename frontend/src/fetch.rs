//! A small hook for loading API data tied to dependencies.

use crate::api::{ApiError, ApiResult};
use std::future::Future;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

pub enum Fetch<T> {
    Loading,
    Ready(T),
    Failed(ApiError),
}

/// Run `load(deps)` whenever `deps` change and expose the latest result.
/// Include a counter in `deps` to force a reload after a mutation.
#[hook]
pub fn use_fetch<T, D, F, Fut>(deps: D, load: F) -> UseStateHandle<Fetch<T>>
where
    T: 'static,
    D: PartialEq + Clone + 'static,
    F: FnOnce(D) -> Fut + 'static,
    Fut: Future<Output = ApiResult<T>> + 'static,
{
    let state = use_state(|| Fetch::Loading);
    {
        let state = state.clone();
        use_effect_with(deps, move |deps| {
            let fut = load(deps.clone());
            spawn_local(async move {
                state.set(match fut.await {
                    Ok(v) => Fetch::Ready(v),
                    Err(e) => Fetch::Failed(e),
                });
            });
        });
    }
    state
}

/// Render loading/error states, or the loaded value with `ready`.
pub fn view<T>(fetch: &Fetch<T>, ready: impl FnOnce(&T) -> Html) -> Html {
    match fetch {
        Fetch::Loading => html! { <p class="muted">{ "Loading…" }</p> },
        Fetch::Failed(e) => html! { <p class="error">{ e.to_string() }</p> },
        Fetch::Ready(v) => ready(v),
    }
}
