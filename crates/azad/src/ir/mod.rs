//! The Action IR (intermediate representation) of Azad: digests, the canonical encoding, directory trees, actions and
//! graphs of actions.

pub mod action;
pub mod canonical;
pub mod cbor;
pub mod digest;
pub mod graph;
pub mod image;
pub mod result;
pub mod text;
pub mod tree;

pub use action::{Action, ActionDef, Kind, Source, Spec, WORK_DIR};
pub use canonical::{Canonical, CanonicalError};
pub use digest::{Digest, HashFunction, Hasher};
pub use graph::{Graph, VerifyError};
pub use image::Image;
pub use result::ActionResult;
pub use tree::{Directory, Node};
