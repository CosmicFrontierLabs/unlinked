//! Organization, project and file pages backed by the server API.

mod file_browser;
use file_browser::FileBrowser;

use crate::api::{self, ApiError};
use crate::diagram::DiagramView;
use crate::editor::{Base, ModelEditor};
use crate::fetch::{use_fetch, use_reload, view, Fetch, Reload};
use crate::sim::SimulationPanel;
use crate::transpiler::Transpiler;
use crate::Route;
use chrono::{DateTime, Utc};
use shared::{
    CreateProjectRequest, DefaultProjectRole, FileInfo, FileVersionInfo, OrgRole, Organization,
    Project, ProjectRole, UserInfo,
};
use std::rc::Rc;
use unlinked_model::diff::{BlockChange, ChartChange, ModelDiff};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Event, HtmlInputElement, HtmlSelectElement, InputEvent};
use yew::prelude::*;
use yew_router::prelude::*;

fn when(t: &DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

fn who(u: &Option<UserInfo>) -> String {
    u.as_ref()
        .map(|u| u.name.clone().unwrap_or_else(|| u.email.clone()))
        .unwrap_or_else(|| "—".into())
}

fn size(bytes: i64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
        b => format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
    }
}

fn input_value(e: InputEvent) -> String {
    e.target_unchecked_into::<HtmlInputElement>().value()
}

fn select_value(e: Event) -> String {
    e.target_unchecked_into::<HtmlSelectElement>().value()
}

/// A text field bound to a `UseStateHandle<String>`.
fn text_input(state: &UseStateHandle<String>, placeholder: &str) -> Html {
    let s = state.clone();
    html! {
        <input value={(**state).clone()} placeholder={placeholder.to_string()}
            aria-label={if placeholder == "email@example.com" { "Member email" } else { placeholder }.to_string()}
            oninput={Callback::from(move |e: InputEvent| s.set(input_value(e)))} />
    }
}

/// Run an API mutation; on success bump `reload`, on failure show the error.
fn mutate<Fut>(fut: Fut, reload: UseReducerHandle<Reload>, error: UseStateHandle<Option<ApiError>>)
where
    Fut: std::future::Future<Output = Result<(), ApiError>> + 'static,
{
    spawn_local(async move {
        match fut.await {
            Ok(()) => {
                error.set(None);
                reload.dispatch(());
            }
            Err(e) => error.set(Some(e)),
        }
    });
}

