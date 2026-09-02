//! Instrument one file and print the result (debug/validation helper).
fn main() {
    let path = std::env::args().nth(1).expect("usage: gen-instrumented <file>");
    let source = std::fs::read_to_string(&path).unwrap();
    let out = stryker_instrumenter::instrument_file(
        camino::Utf8Path::new(&path),
        &source,
        0,
        &stryker_instrumenter::InstrumentOptions::default(),
    )
    .unwrap();
    match out.instrumented {
        Some(text) => print!("{text}"),
        None => std::process::exit(3), // no mutants
    }
}
