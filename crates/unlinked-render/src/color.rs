//! Simulink color values: named colors or `[r, g, b]` with components in 0..1.

pub fn resolve(value: Option<&str>, default: &str) -> String {
    value.and_then(parse).unwrap_or_else(|| default.to_string())
}

fn parse(value: &str) -> Option<String> {
    let v = value.trim();
    let named = match v.to_ascii_lowercase().as_str() {
        "black" => "#000000",
        "white" => "#ffffff",
        "red" => "#ff0000",
        "green" => "#00ff00",
        "blue" => "#0000ff",
        "cyan" => "#00ffff",
        "magenta" => "#ff00ff",
        "yellow" => "#ffff00",
        "gray" => "#808080",
        "lightblue" => "#add8e6",
        "orange" => "#ffa500",
        "darkgreen" => "#006400",
        "automatic" => return None,
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    if v.starts_with('[') {
        let parts: Vec<f64> = v
            .trim_matches(|c| c == '[' || c == ']')
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        if parts.len() >= 3 {
            let c = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            return Some(format!(
                "#{:02x}{:02x}{:02x}",
                c(parts[0]),
                c(parts[1]),
                c(parts[2])
            ));
        }
    }
    None
}

/// Relative luminance (0..1) of a `#rrggbb` color.
pub fn luminance(hex: &str) -> Option<f64> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let ch = |i: usize| {
        u8::from_str_radix(&h[i..i + 2], 16)
            .ok()
            .map(|v| v as f64 / 255.0)
    };
    Some(0.2126 * ch(0)? + 0.7152 * ch(2)? + 0.0722 * ch(4)?)
}

/// `ink` unless it would be hard to read on `background`, in which case the
/// better of `dark`/`light`.
pub fn readable(ink: &str, background: &str, dark: &str, light: &str) -> String {
    match (luminance(ink), luminance(background)) {
        (Some(i), Some(b)) if (i - b).abs() < 0.4 => if b > 0.5 { dark } else { light }.to_string(),
        _ => ink.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_swaps_unreadable_ink() {
        assert_eq!(
            readable("#c0caf5", "#ffa500", "#1a1b26", "#c0caf5"),
            "#1a1b26"
        );
        assert_eq!(
            readable("#c0caf5", "#24283b", "#1a1b26", "#c0caf5"),
            "#c0caf5"
        );
        assert_eq!(
            readable("#000000", "#ffffff", "#000000", "#ffffff"),
            "#000000"
        );
    }

    #[test]
    fn named_and_rgb() {
        assert_eq!(resolve(Some("red"), "#000"), "#ff0000");
        assert_eq!(
            resolve(Some("[0.753000, 0.851000, 1.000000]"), "#000"),
            "#c0d9ff"
        );
        assert_eq!(resolve(Some("bogus"), "#123"), "#123");
        assert_eq!(resolve(None, "#123"), "#123");
    }
}
