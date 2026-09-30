//! Organization, project and file pages backed by the server API.

use crate::api::{self, ApiError};
use crate::diagram::DiagramView;
use crate::fetch::{use_fetch, use_reload, view, Fetch, Reload};
use crate::Route;
use chrono::{DateTime, Utc};
use shared::{
    CreateProjectRequest, DefaultProjectRole, FileInfo, FileVersionInfo, OrgRole, Organization,
    Project, ProjectRole, UserInfo,
};
use std::rc::Rc;
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
                                        <select onchange={on_role}>
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
                <select onchange={set_default}>
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
                    <select onchange={set_role}>
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

    let folder = use_state(String::new);
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
                    if let Err(e) = api::upload(id, &path, &msg, &bytes).await {
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

    let file_rows = {
        let (reload, error) = (reload.clone(), error.clone());
        move |files: &Vec<FileInfo>| -> Html {
            if files.is_empty() {
                return html! { <p class="muted">{ "No files yet." }</p> };
            }
            let mut sorted: Vec<&FileInfo> = files.iter().collect();
            sorted.sort_by(|a, b| a.path.cmp(&b.path));
            html! {
                <table>
                    <tr><th>{ "Path" }</th><th>{ "Version" }</th><th>{ "Size" }</th><th>{ "Updated by" }</th><th>{ "Updated" }</th><th /></tr>
                    { for sorted.into_iter().map(|f| {
                        let file_id = f.id;
                        let on_delete = {
                            let (reload, error) = (reload.clone(), error.clone());
                            let path = f.path.clone();
                            Callback::from(move |_: MouseEvent| {
                                if gloo_utils::window().confirm_with_message(&format!("Delete {path}?")).unwrap_or(false) {
                                    mutate(api::delete_file(id, file_id), reload.clone(), error.clone());
                                }
                            })
                        };
                        html! {
                            <tr>
                                <td><Link<Route> to={Route::File { project_id: id, file_id }}>{ &f.path }</Link<Route>></td>
                                <td>{ format!("v{}", f.latest.version) }</td>
                                <td class="muted">{ size(f.latest.size_bytes) }</td>
                                <td>{ who(&f.latest.author) }</td>
                                <td class="muted">{ when(&f.latest.created_at) }</td>
                                <td>
                                    <a class="button" href={format!("/api/projects/{id}/files/{file_id}/content")}>{ "Download" }</a>
                                    if editor { <button onclick={on_delete}>{ "Delete" }</button> }
                                </td>
                            </tr>
                        }
                    }) }
                </table>
            }
        }
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
                                        <select onchange={on_role}>
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
                    { text_input(&folder, "Folder (optional)") }
                    { text_input(&message, "Change message") }
                    <label class="button primary">
                        { if *uploading { "Uploading…" } else { "Upload files…" } }
                        <input type="file" multiple=true hidden=true onchange={on_upload} />
                    </label>
                </div>
            }
            { view(&files, file_rows) }
            <h2>{ "Members" }</h2>
            <p class="muted">{ "Explicit project members. Organization members also get the project's default access." }</p>
            { view(&members, member_rows) }
            if owner {
                <form class="inline-form" onsubmit={add_member}>
                    { text_input(&email, "email@example.com") }
                    <select onchange={set_new_role}>
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
    Model(Rc<unlinked_model::Model>),
    Text(String),
    Binary,
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
    let project = use_fetch(project_id, api::project);
    let file = use_fetch((project_id, file_id, reload.0), move |(p, f, _)| {
        api::file(p, f)
    });
    let versions = use_fetch((project_id, file_id, reload.0), move |(p, f, _)| {
        api::versions(p, f)
    });
    let content = use_fetch(
        (project_id, file_id, version_id, reload.0),
        move |(p, f, v, _)| async move {
            let info = api::file(p, f).await?;
            let bytes = api::content(p, f, v).await?;
            Ok(if is_model(&info.path) {
                match unlinked_import::import(&info.path, &bytes) {
                    Ok(m) => Content::Model(Rc::new(m)),
                    Err(e) => {
                        return Err(ApiError {
                            status: 0,
                            message: format!("cannot import model: {e}"),
                        })
                    }
                }
            } else {
                match String::from_utf8(bytes) {
                    Ok(s) => Content::Text(s),
                    Err(_) => Content::Binary,
                }
            })
        },
    );
    let editor = matches!(&*project, Fetch::Ready(p) if p.my_role >= ProjectRole::Editor);

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
                    api::upload(project_id, &path, "", &bytes).await.map(|_| ())
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

    let version_list = move |vs: &Vec<FileVersionInfo>| -> Html {
        let current = version_id.or_else(|| vs.first().map(|v| v.id));
        html! {
            <ul class="versions">
                { for vs.iter().map(|v| {
                    let active = Some(v.id) == current;
                    let to = if Some(v.id) == vs.first().map(|l| l.id) {
                        Route::File { project_id, file_id }
                    } else {
                        Route::FileVersion { project_id, file_id, version_id: v.id }
                    };
                    html! {
                        <li class={classes!(active.then_some("active"))}>
                            <Link<Route> {to}>{ format!("v{}", v.version) }</Link<Route>>
                            <span class="muted">{ format!(" {} · {}", when(&v.created_at), who(&v.author)) }</span>
                            if !v.message.is_empty() { <div>{ &v.message }</div> }
                        </li>
                    }
                }) }
            </ul>
        }
    };

    let body = view(&content, |c: &Content| match c {
        // Keyed by model identity so a new version remounts the viewer with
        // fresh navigation state instead of keeping a stale subsystem path.
        Content::Model(m) => html! {
            <DiagramView key={format!("{:p}", Rc::as_ptr(m))} model={m.clone()} />
        },
        Content::Text(t) => html! { <pre class="source">{ t }</pre> },
        Content::Binary => html! { <p class="muted">{ "Binary file; use Download." }</p> },
    });

    html! {
        <div class="page-fill">
            <div class="subbar file-bar">
                <Link<Route> to={Route::Project { project_id }}>{ "← Project" }</Link<Route>>
                { view(&file, |f: &FileInfo| html! { <strong>{ &f.path }</strong> }) }
                <span class="spacer" />
                { error_line(&error) }
                <a class="button" href={match version_id {
                    Some(v) => format!("/api/projects/{project_id}/files/{file_id}/versions/{v}/content"),
                    None => format!("/api/projects/{project_id}/files/{file_id}/content"),
                }}>{ "Download" }</a>
                if editor {
                    <button onclick={on_rename}>{ "Rename" }</button>
                    <label class="button primary">
                        { "Upload new version…" }
                        <input type="file" hidden=true onchange={on_new_version} />
                    </label>
                }
            </div>
            <div class="file-body">
                <div class="file-main">{ body }</div>
                <aside class="history">
                    <h4>{ "History" }</h4>
                    { view(&versions, version_list) }
                </aside>
            </div>
        </div>
    }
}
