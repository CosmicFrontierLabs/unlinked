//! Library-sourced blocks lowered like native ones.
//!
//! Simulink files record blocks dragged from the standard library as
//! `Reference` blocks carrying a `SourceBlock` path and the mask parameter
//! values of that library block (`slope`, `rep_seq_t`, ...). This registry
//! maps the supported source paths to the synthetic block types the rest of
//! the compiler lowers; every other link or mask keeps rejecting explicitly.
//! The catalog descriptors for these blocks name the same paths, so the
//! palette, the dialog and the simulator agree (checked in `tests/sources.rs`).
use unlinked_model::{catalog, Block};

/// Supported `SourceBlock` paths (canonical spacing) and their lowering types.
pub(crate) const LIBRARY_SOURCES: &[(&str, &str)] = &[
    ("simulink/Sources/Ramp", "Ramp"),
    ("simulink/Sources/Chirp Signal", "ChirpSignal"),
    ("simulink/Sources/Repeating Sequence", "RepeatingSequence"),
];

/// Lowering type of a registered library block: a `Reference` block whose
/// `SourceBlock` is in the registry and which carries no mask of its own.
pub(crate) fn lowered_type(block: &Block) -> Option<&'static str> {
    if block.block_type != "Reference" || block.mask.is_some() || block.subsystem.is_some() {
        return None;
    }
    let source = catalog::canonical_source(block.library_source.as_deref()?);
    LIBRARY_SOURCES
        .iter()
        .find(|(path, _)| *path == source)
        .map(|(_, kind)| *kind)
}

/// Rewrite a registered library block as its lowering type, dropping the
/// link so the native checks apply, and say whether it did. Other blocks
/// are left untouched. The lowering types are not block types Simulink
/// writes, so resolving a link is the only way a block acquires one.
pub(crate) fn resolve(block: &mut Block) -> bool {
    match lowered_type(block) {
        Some(kind) => {
            block.block_type = kind.into();
            block.library_source = None;
            true
        }
        None => false,
    }
}
