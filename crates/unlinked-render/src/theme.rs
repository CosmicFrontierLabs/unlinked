//! Color themes. `Light` matches Simulink's own look; `Dark` uses the Tokyo
//! Night palette. Block colors set explicitly in the model are kept, except
//! that the default black/white are mapped onto the theme.

use crate::color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Light,
    Dark,
}

pub struct Palette {
    pub canvas: &'static str,
    pub fg: &'static str,
    pub block_fill: &'static str,
    pub line: &'static str,
    pub text: &'static str,
    pub muted: &'static str,
    pub error: &'static str,
    pub shadow: &'static str,
}

const LIGHT: Palette = Palette {
    canvas: "#ffffff",
    fg: "#000000",
    block_fill: "#ffffff",
    line: "#000000",
    text: "#000000",
    muted: "#555555",
    error: "#ff0000",
    shadow: "#9a9a9a",
};

const DARK: Palette = Palette {
    canvas: "#1a1b26",
    fg: "#c0caf5",
    block_fill: "#24283b",
    line: "#7aa2f7",
    text: "#c0caf5",
    muted: "#565f89",
    error: "#f7768e",
    shadow: "#15161e",
};

impl Theme {
    pub fn palette(&self) -> &'static Palette {
        match self {
            Theme::Light => &LIGHT,
            Theme::Dark => &DARK,
        }
    }
}

impl Palette {
    /// Block outline/icon color: explicit model colors win, default black
    /// follows the theme.
    pub fn block_fg(&self, value: Option<&str>) -> String {
        match value.map(str::trim) {
            None | Some("black") | Some("automatic") => self.fg.to_string(),
            Some(v) => color::resolve(Some(v), self.fg),
        }
    }

    pub fn block_bg(&self, value: Option<&str>) -> String {
        match value.map(str::trim) {
            None | Some("white") | Some("automatic") => self.block_fill.to_string(),
            Some(v) => color::resolve(Some(v), self.block_fill),
        }
    }
}