fn error_line(error: &Option<ApiError>) -> Html {
    match error {
        Some(e) => html! { <p class="error">{ e.to_string() }</p> },
        None => html! {},
    }
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

#[function_component(Dashboard)]
pub fn dashboard() -> Html {
    let reload = use_reload();
    let error = use_state(|| None::<ApiError>);
    let orgs = use_fetch(reload.0, |_| api::orgs());
    let projects = use_fetch(reload.0, |_| api::all_projects());
    let new_org = use_state(String::new);

    let create_org = {
        let (new_org, reload, error) = (new_org.clone(), reload.clone(), error.clone());
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let name = new_org.trim().to_string();
            if name.is_empty() {
                return;
            }
            new_org.set(String::new());
            mutate(
                async move { api::create_org(name).await.map(|_| ()) },
                reload.clone(),
                error.clone(),
            );
        })
    };

    let project_list = |org: &Organization| -> Html {
        let Fetch::Ready(all) = &*projects else {
            return html! {};
        };
        let mine: Vec<&Project> = all.iter().filter(|p| p.org_id == org.id).collect();
        if mine.is_empty() {
            return html! { <p class="muted">{ "No projects yet." }</p> };
        }
        html! {
            <ul class="plain">
                { for mine.into_iter().map(|p| html! {
                    <li>
                        <Link<Route> to={Route::Project { project_id: p.id }}>{ &p.name }</Link<Route>>
                        <span class="muted">{ format!(" · {}", p.my_role.as_str()) }</span>
                    </li>
                }) }
            </ul>
        }
    };

    html! {
        <div class="page">
            <h1>{ "Your organizations" }</h1>
            { error_line(&error) }
            { view(&orgs, |orgs: &Vec<Organization>| html! {
                <div class="cards">
                    { for orgs.iter().map(|o| html! {
                        <div class="card">
                            <h3><Link<Route> to={Route::Org { org_id: o.id }}>{ &o.name }</Link<Route>></h3>
                            <div class="muted">{ format!("You are {}", o.my_role.as_str()) }</div>
                            { project_list(o) }
                        </div>
                    }) }
                </div>
            }) }
            <form class="inline-form" onsubmit={create_org}>
                { text_input(&new_org, "New organization name") }
                <button class="primary" type="submit">{ "Create organization" }</button>
            </form>
            <h2>{ "Local files" }</h2>
            <p><Link<Route> to={Route::LocalView}>{ "Open a model from disk" }</Link<Route>>
                <span class="muted">{ " (parsed in the browser, not uploaded)" }</span></p>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Organization
// ---------------------------------------------------------------------------

const ORG_ROLES: [OrgRole; 3] = [OrgRole::Member, OrgRole::Admin, OrgRole::Owner];
const PROJECT_ROLES: [ProjectRole; 3] =
    [ProjectRole::Viewer, ProjectRole::Editor, ProjectRole::Owner];
const DEFAULT_ROLES: [DefaultProjectRole; 3] = [
    DefaultProjectRole::None,
    DefaultProjectRole::Viewer,
    DefaultProjectRole::Editor,
];

#[derive(Properties, PartialEq)]
pub struct OrgProps {
    pub org_id: Uuid,
}

#[function_component(OrgPage)]
pub fn org_page(props: &OrgProps) -> Html {
    let id = props.org_id;
    let reload = use_reload();
    let error = use_state(|| None::<ApiError>);
    let org = use_fetch((id, reload.0), move |(id, _)| api::org(id));
    let members = use_fetch((id, reload.0), move |(id, _)| api::org_members(id));
    let projects = use_fetch((id, reload.0), move |(id, _)| api::org_projects(id));
    let admin = matches!(&*org, Fetch::Ready(o) if o.my_role >= OrgRole::Admin);
    let audit = use_fetch((id, reload.0, admin), move |(id, _, admin)| async move {
        if admin {
            api::audit(id).await
        } else {
            Ok(Vec::new())
        }
    });

    let email = use_state(String::new);
    let role = use_state(|| OrgRole::Member);
    let add_member = {
        let (email, role, reload, error) =
            (email.clone(), role.clone(), reload.clone(), error.clone());
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let addr = email.trim().to_string();
            let r = *role;
            email.set(String::new());
            mutate(
                async move { api::add_org_member(id, addr, r).await.map(|_| ()) },
                reload.clone(),
                error.clone(),
            );
        })
    };

    let project_name = use_state(String::new);
    let project_desc = use_state(String::new);
    let project_default = use_state(|| DefaultProjectRole::Viewer);
    let create_project = {
        let (name, desc, default_role, reload, error) = (
            project_name.clone(),
            project_desc.clone(),
            project_default.clone(),
            reload.clone(),
            error.clone(),
        );
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let req = CreateProjectRequest {
                name: name.trim().to_string(),
                description: desc.trim().to_string(),
                default_role: Some(*default_role),
            };
            if req.name.is_empty() {
                return;
            }
            name.set(String::new());
            desc.set(String::new());
            mutate(
                async move { api::create_project(id, req).await.map(|_| ()) },
                reload.clone(),
                error.clone(),
            );
        })
    };

    let member_rows = {
        let (reload, error) = (reload.clone(), error.clone());
        move |members: &Vec<shared::OrgMember>| -> Html {
            html! {
                <table>
                    <tr><th>{ "Member" }</th><th>{ "Role" }</th><th>{ "Joined" }</th><th /></tr>
                    { for members.iter().map(|m| {
                        let user = m.user.id;
                        let on_role = {
                            let (reload, error) = (reload.clone(), error.clone());
                            Callback::from(move |e: Event| {
                                if let Some(r) = OrgRole::parse(&select_value(e)) {
                                    mutate(api::update_org_member(id, user, r), reload.clone(), error.clone());
                                }
                            })
                        };
                        let on_remove = {
                            let (reload, error) = (reload.clone(), error.clone());
                            Callback::from(move |_: MouseEvent| {
                                mutate(api::remove_org_member(id, user), reload.clone(), error.clone());
                            })
                        };
                        html! {
                            <tr>
                                <td>{ who(&Some(m.user.clone())) }<div class="muted">{ &m.user.email }</div></td>
                                <td>
                                    if admin {
                                        <select onchange={on_role} aria-label={format!("Organization role for {}", m.user.email)}>
                                            { for ORG_ROLES.iter().map(|r| html! {
                                                <option value={r.as_str()} selected={*r == m.role}>{ r.as_str() }</option>
                                            }) }
                                        </select>
                                    } else {
                                        { m.role.as_str() }
                                    }
                                </td>
                                <td class="muted">{ when(&m.joined_at) }</td>
                                <td>if admin { <button onclick={on_remove}>{ "Remove" }</button> }</td>
                            </tr>
                        }
                    }) }
                </table>
            }
        }
    };

    let set_role = {
        let role = role.clone();
        Callback::from(move |e: Event| {
            if let Some(r) = OrgRole::parse(&select_value(e)) {
                role.set(r);
            }
        })
    };
    let set_default = {
        let d = project_default.clone();
        Callback::from(move |e: Event| {
            if let Some(r) = DefaultProjectRole::parse(&select_value(e)) {
                d.set(r);
            }
        })
    };

    html! {
        <div class="page">
            { view(&org, |o: &Organization| html! {
                <>
                    <div class="muted"><Link<Route> to={Route::Home}>{ "Organizations" }</Link<Route>></div>
                    <h1>{ &o.name }</h1>
                </>
            }) }
            { error_line(&error) }

            <h2>{ "Projects" }</h2>
            { view(&projects, |ps: &Vec<Project>| html! {
                <table>
                    <tr><th>{ "Project" }</th><th>{ "Your role" }</th><th>{ "Default access" }</th><th>{ "Created" }</th></tr>
                    { for ps.iter().map(|p| html! {
                        <tr>
                            <td>
                                <Link<Route> to={Route::Project { project_id: p.id }}>{ &p.name }</Link<Route>>
                                <div class="muted">{ &p.description }</div>
                            </td>
                            <td>{ p.my_role.as_str() }</td>
                            <td>{ p.default_role.as_str() }</td>
                            <td class="muted">{ when(&p.created_at) }</td>
                        </tr>
                    }) }
                </table>
            }) }
            <form class="inline-form" onsubmit={create_project}>
                { text_input(&project_name, "Project name") }
                { text_input(&project_desc, "Description") }
                <label class="muted">{ "Org members get " }</label>
                <select onchange={set_default} aria-label="Default project access for organization members">
                    { for DEFAULT_ROLES.iter().map(|r| html! {
                        <option value={r.as_str()} selected={*r == *project_default}>{ r.as_str() }</option>
                    }) }
                </select>
                <button class="primary" type="submit">{ "Create project" }</button>
            </form>

            <h2>{ "Members" }</h2>
            { view(&members, member_rows) }
            if admin {
                <form class="inline-form" onsubmit={add_member}>
                    { text_input(&email, "email@example.com") }
                    <select onchange={set_role} aria-label="New organization member role">
                        { for ORG_ROLES.iter().map(|r| html! {
                            <option value={r.as_str()} selected={*r == *role}>{ r.as_str() }</option>
                        }) }
                    </select>
                    <button type="submit">{ "Add member" }</button>
                </form>
                <p class="muted">{ "People must sign in once before they can be added." }</p>

                <h2>{ "Audit log" }</h2>
                { view(&audit, |entries: &Vec<shared::AuditEntry>| html! {
                    <table class="audit">
                        { for entries.iter().take(100).map(|a| html! {
                            <tr>
                                <td class="muted">{ when(&a.created_at) }</td>
                                <td>{ who(&a.actor) }</td>
                                <td>{ &a.action }</td>
                                <td><code>{ a.detail.to_string() }</code></td>
                            </tr>
                        }) }
                    </table>
                }) }
            }
        </div>
    }
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

