//! Measurements use recorded samples, with no implied interpolation.
use super::*;

pub(super) const COLORS: [&str; 8] = [
    "#1f77b4", "#d95f02", "#2ca02c", "#9467bd", "#d62728", "#17a2b8", "#8c564b", "#bc3c8e",
];
pub(super) fn color(index: usize) -> &'static str {
    COLORS[index % COLORS.len()]
}

fn nearest(time: &[f64], at: f64) -> Option<usize> {
    if time.is_empty() || !at.is_finite() {
        return None;
    }
    let right = time.partition_point(|t| *t < at);
    if right == 0 {
        return Some(0);
    }
    if right == time.len() {
        return Some(right - 1);
    }
    Some(if at - time[right - 1] <= time[right] - at {
        right - 1
    } else {
        right
    })
}

#[derive(Debug, PartialEq)]
struct Statistics {
    count: usize,
    min: f64,
    max: f64,
    mean: f64,
    rms: f64,
}

fn statistics(values: &[f64]) -> Option<Statistics> {
    let mut count = 0;
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    for &v in values.iter().filter(|v| v.is_finite()) {
        count += 1;
        min = min.min(v);
        max = max.max(v);
    }
    if count == 0 {
        return None;
    }
    // Scale before summing: neither the mean nor the RMS should overflow
    // merely because individual finite samples approach f64::MAX.
    let scale = min.abs().max(max.abs());
    let (mut sum, mut squares) = (0.0, 0.0);
    if scale > 0.0 {
        for &v in values.iter().filter(|v| v.is_finite()) {
            let x = v / scale;
            sum += x;
            squares += x * x;
        }
    }
    Some(Statistics {
        count,
        min,
        max,
        mean: (sum / count as f64).clamp(-1.0, 1.0) * scale,
        rms: (squares / count as f64).min(1.0).sqrt() * scale,
    })
}

fn number(v: f64) -> String {
    if !v.is_finite() {
        return "—".into();
    }
    if v == 0.0 {
        return "0".into();
    }
    // Six significant digits keep small magnitudes useful without flooding rows.
    let exponent = v.abs().log10().floor() as i32;
    if !(-3..6).contains(&exponent) {
        return format!("{v:.5e}");
    }
    let decimals = (5 - exponent).max(0) as usize;
    let formatted = format!("{v:.decimals$}");
    if decimals == 0 {
        formatted
    } else {
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

#[derive(Properties)]
pub(super) struct InspectorProps {
    pub controller: Rc<RefCell<Controller>>,
    pub samples: usize,
    pub selected: Vec<usize>,
    pub on_cursor: Callback<usize>,
}
impl PartialEq for InspectorProps {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.controller, &other.controller)
            && self.samples == other.samples
            && self.selected == other.selected
            && self.on_cursor == other.on_cursor
    }
}

