//! Portable typed Cargo projects. Generation does not invoke a compiler or execute MATLAB.
use crate::Error;

/// A generated program and its dependency manifest. The helper crate is vendored
/// by [`Self::files`], so extracting a project does not depend on an Unlinked checkout.
#[derive(Clone, Debug)]
pub struct GeneratedProject {
    pub source: String,
    pub manifest: String,
    pub library: bool,
}

pub fn generate_project(source: &str, library: bool) -> Result<GeneratedProject, Error> {
    Ok(GeneratedProject {
        source: crate::transpile_typed(source, library)?,
        manifest: String::from(concat!(
            "[package]\nname = \"generated_matlab\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n",
            "# Isolated from any enclosing Cargo workspace.\n[workspace]\n\n",
            "[dependencies]\nndarray = \"=0.17.2\"\nnalgebra = \"=0.35.0\"\n",
            "unlinked-matlab-rt = { path = \"matlab-rt\" }\n"
        )),
        library,
    })
}
impl GeneratedProject {
    /// Relative paths and UTF-8 contents of a complete Cargo project.
    pub fn files(&self) -> Vec<(String, String)> {
        vec![
            ("Cargo.toml".into(), self.manifest.clone()),
            (
                "matlab-rt/LICENSE".into(),
                include_str!("../../../LICENSE").into(),
            ),
            (
                (if self.library {
                    "src/lib.rs"
                } else {
                    "src/main.rs"
                })
                .into(),
                self.source.clone(),
            ),
            (
                "matlab-rt/Cargo.toml".into(),
                include_str!("../../unlinked-matlab-rt/Cargo.toml").into(),
            ),
            (
                "matlab-rt/src/lib.rs".into(),
                include_str!("../../unlinked-matlab-rt/src/lib.rs").into(),
            ),
            (
                "matlab-rt/src/compat.rs".into(),
                include_str!("../../unlinked-matlab-rt/src/compat.rs").into(),
            ),
            (
                "README.md".into(),
                format!(
                    "# Generated MATLAB {}\n\nGenerated code uses ndarray and nalgebra. The MATLAB semantics helper is vendored in matlab-rt/.\n\n{}\n\nGenerated standalone code is intended for trusted execution. It does not have the interpreter's statement budgets or cancellation hooks. Array helpers retain shape/index/allocation validation. The current MATLAB semantics subset is real two-dimensional arrays; ArrayD leaves room for future N-D support.\n",
                    if self.library { "library" } else { "program" },
                    if self.library {
                        "Build with `cargo build`. Public functions are in src/lib.rs."
                    } else {
                        "Build with `cargo build`; run with `cargo run`."
                    },
                ),
            ),
        ]
    }
}