#[derive(Properties, PartialEq)]
pub struct ProjectProps {
    pub project_id: Uuid,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct FolderQuery {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    folder: String,
}

#[function_component(ProjectPage)]
pub fn project_page(props: &ProjectProps) -> Html {
    let id = props.project_id;
    let reload = use_reload();
    let error = use_state(|| None::<ApiError>);
    let project = use_fetch((id, reload.0), move |(id, _)| api::project(id));
    let files = use_fetch((id, reload.0), move |(id, _)| api::files(id));
    let members = use_fetch((id, reload.0), move |(id, _)| api::project_members(id));
    let role = match &*project {
        Fetch::Ready(p) => Some(p.my_role),
        _ => None,
    };
    let editor = role.is_some_and(|r| r >= ProjectRole::Editor);
    let owner = role == Some(ProjectRole::Owner);

    let location = use_location().expect("router location");
    let navigator = use_navigator().expect("router navigator");
    let browsed_folder = location.query::<FolderQuery>().unwrap_or_default().folder;
    // Browsing chooses the upload destination, but typing a new destination
    // does not move the browser into a folder that does not exist yet.
    let folder = use_state(|| browsed_folder.clone());
    {
        let folder = folder.clone();
        use_effect_with(browsed_folder.clone(), move |path| folder.set(path.clone()));
    }
    let message = use_state(String::new);
    let uploading = use_state(|| false);
    let on_upload = {
        let (folder, message, uploading, reload, error) = (
            folder.clone(),
            message.clone(),
            uploading.clone(),
            reload.clone(),
            error.clone(),
        );
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            let Some(list) = input.files() else { return };
            let files: Vec<web_sys::File> =
                (0..list.length()).filter_map(|i| list.get(i)).collect();
            input.set_value("");
            let prefix = folder.trim().trim_matches('/').to_string();
            let msg = message.trim().to_string();
            uploading.set(true);
            let (uploading, reload, error) = (uploading.clone(), reload.clone(), error.clone());
            spawn_local(async move {
                let mut failure = None;
                for f in files {
                    let path = if prefix.is_empty() {
                        f.name()
                    } else {
                        format!("{prefix}/{}", f.name())
                    };
                    let bytes = match api::read_upload(f).await {
                        Ok(b) => b,
                        Err(e) => {
                            failure = Some(e);
                            break;
                        }
                    };
                    if let Err(e) = api::upload(id, &path, &msg, &bytes, None).await {
                        failure = Some(ApiError {
                            status: e.status,
                            message: format!("{path}: {}", e.message),
                        });
                        break;
                    }
                }
                uploading.set(false);
                error.set(failure);
                reload.dispatch(());
            });
        })
    };

    let delete_file = {
        let (reload, error) = (reload.clone(), error.clone());
        Callback::from(move |(file_id, path): (Uuid, String)| {
            if gloo_utils::window()
                .confirm_with_message(&format!("Delete {path} from this project?"))
                .unwrap_or(false)
            {
                mutate(api::delete_file(id, file_id), reload.clone(), error.clone());
            }
        })
    };
    let browse_folder = {
        let error = error.clone();
        Callback::from(move |folder: String| {
            if let Err(e) = navigator
                .push_with_query(&Route::Project { project_id: id }, &FolderQuery { folder })
            {
                error.set(Some(ApiError {
                    status: 0,
                    message: format!("Cannot open folder: {e}"),
                }));
            }
        })
    };

    let email = use_state(String::new);
    let new_role = use_state(|| ProjectRole::Viewer);
    let add_member = {
        let (email, new_role, reload, error) = (
            email.clone(),
            new_role.clone(),
            reload.clone(),
            error.clone(),
        );
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let addr = email.trim().to_string();
            let r = *new_role;
            email.set(String::new());
            mutate(
                async move { api::add_project_member(id, addr, r).await.map(|_| ()) },
                reload.clone(),
                error.clone(),
            );
        })
    };
    let set_new_role = {
        let r = new_role.clone();
        Callback::from(move |e: Event| {
            if let Some(v) = ProjectRole::parse(&select_value(e)) {
                r.set(v);
            }
        })
    };
    let member_rows = {
        let (reload, error) = (reload.clone(), error.clone());
        move |members: &Vec<shared::ProjectMember>| -> Html {
            html! {
                <table>
                    { for members.iter().map(|m| {
                        let user = m.user.id;
                        let on_role = {
                            let (reload, error) = (reload.clone(), error.clone());
                            Callback::from(move |e: Event| {
                                if let Some(r) = ProjectRole::parse(&select_value(e)) {
                                    mutate(api::update_project_member(id, user, r), reload.clone(), error.clone());
                                }
                            })
                        };
                        let on_remove = {
                            let (reload, error) = (reload.clone(), error.clone());
                            Callback::from(move |_: MouseEvent| {
                                mutate(api::remove_project_member(id, user), reload.clone(), error.clone());
                            })
                        };
                        html! {
                            <tr>
                                <td>{ who(&Some(m.user.clone())) }<div class="muted">{ &m.user.email }</div></td>
                                <td>
                                    if owner {
                                        <select onchange={on_role} aria-label={format!("Project role for {}", m.user.email)}>
                                            { for PROJECT_ROLES.iter().map(|r| html! {
                                                <option value={r.as_str()} selected={*r == m.role}>{ r.as_str() }</option>
                                            }) }
                                        </select>
                                    } else {
                                        { m.role.as_str() }
                                    }
                                </td>
                                <td>if owner { <button onclick={on_remove}>{ "Remove" }</button> }</td>
                            </tr>
                        }
                    }) }
                </table>
            }
        }
    };

    html! {
        <div class="page wide">
            { view(&project, |p: &Project| html! {
                <>
                    <div class="muted">
                        <Link<Route> to={Route::Org { org_id: p.org_id }}>{ "Organization" }</Link<Route>>
                    </div>
                    <h1>{ &p.name }</h1>
                    <p class="muted">{ &p.description }</p>
                </>
            }) }
            { error_line(&error) }
            <h2>{ "Files" }</h2>
            if editor {
                <div class="inline-form">
                    { text_input(&folder, "Upload folder (optional)") }
                    { text_input(&message, "Change message") }
                    <label class="button primary file-picker">
                        { if *uploading { "Uploading…" } else { "Upload files…" } }
                        <input type="file" multiple=true class="sr-only" onchange={on_upload} disabled={*uploading} />
                    </label>
                </div>
            }
            { view(&files, |files: &Vec<FileInfo>| html! { <FileBrowser files={files.clone()} folder={browsed_folder.clone()} on_folder={browse_folder.clone()} can_delete={editor} on_delete={delete_file.clone()} /> }) }
            <h2>{ "Members" }</h2>
            <p class="muted">{ "Explicit project members. Organization members also get the project's default access." }</p>
            { view(&members, member_rows) }
            if owner {
                <form class="inline-form" onsubmit={add_member}>
                    { text_input(&email, "email@example.com") }
                    <select onchange={set_new_role} aria-label="New project member role">
                        { for PROJECT_ROLES.iter().map(|r| html! {
                            <option value={r.as_str()} selected={*r == *new_role}>{ r.as_str() }</option>
                        }) }
                    </select>
                    <button type="submit">{ "Add member" }</button>
                </form>
            }
        </div>
    }
}

