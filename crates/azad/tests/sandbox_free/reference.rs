//! `docs/starlark.md` is the reference that the docstrings of the Starlark built-ins make.

use std::env;
use std::fs;
use std::path::Path;

const REFERENCE_FILE: &str = "../../docs/starlark.md";
const UPDATE_ENV: &str = "AZAD_UPDATE_REFERENCE";

#[test]
fn the_starlark_reference_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(REFERENCE_FILE);
    let made = azad::frontend::starlark::reference();
    if env::var_os(UPDATE_ENV).is_some() {
        fs::write(&path, &made).expect("the reference is written");
        return;
    }
    let written = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        written == made,
        "{} is not the reference that the docstrings make; make it again with `{UPDATE_ENV}=1 cargo test -p azad --test reference`",
        path.display()
    );
}
