use std::io::{self, Read};
fn main() {
    let result = (|| -> Result<String, Box<dyn std::error::Error>> {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        let library = match arguments.as_slice() {
            [] => false,
            [mode] if mode == "--library" => true,
            _ => return Err("usage: unlinked-matlab [--library] < source.m > generated.rs (use unlinked transpile for a complete Cargo project)".into()),
        };
        let mut source = String::new();
        io::stdin().read_to_string(&mut source)?;
        Ok(unlinked_matlab::transpile_typed(&source, library)?)
    })();
    match result {
        Ok(rust) => print!("{rust}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
