//! A small hook for loading API data tied to dependencies.

use crate::api::{ApiError, ApiResult};
use std::cell::Cell;
use std::future::Future;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

pub enum Fetch<T> {
    Loading,
    Ready(T),
    Failed(ApiError),
}

/// Run `load(deps)` whenever `deps` change and expose the latest result.
/// The state returns to `Loading` on every change, and a response that
/// arrives after the dependencies changed again (or after unmount) is
/// dropped, so a slow earlier request can never overwrite a newer one.
#[hook]
pub fn use_fetch<T, D, F, Fut>(deps: D, load: F) -> UseStateHandle<Fetch<T>>
where
    T: 'static,
    D: PartialEq + Clone + 'static,
    F: FnOnce(D) -> Fut + 'static,
    Fut: Future<Output = ApiResult<T>> + 'static,
{
    let state = use_state(|| Fetch::Loading);
    let generation = use_memo((), |_| Rc::new(Cell::new(0u64)));
    {
        let state = state.clone();
        let generation = (*generation).clone();
        use_effect_with(deps, move |deps| {
            let ticket = generation.get() + 1;
            generation.set(ticket);
            state.set(Fetch::Loading);
            let fut = load(deps.clone());
            let current = generation.clone();
            spawn_local(async move {
                let result = fut.await;
                if current.get() == ticket {
                    state.set(match result {
                        Ok(v) => Fetch::Ready(v),
                        Err(e) => Fetch::Failed(e),
                    });
                }
            });
            move || generation.set(generation.get() + 1)
        });
    }
    state
}

/// A counter that forces refetches. Increments go through a reducer so
/// concurrent mutations each bump the latest value.
pub struct Reload(pub u32);

impl Reducible for Reload {
    type Action = ();

    fn reduce(self: Rc<Self>, _: ()) -> Rc<Self> {
        Rc::new(Reload(self.0.wrapping_add(1)))
    }
}

#[hook]
pub fn use_reload() -> UseReducerHandle<Reload> {
    use_reducer(|| Reload(0))
}

/// Render loading/error states, or the loaded value with `ready`.
pub fn view<T>(fetch: &Fetch<T>, ready: impl FnOnce(&T) -> Html) -> Html {
    match fetch {
        Fetch::Loading => html! { <p class="muted">{ "Loading…" }</p> },
        Fetch::Failed(e) => html! { <p class="error">{ e.to_string() }</p> },
        Fetch::Ready(v) => ready(v),
    }
}
