use std::io::{self, Read};
fn main() {
    let result = (|| -> Result<String, Box<dyn std::error::Error>> {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        let library = match arguments.as_slice() {
            [] => false,
            [mode] if mode == "--library" => true,
            _ => return Err("usage: unlinked-matlab [--library] < source.m > generated.rs".into()),
        };
        let mut source = String::new();
        io::stdin().read_to_string(&mut source)?;
        Ok(if library {
            unlinked_matlab::transpile_library(&source)?
        } else {
            unlinked_matlab::transpile(&source)?
        })
    })();
    match result {
        Ok(rust) => print!("{rust}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
