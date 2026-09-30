//! Imported real models must not acquire structural errors from palette rules.
use std::path::Path;
#[test]
fn catalog_validation_preserves_corpus() {
    let Some(dir) = std::env::var_os("UNLINKED_TEST_CASES") else {
        return;
    };
    let mut pending = vec![std::path::PathBuf::from(dir)];
    let mut checked = 0;
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                pending.push(entry.unwrap().path());
            }
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("slx" | "mdl")
        ) {
            let bytes = std::fs::read(&path).unwrap();
            let name = Path::new(&path).file_name().unwrap().to_str().unwrap();
            let model = unlinked_import::import(name, &bytes).unwrap();
            let report = unlinked_model::validation::validate_structure(&model);
            assert!(report.is_valid(), "{}: {:?}", path.display(), report);
            checked += 1;
        }
    }
    assert!(checked > 0, "no corpus models found");
}
