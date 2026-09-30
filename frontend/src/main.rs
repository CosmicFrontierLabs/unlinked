mod diagram;
mod local_viewer;

use local_viewer::LocalViewer;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
enum Route {
    #[at("/")]
    Home,
    #[at("/view")]
    LocalView,
    #[not_found]
    #[at("/404")]
    NotFound,
}

fn switch(route: Route) -> Html {
    match route {
        Route::Home => html! { <Home /> },
        Route::LocalView => html! { <LocalViewer /> },
        Route::NotFound => html! { <div class="page"><h1>{ "404 - Not Found" }</h1></div> },
    }
}

#[function_component(App)]
pub fn app() -> Html {
    html! {
        <BrowserRouter>
            <header class="topbar">
                <Link<Route> to={Route::Home} classes="brand">{ "Unlinked" }</Link<Route>>
                <nav>
                    <Link<Route> to={Route::LocalView}>{ "Open local model" }</Link<Route>>
                </nav>
            </header>
            <main>
                <Switch<Route> render={switch} />
            </main>
        </BrowserRouter>
    }
}

#[function_component(Home)]
fn home() -> Html {
    html! {
        <div class="page">
            <h1>{ "Unlinked" }</h1>
            <p class="lead">{ "View and simulate Simulink models in the browser. Everything runs in Rust." }</p>
            <div class="cards">
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
