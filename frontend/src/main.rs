mod api;
mod auth;
pub mod diagnostics_worker;
mod diagram;
mod dialog;
mod editor;
mod fetch;
mod local_viewer;
mod pages;
mod plot;
mod settings;
mod sim;
mod transpiler;

use auth::{Login, Session, SessionProvider, UserMenu};
use local_viewer::LocalViewer;
use pages::{ComparePage, Dashboard, FilePage, OrgPage, ProjectPage};
use uuid::Uuid;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
pub enum Route {
    #[at("/")]
    Home,
    #[at("/login")]
    Login,
    #[at("/transpile")]
    Transpile,
    #[at("/view")]
    LocalView,
    #[at("/orgs/:org_id")]
    Org { org_id: Uuid },
    #[at("/projects/:project_id")]
    Project { project_id: Uuid },
    #[at("/projects/:project_id/files/:file_id")]
    File { project_id: Uuid, file_id: Uuid },
    #[at("/projects/:project_id/files/:file_id/versions/:version_id")]
    FileVersion {
        project_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
    },
    #[at("/projects/:project_id/files/:file_id/compare/:old/:new")]
    Compare {
        project_id: Uuid,
        file_id: Uuid,
        old: Uuid,
        new: Uuid,
    },
    #[not_found]
    #[at("/404")]
    NotFound,
}

fn switch(route: Route) -> Html {
    match route {
        Route::Home => html! { <Home /> },
        Route::Login => html! { <Login /> },
        Route::Transpile => html! { <transpiler::Transpiler /> },
        Route::LocalView => html! { <LocalViewer /> },
        Route::Org { org_id } => html! { <RequireLogin><OrgPage {org_id} /></RequireLogin> },
        Route::Project { project_id } => {
            html! { <RequireLogin><ProjectPage {project_id} /></RequireLogin> }
        }
        Route::File {
            project_id,
            file_id,
        } => html! { <RequireLogin><FilePage {project_id} {file_id} /></RequireLogin> },
        Route::FileVersion {
            project_id,
            file_id,
            version_id,
        } => html! {
            <RequireLogin><FilePage {project_id} {file_id} version_id={Some(version_id)} /></RequireLogin>
        },
        Route::Compare {
            project_id,
            file_id,
            old,
            new,
        } => html! {
            <RequireLogin><ComparePage {project_id} {file_id} {old} {new} /></RequireLogin>
        },
        Route::NotFound => html! { <div class="page"><h1>{ "404 - Not Found" }</h1></div> },
    }
}

#[function_component(App)]
pub fn app() -> Html {
    html! {
        <SessionProvider>
            <BrowserRouter>
                <header class="topbar">
                    <Link<Route> to={Route::Home} classes="brand">{ "Unlinked" }</Link<Route>>
                    <nav>
                        <Link<Route> to={Route::LocalView}>{ "Open local model" }</Link<Route>>
                        <Link<Route> to={Route::Transpile}>{ "MATLAB to Rust" }</Link<Route>>
                    </nav>
                    <UserMenu />
                </header>
                <main>
                    <Switch<Route> render={switch} />
                </main>
            </BrowserRouter>
        </SessionProvider>
    }
}

#[derive(Properties, PartialEq)]
struct RequireLoginProps {
    children: Html,
}

/// Show `children` to signed-in users, a sign-in prompt otherwise.
#[function_component(RequireLogin)]
fn require_login(props: &RequireLoginProps) -> Html {
    let session = use_context::<Session>().expect("session context");
    if !session.loaded() {
        return html! {};
    }
    if session.user().is_none() {
        return html! {
            <div class="page narrow">
                <h2>{ "Sign in required" }</h2>
                <Link<Route> to={Route::Login} classes="button primary">{ "Sign in" }</Link<Route>>
            </div>
        };
    }
    props.children.clone()
}

#[function_component(Home)]
fn home() -> Html {
    let session = use_context::<Session>().expect("session context");
    if session.user().is_some() {
        return html! { <Dashboard /> };
    }
    html! {
        <div class="page">
            <h1>{ "Unlinked" }</h1>
            <p class="lead">{ "View, share and simulate Simulink models in the browser. Everything runs in Rust." }</p>
            <div class="cards">
                <Link<Route> to={Route::Login} classes="card">
                    <h3>{ "Sign in" }</h3>
                    <p class="muted">{ "Share models with your organization, with version history." }</p>
                </Link<Route>>
                <Link<Route> to={Route::LocalView} classes="card">
                    <h3>{ "Open a local model" }</h3>
                    <p class="muted">{ "Parse and render a .slx or .mdl file without uploading it." }</p>
                </Link<Route>>
            </div>
        </div>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
