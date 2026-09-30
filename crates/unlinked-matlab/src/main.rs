use std::io::{self, Read};
fn main() {
    let result = (|| -> Result<String, Box<dyn std::error::Error>> {
        let mut source = String::new();
        io::stdin().read_to_string(&mut source)?;
        Ok(unlinked_matlab::transpile(&source)?)
    })();
    match result {
        Ok(rust) => print!("{rust}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
