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
        return refuse(
            Reason::InvalidScope,
            format!(
                "invalid scope: {raw} (use a project-relative path like src/a.rs, a \
                 directory as src/**, a resource as res:<name>, or **; no globs, absolute \
                 paths, ./ or backslashes)"
            ),
        );
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

/// Why `child` is not within `parent`, for a refusal: the requested patterns
/// no parent pattern contains, what is allowed, and a hint when the allowed
/// side names a bare path that was meant as a directory.
pub fn explain(child: &[String], parent: &[String]) -> String {
    let parse_all = |xs: &[String]| xs.iter().filter_map(|x| parse(x).ok()).collect::<Vec<_>>();
    let (child, parent) = (parse_all(child), parse_all(parent));
    let outside: Vec<&Pattern> = child
        .iter()
        .filter(|c| !parent.iter().any(|p| p.contains(c)))
        .collect();
    let list = |xs: &mut dyn Iterator<Item = &Pattern>| {
        xs.map(Pattern::as_string).collect::<Vec<_>>().join(", ")
    };
    let allowed = if parent.is_empty() {
        "nothing".to_owned()
    } else {
        list(&mut parent.iter())
    };
    let mut text = format!(
        "{} not within {allowed}",
        list(&mut outside.iter().copied())
    );
    let bare = parent.iter().find_map(|p| match p {
        Pattern::Exact(dir)
            if outside.iter().any(|c| match c {
                Pattern::Exact(path) | Pattern::Dir(path) => path
                    .strip_prefix(dir.as_str())
                    .is_some_and(|tail| tail.starts_with('/')),
                _ => false,
            }) =>
        {
            Some(dir)
        }
        _ => None,
    });
    if let Some(dir) = bare {
        text.push_str(&format!(
            "; `{dir}` names one path, its directory is `{dir}/**`"
        ));
    }
    text
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
