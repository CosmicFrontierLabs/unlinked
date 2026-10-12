//! Folder-aware project files with global search and bounded rendering.
use super::{size, when, who};
use crate::Route;
use shared::FileInfo;
use std::collections::BTreeMap;
use uuid::Uuid;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_router::prelude::Link;

const PAGE_SIZE: usize = 100;
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    All,
    Model,
    Script,
    Other,
}
impl Kind {
    fn of(path: &str) -> Self {
        match path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "mdl" | "slx" => Self::Model,
            "m" => Self::Script,
            _ => Self::Other,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::All => "All files",
            Self::Model => "Models",
            Self::Script => "MATLAB scripts",
            Self::Other => "Other files",
        }
    }
    fn value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Model => "model",
            Self::Script => "script",
            Self::Other => "other",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "model" => Self::Model,
            "script" => Self::Script,
            "other" => Self::Other,
            _ => Self::All,
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Name,
    Updated,
    Size,
}
#[derive(Debug)]
enum Entry<'a> {
    Folder {
        name: String,
        path: String,
        count: usize,
    },
    File(&'a FileInfo),
}
fn entries<'a>(
    files: &'a [FileInfo],
    folder: &str,
    query: &str,
    kind: Kind,
    sort: Sort,
) -> Vec<Entry<'a>> {
    let query = query.trim().to_lowercase();
    let folder = folder.trim().trim_matches('/');
    let prefix = if folder.is_empty() {
        String::new()
    } else {
        format!("{folder}/")
    };
    let mut folders = BTreeMap::<String, usize>::new();
    let mut visible = Vec::new();
    for f in files {
        if kind != Kind::All && Kind::of(&f.path) != kind {
            continue;
        }
        if !query.is_empty() {
            if f.path.to_lowercase().contains(&query) {
                visible.push(f);
            }
        } else if let Some(relative) = f.path.strip_prefix(&prefix) {
            if let Some((name, _)) = relative.split_once('/') {
                *folders.entry(name.into()).or_default() += 1;
            } else {
                visible.push(f);
            }
        }
    }
    match sort {
        Sort::Name => visible.sort_by_cached_key(|f| f.path.to_lowercase()),
        Sort::Updated => visible.sort_by(|a, b| {
            b.latest
                .created_at
                .cmp(&a.latest.created_at)
                .then(a.path.cmp(&b.path))
        }),
        Sort::Size => visible.sort_by(|a, b| {
            b.latest
                .size_bytes
                .cmp(&a.latest.size_bytes)
                .then(a.path.cmp(&b.path))
        }),
    }
    folders
        .into_iter()
        .map(|(name, count)| Entry::Folder {
            path: format!("{prefix}{name}"),
            name,
            count,
        })
        .chain(visible.into_iter().map(Entry::File))
        .collect()
}

