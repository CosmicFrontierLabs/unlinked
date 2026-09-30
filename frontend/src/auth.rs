//! Signed-in user context and the login page.

use crate::api;
use shared::UserInfo;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// `None` while loading, `Some(None)` when signed out.
#[derive(Clone, PartialEq)]
pub struct Session {
    pub user: Option<Option<Rc<UserInfo>>>,
}

impl Session {
    pub fn user(&self) -> Option<&UserInfo> {
        self.user.as_ref().and_then(|u| u.as_deref())
    }

    pub fn loaded(&self) -> bool {
        self.user.is_some()
    }
}

#[derive(Properties, PartialEq)]
pub struct ProviderProps {
    pub children: Html,
}

#[function_component(SessionProvider)]
pub fn session_provider(props: &ProviderProps) -> Html {
    let session = use_state(|| Session { user: None });
    {
        let session = session.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                let user = api::me().await.ok().flatten().map(Rc::new);
                session.set(Session { user: Some(user) });
            });
        });
    }
    html! {
        <ContextProvider<Session> context={(*session).clone()}>
            { props.children.clone() }
        </ContextProvider<Session>>
    }
}

#[function_component(UserMenu)]
pub fn user_menu() -> Html {
    let session = use_context::<Session>().expect("session context");
    let Some(user) = session.user() else {
        return if session.loaded() {
            html! { <a href="/login">{ "Sign in" }</a> }
        } else {
            html! {}
        };
    };
    let logout = Callback::from(|_: MouseEvent| {
        spawn_local(async {
            let _ = api::logout().await;
            let _ = gloo_utils::window().location().set_href("/");
        });
    });
    html! {
        <div class="user">
            if let Some(url) = &user.avatar_url {
                <img class="avatar" src={url.clone()} alt="" />
            }
            <span>{ user.name.clone().unwrap_or_else(|| user.email.clone()) }</span>
            <button onclick={logout}>{ "Sign out" }</button>
        </div>
    }
}

#[function_component(Login)]
pub fn login() -> Html {
    let providers = use_state(|| None::<api::ApiResult<shared::AuthProvidersResponse>>);
    {
        let providers = providers.clone();
        use_effect_with((), move |_| {
            spawn_local(async move { providers.set(Some(api::providers().await)) });
        });
    }
    let body = match &*providers {
        None => html! { <p class="muted">{ "Loading…" }</p> },
        Some(Err(e)) => html! { <p class="error">{ e.to_string() }</p> },
        Some(Ok(p)) => html! {
            <div class="login-buttons">
                { for p.providers.iter().map(|name| {
                    let label = match name.as_str() {
                        "google" => "Continue with Google".to_string(),
                        "github" => "Continue with GitHub".to_string(),
                        other => format!("Continue with {other}"),
                    };
                    html! { <a class="button primary" href={format!("/api/auth/login/{name}")}>{ label }</a> }
                }) }
                if p.dev_mode {
                    <a class="button" href="/api/auth/dev-login">{ "Dev login" }</a>
                }
                if p.providers.is_empty() && !p.dev_mode {
                    <p class="muted">{ "No sign-in providers are configured on this server." }</p>
                }
            </div>
        },
    };
    html! {
        <div class="page narrow">
            <h1>{ "Sign in" }</h1>
            <p class="muted">{ "Sign in to share models with your organization." }</p>
            { body }
        </div>
    }
}
