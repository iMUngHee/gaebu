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

use crate::error::{Reason, Result, refuse};

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
    if raw == "**" {
        return Ok(Pattern::All);
    }
    if raw == "res:**" {
        return Ok(Pattern::ResAll);
    }
    let (name, resource, directory) = if let Some(name) = raw.strip_prefix("res:") {
        (name, true, false)
    } else if let Some(name) = raw.strip_suffix("/**") {
        (name, false, true)
    } else {
        (raw, false, false)
    };
    if name.contains(['\\', ':', '*', '?', '[', ']', '{', '}'])
        || (resource && name.contains('/'))
        || name.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return refuse(Reason::InvalidScope, format!("invalid scope: {raw}"));
    }
    let name = name.to_lowercase();
    Ok(if resource {
        Pattern::Res(name)
    } else if directory {
        Pattern::Dir(name)
    } else {
        Pattern::Exact(name)
    })
}

impl Pattern {
    /// Canonical text form (what is stored).
    pub fn as_string(&self) -> String {
        match self {
            Self::All => "**".into(),
            Self::Exact(name) => name.clone(),
            Self::Dir(name) => format!("{name}/**"),
            Self::Res(name) => format!("res:{name}"),
            Self::ResAll => "res:**".into(),
        }
    }

    /// True when every path or resource matched by `other` is matched by `self`.
    pub fn contains(&self, other: &Pattern) -> bool {
        match (self, other) {
            (Self::All, Self::All | Self::Exact(_) | Self::Dir(_)) => true,
            (Self::Dir(parent), Self::Exact(child) | Self::Dir(child)) => {
                child == parent
                    || child
                        .strip_prefix(parent)
                        .is_some_and(|tail| tail.starts_with('/'))
            }
            (Self::Exact(a), Self::Exact(b)) | (Self::Res(a), Self::Res(b)) => a == b,
            (Self::ResAll, Self::Res(_) | Self::ResAll) => true,
            _ => false,
        }
    }

    /// True when some path or resource is matched by both.
    pub fn overlaps(&self, other: &Pattern) -> bool {
        self.contains(other) || other.contains(self)
    }
}

/// Parse and canonicalize a list; rejects the whole list on the first bad item.
pub fn normalize_list(raw: &[String]) -> Result<Vec<String>> {
    raw.iter()
        .map(|item| parse(item).map(|pattern| pattern.as_string()))
        .collect()
}

/// True when every pattern in `child` is contained by some pattern in `parent`.
pub fn within(child: &[String], parent: &[String]) -> Result<bool> {
    let child = child
        .iter()
        .map(|item| parse(item))
        .collect::<Result<Vec<_>>>()?;
    let parent = parent
        .iter()
        .map(|item| parse(item))
        .collect::<Result<Vec<_>>>()?;
    Ok(child
        .iter()
        .all(|item| parent.iter().any(|grant| grant.contains(item))))
}