// ---------------------------------------------------------------------------
// File
// ---------------------------------------------------------------------------

#[derive(Properties, PartialEq)]
pub struct FileProps {
    pub project_id: Uuid,
    pub file_id: Uuid,
    #[prop_or_default]
    pub version_id: Option<Uuid>,
}

enum Content {
    /// The imported model and the exact version it was read from (the
    /// base edits are saved against).
    Model(Rc<unlinked_model::Model>, Base),
    Text(String, Uuid, i32),
    Binary(Uuid, i32),
}

impl Content {
    fn version(&self) -> (Uuid, i32) {
        match self {
            Self::Model(_, base) => (base.version_id, base.version),
            Self::Text(_, id, number) | Self::Binary(id, number) => (*id, *number),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Diagram,
    Simulate,
}

fn is_model(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".slx") || p.ends_with(".mdl")
}

#[function_component(FilePage)]
pub fn file_page(props: &FileProps) -> Html {
    let (project_id, file_id, version_id) = (props.project_id, props.file_id, props.version_id);
    let reload = use_reload();
    let error = use_state(|| None::<ApiError>);
    let tab = use_state(|| Tab::Diagram);
    let history_open = use_state(|| false);
    let simulation_running = use_state(|| false);
    let shown_run = use_state(|| None::<Rc<crate::results::ShownRun>>);
    let requested_signals = use_state(|| None::<crate::results::PlotRequest>);
    let on_result = {
        let (shown_run, requested_signals) = (shown_run.clone(), requested_signals.clone());
        Callback::from(move |run: Option<Rc<crate::results::ShownRun>>| {
            if run.is_none() {
                requested_signals.set(None);
            }
            shown_run.set(run);
        })
    };
    let on_cursor = {
        let shown_run = shown_run.clone();
        Callback::from(move |index: usize| {
            if let Some(run) = &*shown_run {
                if index < run.time.len() && index != run.sample_index {
                    let mut updated = (**run).clone();
                    updated.sample_index = index;
                    shown_run.set(Some(Rc::new(updated)));
                }
            }
        })
    };
    let simulation_controls = use_state_eq(crate::results::SimulationControls::default);
    let on_controls = {
        let controls = simulation_controls.clone();
        use_callback((), move |next, _| controls.set(next))
    };
    let plot_serial = use_mut_ref(|| 0_u64);
    let request_plot = {
        let (requested, tab, serial) =
            (requested_signals.clone(), tab.clone(), plot_serial.clone());
        let shown_run = shown_run.clone();
        Callback::from(move |(ids, replace): (Vec<String>, bool)| {
            let Some(run) = &*shown_run else {
                return;
            };
            let recorded: std::collections::HashSet<&str> = run
                .signals
                .iter()
                .map(|signal| signal.id.as_str())
                .collect();
            let ids: Vec<String> = ids
                .into_iter()
                .filter(|id| recorded.contains(id.as_str()))
                .collect();
            if ids.is_empty() {
                return;
            }
            let next = {
                let mut serial = serial.borrow_mut();
                *serial = serial.wrapping_add(1);
                *serial
            };
            requested.set(Some(crate::results::PlotRequest {
                serial: next,
                ids,
                replace,
            }));
            tab.set(Tab::Simulate);
        })
    };
    let on_plot = request_plot.reform(|id: String| (vec![id], false));
    let on_show_signals = request_plot.reform(|ids: Vec<String>| (ids, true));
    let on_running_change = {
        let simulation_running = simulation_running.clone();
        Callback::from(move |running: bool| simulation_running.set(running))
    };
    let diagram_dirty = use_state(|| false);
    let on_dirty_change = {
        let diagram_dirty = diagram_dirty.clone();
        Callback::from(move |dirty: bool| diagram_dirty.set(dirty))
    };
    let toggle_history = {
        let history_open = history_open.clone();
        Callback::from(move |_: MouseEvent| history_open.set(!*history_open))
    };
    let leave_message = match (*diagram_dirty, *simulation_running) {
        (true, true) => Some("Leave this model? Unsaved diagram changes will be discarded and the running simulation will stop.".to_string()),
        (true, false) => Some("Leave this model? Unsaved diagram changes will be discarded.".to_string()),
        (false, true) => Some("Leave this model? The running simulation will stop.".to_string()),
        _ => None,
    };
    crate::navigation::use_navigation_guard(leave_message.clone());
    // Metadata mutations must not remount either workspace. Content changes
    // only after a save or an explicit version switch.
    let content_reload = use_reload();
    {
        let (diagram_dirty, simulation_running, shown_run, requested_signals) = (
            diagram_dirty.clone(),
            simulation_running.clone(),
            shown_run.clone(),
            requested_signals.clone(),
        );
        use_effect_with(
            (project_id, file_id, version_id, content_reload.0),
            move |_| {
                diagram_dirty.set(false);
                simulation_running.set(false);
                shown_run.set(None);
                requested_signals.set(None);
            },
        );
    }
    let project = use_fetch(project_id, api::project);
    let file = use_fetch((project_id, file_id, reload.0), move |(p, f, _)| {
        api::file(p, f)
    });
    let versions = use_fetch((project_id, file_id, reload.0), move |(p, f, _)| {
        api::versions(p, f)
    });
    let content = use_fetch(
        (project_id, file_id, version_id, content_reload.0),
        move |(p, f, v, _)| async move {
            let info = api::file(p, f).await?;
            // Read an explicit version so the bytes and the recorded base
            // cannot disagree if someone saves meanwhile.
            let (version_id, version) = match v {
                Some(v) => {
                    let versions = api::versions(p, f).await?;
                    let found = versions.iter().find(|x| x.id == v).ok_or(ApiError {
                        status: 404,
                        message: "version not found".into(),
                    })?;
                    (v, found.version)
                }
                None => (info.latest.id, info.latest.version),
            };
            let bytes = api::content(p, f, Some(version_id)).await?;
            Ok(if is_model(&info.path) {
                match unlinked_import::import(&info.path, &bytes) {
                    Ok(m) => {
                        let model = Rc::new(m);
                        let base = Base {
                            path: info.path,
                            version_id,
                            version,
                            model: model.clone(),
                            bytes: Rc::new(bytes),
                        };
                        Content::Model(model, base)
                    }
                    Err(e) => {
                        return Err(ApiError {
                            status: 0,
                            message: format!("cannot import model: {e}"),
                        })
                    }
                }
            } else {
                match String::from_utf8(bytes) {
                    Ok(s) => Content::Text(s, version_id, version),
                    Err(_) => Content::Binary(version_id, version),
                }
            })
        },
    );
    let editor = matches!(&*project, Fetch::Ready(p) if p.my_role >= ProjectRole::Editor);
    // After saving edits, show the new latest version.
    let on_saved = {
        let (navigator, reload, content_reload) =
            (use_navigator(), reload.clone(), content_reload.clone());
        Callback::from(move |_: ()| {
            if let Some(nav) = &navigator {
                nav.push(&Route::File {
                    project_id,
                    file_id,
                });
            }
            reload.dispatch(());
            content_reload.dispatch(());
        })
    };

    let on_new_version = {
        let (file, reload, error) = (file.clone(), reload.clone(), error.clone());
        Callback::from(move |e: Event| {
            let Fetch::Ready(info) = &*file else { return };
            let input: HtmlInputElement = e.target_unchecked_into();
            let Some(f) = input.files().and_then(|l| l.get(0)) else {
                return;
            };
            input.set_value("");
            let path = info.path.clone();
            mutate(
                async move {
                    let bytes = api::read_upload(f).await?;
                    api::upload(project_id, &path, "", &bytes, None)
                        .await
                        .map(|_| ())
                },
                reload.clone(),
                error.clone(),
            );
        })
    };

    let on_rename = {
        let (file, reload, error) = (file.clone(), reload.clone(), error.clone());
        Callback::from(move |_: MouseEvent| {
            let Fetch::Ready(info) = &*file else { return };
            let Ok(Some(path)) =
                gloo_utils::window().prompt_with_message_and_default("New path", &info.path)
            else {
                return;
            };
            let path = path.trim().to_string();
            if path.is_empty() || path == info.path {
                return;
            }
            mutate(
                api::rename_file(project_id, file_id, path),
                reload.clone(),
                error.clone(),
            );
        })
    };

    let model_file = matches!(&*file, Fetch::Ready(f) if is_model(&f.path));
    let matlab_file =
        matches!(&*file, Fetch::Ready(f) if f.path.to_ascii_lowercase().ends_with(".m"));
    let selected_version = match &*content {
        Fetch::Ready(content) => Some(content.version().0),
        _ => version_id,
    };
    let version_list = move |vs: &Vec<FileVersionInfo>| -> Html {
        let current = selected_version.or_else(|| vs.first().map(|v| v.id));
        html! {
            <ul class="versions">
                { for vs.iter().map(|v| {
                    let active = Some(v.id) == current;
                    let to = Route::FileVersion { project_id, file_id, version_id: v.id };
                    let compare = current
                        .filter(|c| model_file && *c != v.id)
                        .map(|c| {
                            // Compare older -> newer regardless of which is selected.
                            let (old, new) = if v.version < vs.iter().find(|x| x.id == c).map_or(0, |x| x.version) {
                                (v.id, c)
                            } else {
                                (c, v.id)
                            };
                            Route::Compare { project_id, file_id, old, new }
                        });
                    html! {
                        <li class={classes!(active.then_some("active"))}>
                            <Link<Route> {to}>{ format!("v{}", v.version) }</Link<Route>>
                            <span class="muted">{ format!(" {} · {}", when(&v.created_at), who(&v.author)) }</span>
                            if let Some(to) = compare {
                                { " " }<Link<Route> {to} classes="compare">{ "compare" }</Link<Route>>
                            }
                            if !v.message.is_empty() { <div>{ &v.message }</div> }
                        </li>
                    }
                }) }
            </ul>
        }
    };

    let shown_version = match &*content {
        Fetch::Ready(content) => Some(content.version().1),
        _ => None,
    };
    let loaded_version_id = match &*content {
        Fetch::Ready(content) => Some(content.version().0),
        _ => version_id,
    };
    let newer_available = match (&*file, shown_version) {
        (Fetch::Ready(info), Some(version)) => {
            info.latest.version > version && version_id.is_none()
        }
        _ => false,
    };
    let show_latest = {
        let (content_reload, leave_message) = (content_reload.clone(), leave_message.clone());
        Callback::from(move |_: MouseEvent| {
            if leave_message.as_ref().is_some_and(|message| {
                !gloo_utils::window()
                    .confirm_with_message(message)
                    .unwrap_or(false)
            }) {
                return;
            }
            content_reload.dispatch(());
        })
    };
    let tab_button = |t: Tab, label: &str| {
        let tab = tab.clone();
        html! {
            <button class={classes!("tab", (*tab == t).then_some("active"))}
                aria-pressed={(*tab == t).to_string()}
                onclick={Callback::from(move |_: MouseEvent| { tab.set(t); })}>{ label }</button>
        }
    };
    let body = view(&content, |c: &Content| match c {
        // Keyed by model identity so a new version remounts the viewer with
        // fresh navigation state instead of keeping a stale subsystem path.
        Content::Model(m, base) => html! {
            <ContextProvider<crate::results::ResultsContext> context={crate::results::ResultsContext {
                run: (*shown_run).clone(), shown_version: base.version, dirty: *diagram_dirty, on_plot: on_plot.clone(), on_show_signals: on_show_signals.clone(),
                on_run: if *diagram_dirty { None } else { simulation_controls.on_run.clone() },
                on_stop: simulation_controls.on_stop.clone(), running: simulation_controls.running,
                run_error: simulation_controls.error.clone(),
                run_unavailable: if *diagram_dirty { Some("Save the diagram changes before running from here.".into()) } else { simulation_controls.unavailable_reason.clone() },
            }}>
                <div class="tabs" role="group" aria-label="Model workspace">
                    { tab_button(Tab::Diagram, "Diagram") }
                    { tab_button(Tab::Simulate, "Simulate") }
                    <span class="workspace-caption">{ "Diagram edits and run results stay open when you switch views." }</span>
                </div>
                // Keep both workspaces mounted. Switching views must not
                // discard edits, reset navigation, or cancel a simulation.
                <section class="workspace-pane" hidden={*tab != Tab::Diagram} aria-label="Diagram">
                    <ModelEditor key={format!("{:p}", Rc::as_ptr(m))} {project_id} {file_id}
                        base={base.clone()} can_edit={editor} active={*tab == Tab::Diagram}
                        on_dirty_change={on_dirty_change.clone()}
                        fit_key={AttrValue::from(format!("{:p}", Rc::as_ptr(m)))}
                        on_saved={on_saved.clone()} />
                </section>
                <section class="workspace-pane" hidden={*tab != Tab::Simulate} aria-label="Simulation">
                    if *diagram_dirty {
                        <p class="workspace-notice" role="status">{ format!("The diagram has unsaved changes. Simulation uses saved v{}. Save your diagram to include those changes.", base.version) }</p>
                    }
                    <SimulationPanel key={format!("{file_id}:{}", base.version)} {project_id} {file_id}
                        on_running_change={on_running_change.clone()} on_controls={on_controls.clone()}
                        on_result={on_result.clone()} on_cursor={on_cursor.clone()} requested_signals={(*requested_signals).clone()}
                        version={base.version} active={*tab == Tab::Simulate} config={m.config.clone()}
                        has_random_sources={m.walk().iter().any(|(_, sys)| sys.blocks.iter().any(|b| matches!(b.block_type.as_str(), "RandomNumber" | "UniformRandomNumber")))}
                        inputs={m.root.blocks.iter().filter(|b| b.block_type == "Inport").map(|b| (b.id.0.clone(), b.name.clone())).collect::<Vec<_>>()}
                        outports={m.root.blocks.iter().filter(|b| b.block_type == "Outport").map(|b| b.name.clone()).collect::<Vec<_>>()} />
                </section>
            </ContextProvider<crate::results::ResultsContext>>
        },
        Content::Text(t, _, _) if matlab_file => html! {
            <>
                <div class="tabs">
                    { tab_button(Tab::Diagram, "Source") }
                    { tab_button(Tab::Simulate, "MATLAB to Rust") }
                </div>
                if *tab == Tab::Diagram {
                    <pre class="source">{ t }</pre>
                } else {
                    <Transpiler source={AttrValue::from(t.clone())} />
                }
            </>
        },
        Content::Text(t, _, _) => html! { <pre class="source">{ t }</pre> },
        Content::Binary(_, _) => html! { <p class="muted">{ "Binary file; use Download." }</p> },
    });

    html! {
        <div class="page-fill">
            <div class="subbar file-bar">
                <nav class="file-breadcrumbs" aria-label="File breadcrumb">
                    <Link<Route> to={Route::Home}>{ "Projects" }</Link<Route>>
                    <span aria-hidden="true">{ "/" }</span>
                    <Link<Route> to={Route::Project { project_id }}>
                        { match &*project { Fetch::Ready(p) => p.name.clone(), _ => "Project".into() } }
                    </Link<Route>>
                    <span aria-hidden="true">{ "/" }</span>
                    { view(&file, |f: &FileInfo| html! { <strong title={f.path.clone()}>{ &f.path }</strong> }) }
                </nav>
                if let Some(version) = shown_version {
                    <span class="version-badge">{ format!("v{version}") }</span>
                }
                if newer_available { <button onclick={show_latest}>{ "Open newer version" }</button> }
                <span class="spacer" />
                { error_line(&error) }
                <button onclick={toggle_history} aria-expanded={history_open.to_string()} aria-controls="file-history">
                    { if *history_open { "Hide history" } else { "History" } }
                </button>
                <a class="button" download="" href={match loaded_version_id {
                    Some(v) => format!("/api/projects/{project_id}/files/{file_id}/versions/{v}/content"),
                    None => format!("/api/projects/{project_id}/files/{file_id}/content"),
                }}>{ "Download" }</a>
                if editor {
                    <button onclick={on_rename}>{ "Rename" }</button>
                    <label class="button primary file-picker">
                        { "Upload new version…" }
                        <input type="file" class="sr-only" onchange={on_new_version} />
                    </label>
                }
            </div>
            <div class="file-body">
                <div class="file-main">{ body }</div>
                <aside id="file-history" class="history" hidden={!*history_open} aria-label="Version history">
                    <h4>{ "Version history" }</h4>
                    { view(&versions, version_list) }
                </aside>
            </div>
        </div>
    }
}

// ---------------------------------------------------------------------------
// Version comparison
// ---------------------------------------------------------------------------

#[derive(Properties, PartialEq)]
pub struct CompareProps {
    pub project_id: Uuid,
    pub file_id: Uuid,
    pub old: Uuid,
    pub new: Uuid,
}

struct Comparison {
    path: String,
    old_version: i32,
    new_version: i32,
    model: Rc<unlinked_model::Model>,
    diff: Rc<ModelDiff>,
}

fn change_label(change: &BlockChange) -> (&'static str, String) {
    match change {
        BlockChange::Added => ("added", "added".into()),
        BlockChange::Removed => ("removed", "removed".into()),
        BlockChange::Modified(m) => {
            let mut parts = Vec::new();
            if let Some(old) = &m.renamed_from {
                parts.push(format!("renamed from {}", old.replace('\n', " ")));
            }
            if let Some(old) = &m.type_changed_from {
                parts.push(format!("type was {old}"));
            }
            for (name, old, new) in &m.parameters {
                parts.push(format!(
                    "{name}: {} → {}",
                    old.as_deref().unwrap_or("∅"),
                    new.as_deref().unwrap_or("∅")
                ));
            }
            if m.mask_changed {
                parts.push("mask changed".into());
            }
            if m.ports_changed {
                parts.push("ports changed".into());
            }
            if let Some((old, new)) = &m.library_changed {
                let show = |l: &Option<String>| l.as_deref().unwrap_or("none").replace('\n', " ");
                parts.push(format!("library link {} → {}", show(old), show(new)));
            }
            if m.subsystem_changed {
                parts.push("contents added or removed".into());
            }
            for (name, old, new) in &m.interface {
                parts.push(format!(
                    "bus port {name}: {} → {}",
                    old.as_deref().unwrap_or("∅"),
                    new.as_deref().unwrap_or("∅")
                ));
            }
            if m.appearance_changed {
                parts.push("appearance changed".into());
            }
            if m.moved {
                parts.push("moved".into());
            }
            if m.resized {
                parts.push("resized".into());
            }
            let class = if m.layout_only() { "moved" } else { "modified" };
            (class, parts.join("; "))
        }
    }
}

#[function_component(ComparePage)]
pub fn compare_page(props: &CompareProps) -> Html {
    let (project_id, file_id, old, new) = (props.project_id, props.file_id, props.old, props.new);
    let comparison = use_fetch(
        (project_id, file_id, old, new),
        move |(p, f, old, new)| async move {
            let info = api::file(p, f).await?;
            let versions = api::versions(p, f).await?;
            let number = |id: Uuid| {
                versions
                    .iter()
                    .find(|v| v.id == id)
                    .map_or(0, |v| v.version)
            };
            let import = |bytes: Vec<u8>| {
                unlinked_import::import(&info.path, &bytes).map_err(|e| ApiError {
                    status: 0,
                    message: format!("cannot import model: {e}"),
                })
            };
            let old_model = import(api::content(p, f, Some(old)).await?)?;
            let new_model = import(api::content(p, f, Some(new)).await?)?;
            let diff = unlinked_model::diff::diff(&old_model, &new_model);
            Ok(Comparison {
                path: info.path,
                old_version: number(old),
                new_version: number(new),
                model: Rc::new(new_model),
                diff: Rc::new(diff),
            })
        },
    );

    view(&comparison, |c: &Comparison| {
        let d = &c.diff;
        html! {
            <div class="page-fill">
                <div class="subbar file-bar">
                    <Link<Route> to={Route::File { project_id, file_id }}>{ "← File" }</Link<Route>>
                    <strong>{ &c.path }</strong>
                    <span class="muted">{ format!("v{} → v{}", c.old_version, c.new_version) }</span>
                    <span class="spacer" />
                    <span class="legend added">{ "added" }</span>
                    <span class="legend modified">{ "modified" }</span>
                    <span class="legend moved">{ "moved" }</span>
                    <span class="legend nested">{ "contains changes" }</span>
                </div>
                <div class="file-body">
                    <div class="file-main">
                        <DiagramView key={format!("{old}-{new}")} model={c.model.clone()} diff={Some(c.diff.clone())} />
                    </div>
                    <aside class="history changes">
                        <h4>{ "Changes" }</h4>
                        if d.is_empty() {
                            <p class="muted">{ "No structural changes." }</p>
                        }
                        <ul class="plain">
                            { for d.blocks.iter().map(|b| {
                                let (class, text) = change_label(&b.change);
                                let place = if b.system.is_empty() { String::new() } else { format!("{}/", b.system.join("/")) };
                                html! {
                                    <li class={classes!("change", class)}>
                                        <strong>{ format!("{place}{}", b.name.replace('\n', " ")) }</strong>
                                        <span class="muted">{ format!(" {}", b.block_type) }</span>
                                        <div>{ text }</div>
                                    </li>
                                }
                            }) }
                            { for d.charts.iter().map(|c| {
                                let (class, what) = match c.change {
                                    ChartChange::Added => ("added", "chart added".to_string()),
                                    ChartChange::Removed => ("removed", "chart removed".to_string()),
                                    ChartChange::Modified => ("modified", match &c.previous_name {
                                        Some(old) => format!("chart changed (was {})", old.replace('\n', " ")),
                                        None => "chart code, states, data or timing changed".to_string(),
                                    }),
                                };
                                html! {
                                    <li class={classes!("change", class)}>
                                        <strong>{ c.name.replace('\n', " ") }</strong>
                                        <span class="muted">{ " Stateflow / MATLAB Function" }</span>
                                        <div>{ what }</div>
                                    </li>
                                }
                            }) }
                            { for d.systems.iter().map(|s| html! {
                                <li class="change modified">
                                    <strong>{ if s.system.is_empty() { "root".to_string() } else { s.system.join("/") } }</strong>
                                    <div>{ format!("wiring: +{} −{} connections", s.connections_added.len(), s.connections_removed.len()) }</div>
                                </li>
                            }) }
                            { for d.config.iter().map(|c| ("Configuration", c)).chain(d.workspace.iter().map(|c| ("Workspace", c))).map(|(section, (k, a, b))| html! {
                                <li class="change modified">
                                    <strong>{ section }</strong>
                                    <div>{ format!("{k}: {} → {}", a.as_deref().unwrap_or("∅"), b.as_deref().unwrap_or("∅")) }</div>
                                </li>
                            }) }
                        </ul>
                    </aside>
                </div>
            </div>
        }
    })
}
