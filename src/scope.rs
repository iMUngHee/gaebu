//! Scope patterns (Decision 9). Deliberately tiny so containment is decidable:
//!
//! - `**`            every path
//! - `a/b/c`         exactly that path
//! - `a/b/**`        that directory and everything below it
//! - `res:<name>`    one resource (e.g. `res:gpu`)
//! - `res:**`        every resource
//!
//! Paths are project-relative, `/`-separated and compared lowercased. Anything
//! else (empty segments, `.`, `..`, `\`, drive letters, a leading `/`, any
//! other wildcard) is `invalid_scope`.

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    All,
    Exact(String),
    Dir(String),
    Res(String),
    ResAll,
}

/// Parse and normalize one pattern.
pub fn parse(raw: &str) -> Result<Pattern> {
    let _ = raw;
    todo!("nawi: scope grammar")
}

impl Pattern {
    /// Canonical text form (what is stored).
    pub fn as_string(&self) -> String {
        todo!("nawi: scope grammar")
    }

    /// True when every path or resource matched by `other` is matched by `self`.
    pub fn contains(&self, other: &Pattern) -> bool {
        let _ = other;
        todo!("nawi: scope containment")
    }

    /// True when some path or resource is matched by both.
    pub fn overlaps(&self, other: &Pattern) -> bool {
        let _ = other;
        todo!("nawi: scope overlap")
    }
}

/// Parse and canonicalize a list; rejects the whole list on the first bad item.
pub fn normalize_list(raw: &[String]) -> Result<Vec<String>> {
    let _ = raw;
    todo!("nawi: scope grammar")
}

/// True when every pattern in `child` is contained by some pattern in `parent`.
pub fn within(child: &[String], parent: &[String]) -> Result<bool> {
    let _ = (child, parent);
    todo!("nawi: scope containment")
}
