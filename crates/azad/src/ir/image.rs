//! The record of a pulled image: its tree, and the part of its configuration that an action uses. The store and remote
//! caches keep it under the name of the image, so the configuration is known without the layers (decision 0016).

use std::collections::BTreeMap;

use crate::ir::canonical::{Canonical, CanonicalError, Fields, digest_to_value, text_map, text_values};
use crate::ir::cbor::Value;
use crate::ir::digest::Digest;

const KEY_TREE: &str = "r";
const KEY_ENV: &str = "e";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The layers of the image, applied into one tree.
    pub tree: Digest,
    /// The `Env` of the image's configuration: the base of the environment of an action whose root is the image.
    pub env: BTreeMap<String, String>,
}

impl Image {
    /// The environment of an action with this root: the image's, with each variable of `action_env` in place of the
    /// image's value, as `docker run --env` does.
    pub fn env_with(&self, action_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut env = self.env.clone();
        env.extend(action_env.iter().map(|(key, value)| (key.clone(), value.clone())));
        env
    }
}

impl Canonical for Image {
    const TYPE: &'static str = "image";

    fn fields(&self) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (KEY_TREE.to_owned(), digest_to_value(&self.tree)),
            (KEY_ENV.to_owned(), text_values(&self.env)),
        ])
    }

    fn from_fields(fields: &mut Fields) -> Result<Self, CanonicalError> {
        Ok(Image { tree: fields.digest(KEY_TREE)?, env: text_map(fields.map(KEY_ENV)?, KEY_ENV)? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_action_env_replaces_the_image_env_per_variable() {
        let image = Image {
            tree: Digest::of_bytes(b"tree"),
            env: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin".to_owned()),
                ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ]),
        };
        let action_env =
            BTreeMap::from([("PATH".to_owned(), "/opt/bin".to_owned()), ("HOME".to_owned(), "/tmp".to_owned())]);
        let expected = BTreeMap::from([
            ("PATH".to_owned(), "/opt/bin".to_owned()),
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ("HOME".to_owned(), "/tmp".to_owned()),
        ]);
        assert_eq!(image.env_with(&action_env), expected);
        assert_eq!(Image::decode(&image.encode()), Ok(image));
    }
}
