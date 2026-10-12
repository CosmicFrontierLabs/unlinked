//! Toolbar icons: small stroked SVGs drawn on a 16-unit grid in the current
//! text color, so they follow button states and themes.

use yew::prelude::*;

fn icon(body: Html) -> Html {
    html! {
        <svg class="icon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"
            fill="none" stroke="currentColor" stroke-width="1.5"
            stroke-linecap="round" stroke-linejoin="round">
            { body }
        </svg>
    }
}

pub fn undo() -> Html {
    icon(html! { <><path d="M5.5 3 2.5 6l3 3" /><path d="M2.5 6h7a4 4 0 0 1 0 8H7" /></> })
}

pub fn redo() -> Html {
    icon(html! { <><path d="m10.5 3 3 3-3 3" /><path d="M13.5 6h-7a4 4 0 0 0 0 8H9" /></> })
}

pub fn rotate() -> Html {
    icon(html! { <><path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9" /><path d="M13.5 2.5v3h-3" /></> })
}

pub fn flip() -> Html {
    icon(
        html! { <><path d="M8 1.5v13" stroke-dasharray="1.5 1.5" /><path d="M5.5 4 2 12h3.5z" /><path d="M10.5 4 14 12h-3.5z" /></> },
    )
}

pub fn group() -> Html {
    icon(
        html! { <><rect x="1.5" y="1.5" width="13" height="13" rx="1.5" stroke-dasharray="2 1.5" /><rect x="4" y="4" width="3" height="3" /><rect x="9" y="9" width="3" height="3" /><path d="M7 5.5h1.5v5H9" /></> },
    )
}

pub fn expand() -> Html {
    icon(
        html! { <><rect x="5" y="5" width="6" height="6" rx="1" /><path d="M1.5 5V1.5H5M11 1.5h3.5V5M14.5 11v3.5H11M5 14.5H1.5V11" /></> },
    )
}

pub fn copy() -> Html {
    icon(
        html! { <><rect x="5" y="5" width="9" height="9" rx="1.5" /><path d="M11 5V3a1.5 1.5 0 0 0-1.5-1.5h-6A1.5 1.5 0 0 0 2 3v6.5A1.5 1.5 0 0 0 3.5 11H5" /></> },
    )
}

pub fn paste() -> Html {
    icon(
        html! { <><rect x="3" y="2.5" width="10" height="12" rx="1.5" /><rect x="5.5" y="1.5" width="5" height="2.5" rx="0.75" /><path d="M5.5 8h5M5.5 11h3" /></> },
    )
}

pub fn delete() -> Html {
    icon(
        html! { <><path d="M2.5 4h11M6 4V2.5h4V4" /><path d="M3.75 4l.75 10h7l.75-10" /><path d="M6.5 7v4.5M9.5 7v4.5" /></> },
    )
}

pub fn zoom_in() -> Html {
    icon(html! { <><circle cx="7" cy="7" r="5" /><path d="m14.5 14.5-4-4M5 7h4M7 5v4" /></> })
}

pub fn zoom_out() -> Html {
    icon(html! { <><circle cx="7" cy="7" r="5" /><path d="m14.5 14.5-4-4M5 7h4" /></> })
}

pub fn fit() -> Html {
    icon(html! { <path d="M1.5 5V1.5H5M11 1.5h3.5V5M14.5 11v3.5H11M5 14.5H1.5V11M5 8h6M8 5v6" /> })
}

pub fn sun() -> Html {
    icon(
        html! { <><circle cx="8" cy="8" r="3" /><path d="M8 1v1.5M8 13.5V15M1 8h1.5M13.5 8H15M3 3l1 1M12 12l1 1M3 13l1-1M12 4l1-1" /></> },
    )
}

pub fn moon() -> Html {
    icon(html! { <path d="M13.5 9.5A6 6 0 0 1 6.5 2.5a6 6 0 1 0 7 7z" /> })
}

pub fn settings() -> Html {
    icon(
        html! { <><path d="M2 4h7M12 4h2M2 12h2M7 12h7M2 8h3M8 8h6" /><circle cx="10.5" cy="4" r="1.5" /><circle cx="5.5" cy="12" r="1.5" /><circle cx="6.5" cy="8" r="1.5" /></> },
    )
}

pub fn close() -> Html {
    icon(html! { <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" /> })
}

pub fn search() -> Html {
    icon(html! { <><circle cx="7" cy="7" r="5" /><path d="m14.5 14.5-4-4" /></> })
}

pub fn annotation() -> Html {
    icon(html! { <path d="M3 3.5h10M8 3.5v10M6 13.5h4" /> })
}

pub fn help() -> Html {
    icon(
        html! { <><circle cx="8" cy="8" r="6.5" /><path d="M6 6.25a2 2 0 1 1 2.75 1.85c-.5.2-.75.6-.75 1.15v.5" /><path d="M8 11.75v.25" /></> },
    )
}
