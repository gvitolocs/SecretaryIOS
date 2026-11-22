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
//!
//! What comes out of here is a pointer the data model is asked to address, so the model's cap on
//! depth is checked on the way out: a scope that joins past [`crate::data_model::MAX_DEPTH`] is
//! refused here, in the same `DataError` shape the model refuses it in.

use crate::data_model::{depth_of, MAX_DEPTH};
use crate::error::Error;

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

    /// The absolute pointer `relative` addresses in this scope.
    ///
    /// A pointer that starts with `/` is absolute already and is returned as it stands; `""` and
    /// `"."` are the scope itself; anything else hangs under the scope, however many segments it
    /// has. A trailing slash of the scope is not part of the scope: `/user/` and `/user` are the
    /// same one.
    ///
    /// # Errors
    /// A `DataError` when the pointer that comes out names more segments than
    /// [`MAX_DEPTH`], which is a pointer no part of the port can address.
    pub fn resolve(&self, relative: &str) -> Result<String, Error> {
        let resolved = if relative.starts_with('/') {
            relative.to_string()
        } else {
            let base = self.path.trim_end_matches('/');
            if relative.is_empty() || relative == "." {
                if base.is_empty() {
                    "/".to_string()
                } else {
                    base.to_string()
                }
            } else {
                format!("{base}/{relative}")
            }
        };
        let depth = depth_of(&resolved);
        if depth > MAX_DEPTH {
            return Err(Error::data(format!(
                "Cannot resolve '{relative}' against '{}': the pointer names {depth} segments, more than the {MAX_DEPTH} the model addresses.",
                self.path
            )));
        }
        Ok(resolved)
    }
}

/// The cap is what the suites do not reach: `data_context.yaml` resolves short paths and says
/// nothing about depth, and the refusal has to be here rather than at the write because a resolved
/// pointer is passed around as the address of a binding.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCategory;

    /// A pointer of `segments` segments: `/s0/s1/…`.
    fn pointer(segments: usize) -> String {
        let mut path = String::with_capacity(segments * 4);
        for index in 0..segments {
            path.push('/');
            path.push('s');
            path.push_str(&index.to_string());
        }
        path
    }

    #[test]
    fn a_scope_that_joins_past_the_cap_is_refused() {
        let scope = ScopePath::new("/people/2");
        assert_eq!(scope.resolve("name").as_deref(), Ok("/people/2/name"));
        assert_eq!(scope.resolve(".").as_deref(), Ok("/people/2"));

        // A scope of the deepest addressable size, one segment short of it, still joins.
        let joined = ScopePath::new(&pointer(MAX_DEPTH - 1))
            .resolve("last")
            .expect("a pointer of exactly the deepest size");
        assert_eq!(depth_of(&joined), MAX_DEPTH);

        for problem in [
            ScopePath::new(&pointer(MAX_DEPTH)).resolve("one_more"),
            ScopePath::new("/").resolve(&pointer(MAX_DEPTH + 1)),
        ] {
            assert_eq!(
                problem
                    .expect_err("a pointer of more segments than the model addresses")
                    .category(),
                ErrorCategory::DataError
            );
        }
    }
}