#[derive(Properties, PartialEq)]
pub(super) struct BrowserProps {
    pub files: Vec<FileInfo>,
    pub folder: String,
    pub on_folder: Callback<String>,
    pub can_delete: bool,
    pub on_delete: Callback<(Uuid, String)>,
}
#[function_component(FileBrowser)]
pub(super) fn file_browser(props: &BrowserProps) -> Html {
    let query = use_state(String::new);
    let kind = use_state(|| Kind::All);
    let sort = use_state(|| Sort::Name);
    let page = use_state(|| 0usize);
    let navigate = |path: String| {
        let (on_folder, query, page) = (props.on_folder.clone(), query.clone(), page.clone());
        Callback::from(move |_: MouseEvent| {
            on_folder.emit(path.clone());
            query.set(String::new());
            page.set(0);
        })
    };
    let search = {
        let (query, page) = (query.clone(), page.clone());
        Callback::from(move |e: InputEvent| {
            query.set(e.target_unchecked_into::<HtmlInputElement>().value());
            page.set(0);
        })
    };
    let filter = {
        let (kind, page) = (kind.clone(), page.clone());
        Callback::from(move |e: Event| {
            kind.set(Kind::parse(
                &e.target_unchecked_into::<HtmlSelectElement>().value(),
            ));
            page.set(0);
        })
    };
    let sort_by = {
        let (sort, page) = (sort.clone(), page.clone());
        Callback::from(move |e: Event| {
            sort.set(
                match e
                    .target_unchecked_into::<HtmlSelectElement>()
                    .value()
                    .as_str()
                {
                    "updated" => Sort::Updated,
                    "size" => Sort::Size,
                    _ => Sort::Name,
                },
            );
            page.set(0);
        })
    };
    let folder = props.folder.trim().trim_matches('/');
    let rows = entries(&props.files, folder, &query, *kind, *sort);
    let pages = rows.len().div_ceil(PAGE_SIZE).max(1);
    let current = (*page).min(pages - 1);
    let global_search = !query.trim().is_empty();
    let crumbs: Vec<_> = folder.split('/').filter(|s| !s.is_empty()).collect();
    html! {
        <section class="project-files" aria-label="Project files">
            <div class="file-browser-tools">
                <input type="search" class="file-search" aria-label="Search project files" placeholder="Search all project files…" value={(*query).clone()} oninput={search} />
                <label>{ "Type" }<select aria-label="File type" onchange={filter}>{ for [Kind::All, Kind::Model, Kind::Script, Kind::Other].iter().map(|k| html! { <option value={k.value()} selected={*k == *kind}>{k.label()}</option> }) }</select></label>
                <label>{ "Sort" }<select aria-label="Sort files" onchange={sort_by}>
                    <option value="name" selected={*sort == Sort::Name}>{ "Name" }</option>
                    <option value="updated" selected={*sort == Sort::Updated}>{ "Recently updated" }</option>
                    <option value="size" selected={*sort == Sort::Size}>{ "Largest first" }</option>
                </select></label>
            </div>
            <nav class="folder-breadcrumbs" aria-label="File folders">
                <button onclick={navigate(String::new())} aria-current={folder.is_empty().then_some("page")}>{ "Project files" }</button>
                { for crumbs.iter().enumerate().map(|(i,name)| html! { <><span aria-hidden="true">{"/"}</span><button onclick={navigate(crumbs[..=i].join("/"))} aria-current={(i+1==crumbs.len()).then_some("page")}>{*name}</button></> }) }
                if global_search { <span class="file-search-note">{ "Searching all folders" }</span> }
            </nav>
            if rows.is_empty() {
                <div class="file-browser-empty">
                    <strong>{ if props.files.is_empty() { "Your project is ready for its first model" } else { "No matching files" } }</strong>
                    <p>{ if props.files.is_empty() { "Upload .slx or .mdl models, MATLAB scripts, and supporting files." } else { "Try a different search or file type, or return to Project files." } }</p>
                </div>
            } else {
                <div class="file-table-scroll"><table class="file-browser-table">
                    <thead><tr><th scope="col">{ "Name" }</th><th scope="col">{ "Version" }</th><th scope="col">{ "Size" }</th><th scope="col">{ "Updated" }</th><th scope="col"><span class="sr-only">{ "Actions" }</span></th></tr></thead>
                    <tbody>{ for rows.iter().skip(current*PAGE_SIZE).take(PAGE_SIZE).map(|entry| match entry {
                        Entry::Folder {name,path,count} => html! { <tr class="folder-row"><td><button class="folder-link" onclick={navigate(path.clone())}><span aria-hidden="true">{"▸ ▤"}</span>{name}</button></td><td colspan="3" class="muted">{format!("{count} file{}", if *count == 1 {""} else {"s"})}</td><td /></tr> },
                        Entry::File(f) => {
                            let shown = if global_search { f.path.as_str() } else { f.path.rsplit('/').next().unwrap_or(&f.path) };
                            let delete = {let (on_delete, id, path) = (props.on_delete.clone(), f.id, f.path.clone()); Callback::from(move |_| on_delete.emit((id,path.clone())))};
                            let badge = match Kind::of(&f.path) { Kind::Model => "MODEL", Kind::Script => "M", _ => "FILE" };
                            html! { <tr><td class="file-name-cell"><span class={classes!("file-kind", (Kind::of(&f.path)==Kind::Model).then_some("model"))}>{badge}</span>
                                <Link<Route> to={Route::File {project_id:f.project_id,file_id:f.id}}>{shown}</Link<Route>>
                                <span class="file-author muted">{who(&f.latest.author)}</span></td>
                                <td><span class="version-badge">{format!("v{}",f.latest.version)}</span></td><td class="muted">{size(f.latest.size_bytes)}</td>
                                <td class="muted" title={f.latest.created_at.to_rfc3339()}>{when(&f.latest.created_at)}</td>
                                <td class="file-row-actions"><a class="button" download="" href={format!("/api/projects/{}/files/{}/versions/{}/content",f.project_id,f.id,f.latest.id)} aria-label={format!("Download {}",f.path)}>{"Download"}</a>
                                if props.can_delete { <button onclick={delete} aria-label={format!("Delete {}",f.path)}>{"Delete"}</button> }</td></tr> }
                        }
                    }) }</tbody>
                </table></div>
                <div class="file-browser-footer"><span>{format!("{}–{} of {} entries",current*PAGE_SIZE+1,((current+1)*PAGE_SIZE).min(rows.len()),rows.len())}</span>
                if pages > 1 {
                    <button disabled={current==0} onclick={{let page=page.clone();Callback::from(move |_| page.set(current.saturating_sub(1)))}}>{"Previous"}</button>
                    <span>{format!("Page {} of {pages}",current+1)}</span>
                    <button disabled={current+1==pages} onclick={{let page=page.clone();Callback::from(move |_| page.set(current+1))}}>{"Next"}</button>
                }
                </div>
            }
        </section>
    }
}
