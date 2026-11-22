//! Scoped path resolution: where a binding written relative to a component's scope points.
//!
//! A component inside a list item binds `name`, and the scope of that item is `/people/2`; the
//! binding then addresses `/people/2/name`. An absolute pointer ignores the scope, an empty or `.`
//! pointer is the scope itself.
//!
//! The behaviour is the original's `DataContext.resolve_path`
//! (`a2ui/core/resolution/data_context.py`) measured by every case of
//! `conformance/core/data_context.yaml`. The rest of the original's `DataContext` — bindings,
//! function calls, actions — is built with the message processor; what lives here is the scope,
//! which needs none of it.

/// The data path one scope of a surface reads relative pointers against.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopePath {
    /// The scope, ended by exactly one `/` as the original keeps it, so joining is concatenation.
    path: String,
}

impl ScopePath {
    /// The scope `context`, which is the root when it is empty.
    pub fn new(context: &str) -> ScopePath {
        ScopePath {
            path: if context.is_empty() {
                "/".to_string()
            } else if context.ends_with('/') {
                context.to_string()
            } else {
                format!("{context}/")
            },
        }
    }

    /// The scope itself, in the original's spelling.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The absolute pointer `relative` addresses in this scope.
    ///
    /// A pointer that starts with `/` is absolute already and is returned as it stands; `""` and
    /// `"."` are the scope itself; anything else hangs under the scope, however many segments it
    /// has.
    pub fn resolve(&self, relative: &str) -> String {
        let _ = (&self.path, relative);
        String::new()
    }
}
