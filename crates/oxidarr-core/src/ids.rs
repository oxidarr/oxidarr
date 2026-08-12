//! Newtyped identifiers.
//!
//! `IndexerId`, `AppId`, and `RemoteIndexerId` appear together in the
//! application-sync mapping and are all integers underneath. Swapping two of
//! them would compile and silently desynchronise every downstream
//! application, so they are distinct types.

use serde::{Deserialize, Serialize};

macro_rules! id_newtype {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub i32);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_newtype!(
    /// Identifies an indexer within this Oxidarr instance.
    IndexerId
);
id_newtype!(
    /// Identifies a configured downstream application.
    AppId
);
id_newtype!(
    /// Identifies an indexer *inside a remote application*, not here.
    RemoteIndexerId
);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn serializes_transparently_as_a_bare_integer() {
        assert_eq!(serde_json::to_string(&IndexerId(7)).unwrap(), "7");
    }

    #[test]
    fn deserializes_from_a_bare_integer() {
        let id: RemoteIndexerId = serde_json::from_str("42").unwrap();
        assert_eq!(id, RemoteIndexerId(42));
    }

    #[test]
    fn displays_as_the_inner_value() {
        assert_eq!(AppId(3).to_string(), "3");
    }
}