#[function_component(Measurements)]
pub(super) fn measurements(props: &InspectorProps) -> Html {
    static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let instance = use_state(|| NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let content_id = format!("measurement-content-{}", *instance);
    let error_id = format!("measurement-cursor-error-{}", *instance);
    let expanded = use_state(|| false);
    let intervals = use_state(|| false);
    let a = use_state(String::new);
    let b = use_state(String::new);
    let toggle = {
        let expanded = expanded.clone();
        Callback::from(move |_| expanded.set(!*expanded))
    };
    let c = props.controller.borrow();
    let cursor = |draft: &str, fallback: Option<&f64>| {
        if draft.trim().is_empty() {
            fallback.copied()
        } else {
            draft.trim().parse::<f64>().ok().filter(|v| v.is_finite())
        }
    };
    let ia = cursor(&a, c.time.first()).and_then(|v| nearest(&c.time, v));
    let ib = cursor(&b, c.time.last()).and_then(|v| nearest(&c.time, v));
    {
        let on_cursor = props.on_cursor.clone();
        use_effect_with(ib, move |index| {
            if let Some(index) = index {
                on_cursor.emit(*index);
            }
        });
    }
    let choose_sample = {
        let (controller, b) = (props.controller.clone(), b.clone());
        Callback::from(move |index: usize| {
            let value = controller
                .borrow()
                .time
                .get(index)
                .map(|time| format!("{time:?}"));
            if let Some(value) = value {
                b.set(value);
            }
        })
    };
    let move_sample = |index: usize| {
        let choose_sample = choose_sample.clone();
        Callback::from(move |_: MouseEvent| choose_sample.emit(index))
    };
    let scrub = choose_sample.reform(|e: InputEvent| {
        e.target_unchecked_into::<HtmlInputElement>()
            .value()
            .parse::<usize>()
            .unwrap_or(0)
    });
    let follow_latest = {
        let b = b.clone();
        Callback::from(move |_: MouseEvent| b.set(String::new()))
    };
    let last_sample = c.time.len().saturating_sub(1);
    let sample = ib.unwrap_or(last_sample);
    let input = |name: &'static str,
                 draft: &UseStateHandle<String>,
                 default: Option<&f64>,
                 valid: bool| {
        let state = draft.clone();
        let oninput = Callback::from(move |e: InputEvent| {
            state.set(e.target_unchecked_into::<HtmlInputElement>().value())
        });
        html! { <label>{ name }<input type="text" inputmode="decimal" aria-label={name}
        value={(**draft).clone()} placeholder={default.map(|v| number(*v))}
        aria-invalid={(!valid).to_string()} aria-describedby={(!valid).then_some(error_id.clone())} {oninput} /></label> }
    };
    let mode = |stats: bool, name: &'static str| {
        let intervals = intervals.clone();
        let onclick = {
            let intervals = intervals.clone();
            Callback::from(move |_| intervals.set(stats))
        };
        html! { <button aria-pressed={(*intervals == stats).to_string()} class={classes!((*intervals == stats).then_some("active"))} {onclick}>{ name }</button> }
    };
    html! {
        <section class="signal-measurements" aria-label="Signal measurements">
            <button class="measurements-toggle" onclick={toggle} aria-expanded={expanded.to_string()} aria-controls={content_id.clone()}>
                { if *expanded { "▾ Measurements" } else { "▸ Measurements" } }
            </button>
            if *expanded {
                <div id={content_id}>
                <div class="measurement-controls">
                    { input("Cursor A (s)", &a, c.time.first(), ia.is_some()) }
                    { input("Cursor B (s)", &b, c.time.last(), ib.is_some()) }
                    <div class="measurement-modes">{ mode(false, "Cursor values") }{ mode(true, "Interval statistics") }</div>
                </div>
                <div class="sample-scrubber">
                    <span>{"Cursor B"}</span>
                    <button onclick={move_sample(sample.saturating_sub(1))} disabled={sample == 0} aria-label="Previous recorded sample" title="Previous recorded sample">{"‹"}</button>
                    <input type="range" min="0" max={last_sample.to_string()} step="1" value={sample.to_string()} oninput={scrub}
                        aria-label="Recorded sample" aria-valuetext={c.time.get(sample).map(|time| format!("Sample {} of {} at {time:?} seconds", sample + 1, c.time.len()))} />
                    <button onclick={move_sample((sample + 1).min(last_sample))} disabled={sample == last_sample} aria-label="Next recorded sample" title="Next recorded sample">{"›"}</button>
                    <button onclick={follow_latest} aria-pressed={b.trim().is_empty().to_string()} title="Keep cursor B at the latest recorded sample">{"Latest"}</button>
                    <span class="sample-index">{format!("{} / {}", sample + 1, c.time.len())}</span>
                </div>
                if let (Some(ia), Some(ib)) = (ia, ib) {
                    <p class="measurement-note">{ format!("Nearest recorded samples: A = {} s · B = {} s · Δt = {} s", number(c.time[ia]), number(c.time[ib]), number(c.time[ib] - c.time[ia])) }</p>
                    if props.selected.is_empty() { <p class="muted">{ "Select signals to measure." }</p> }
                    else {
                        <div class="measurement-table-scroll"><table class="measurement-table">
                            <thead><tr><th scope="col">{ "Signal" }</th>
                            if *intervals { <><th scope="col">{ "Min" }</th><th scope="col">{ "Max" }</th><th scope="col">{ "Mean" }</th><th scope="col">{ "RMS" }</th><th scope="col">{ "Samples" }</th></> }
                            else { <><th scope="col">{ "A" }</th><th scope="col">{ "B" }</th><th scope="col">{ "B − A" }</th></> }
                            </tr></thead>
                            <tbody>{ for props.selected.iter().filter_map(|&i| {
                                let signal = c.signals.get(i)?;
                                let values = c.columns.get(i)?;
                                let cells = if *intervals {
                                    let lo = ia.min(ib).min(values.len());
                                    let hi = (ia.max(ib) + 1).min(values.len());
                                    match statistics(&values[lo..hi]) {
                                        Some(s) => html! { <><td>{number(s.min)}</td><td>{number(s.max)}</td><td>{number(s.mean)}</td><td>{number(s.rms)}</td><td>{s.count}</td></> },
                                        None => html! { <td colspan="5">{ "No finite samples" }</td> },
                                    }
                                } else {
                                    let va = values.get(ia).copied().unwrap_or(f64::NAN);
                                    let vb = values.get(ib).copied().unwrap_or(f64::NAN);
                                    html! { <><td>{number(va)}</td><td>{number(vb)}</td><td>{number(vb-va)}</td></> }
                                };
                                Some(html! { <tr><th scope="row" title={signal.name.clone()}><span class="signal-swatch" style={format!("background:{}", color(i))} />{label(&signal.name)}</th>{cells}</tr> })
                            }) }</tbody>
                        </table></div>
                    }
                    if *intervals { <p class="muted measurement-note">{ "Statistics include both endpoint samples, weight each sample equally, and omit nonfinite values." }</p> }
                } else { <p id={error_id} class="field-error" role="alert">{ "Enter finite cursor times to inspect recorded samples." }</p> }
                </div>
            }
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measurements_use_significant_digits_without_negative_zero() {
        assert_eq!(number(-0.0), "0");
        assert_eq!(number(0.00012345), "1.23450e-4");
        assert_eq!(number(12345.67), "12345.7");
        assert_eq!(number(1.5), "1.5");
    }
    #[test]
    fn nearest_samples_clamp_and_break_ties_to_left() {
        let t = [1.0, 2.0, 5.0];
        assert_eq!(nearest(&t, -10.0), Some(0));
        assert_eq!(nearest(&t, 10.0), Some(2));
        assert_eq!(nearest(&t, 3.5), Some(1));
        assert_eq!(nearest(&t, 4.0), Some(2));
        assert_eq!(nearest(&[], 0.0), None);
        assert_eq!(nearest(&t, f64::NAN), None);
    }
    #[test]
    fn interval_statistics_and_extreme_finite_samples() {
        let s = statistics(&[-2.0, 0.0, 2.0, f64::NAN]).unwrap();
        assert_eq!((s.count, s.min, s.max, s.mean), (3, -2.0, 2.0, 0.0));
        assert!((s.rms - (8.0_f64 / 3.0).sqrt()).abs() < 1e-12);
        let s = statistics(&[f64::MAX, f64::MAX]).unwrap();
        assert_eq!((s.mean, s.rms), (f64::MAX, f64::MAX));
        assert_eq!(statistics(&[0.0]).unwrap().rms, 0.0);
        assert_eq!(statistics(&[f64::INFINITY]), None);
    }
}
