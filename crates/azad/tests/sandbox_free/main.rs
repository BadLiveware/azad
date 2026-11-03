//! The integration tests that need no sandbox, in one binary, so that the library is linked once for them. Azad's own
//! `//:test` runs them in its image.

mod compile;
mod ecosystems;
mod eval;
mod ir;
mod patterns;
mod project;
mod reference;
mod store;
mod versions;
mod yaml;
