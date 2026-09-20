//! Stable identifiers. Ids are never reused and never renumbered: every op and every
//! selector addresses objects through them, so a reorder can never move an agent's target.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
            schemars::JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            /// Fresh, sortable, collision-free id.
            pub fn generate() -> Self {
                Self(format!(
                    "{}_{}",
                    $prefix,
                    ulid::Ulid::new().to_string()[10..].to_ascii_lowercase()
                ))
            }

            /// Id derived from a human name, e.g. `lyr_sky`. Uniqueness is the caller's job.
            pub fn from_name(name: &str) -> Self {
                let slug: String = name
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
                    .collect();
                let slug = slug.trim_matches('-').to_string();
                if slug.is_empty() {
                    Self::generate()
                } else {
                    Self(format!("{}_{}", $prefix, slug))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
    };
}

id_type!(DocId, "doc");
id_type!(LayerId, "lyr");
id_type!(ObjectId, "obj");
id_type!(ArtboardId, "ab");
id_type!(NodeId, "nd");
id_type!(MeshId, "msh");
id_type!(MaterialId, "mat");
id_type!(LightId, "lgt");
id_type!(CameraId, "cam");
id_type!(AnimId, "anm");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_ids_are_readable_and_generated_ids_are_unique() {
        assert_eq!(LayerId::from_name("Sky Gradient").as_str(), "lyr_sky-gradient");
        assert_eq!(LayerId::from_name("  ").as_str().len() > 4, true);
        assert_ne!(LayerId::generate(), LayerId::generate());
    }

    #[test]
    fn ids_round_trip_as_bare_json_strings() {
        let id = DocId::from("doc_main");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"doc_main\"");
        let back: DocId = serde_json::from_str("\"doc_main\"").unwrap();
        assert_eq!(back, id);
    }
}
