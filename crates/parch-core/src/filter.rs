//! Include/exclude glob filters on archive names.
//!
//! `*` matches across `/`. A pattern without `/` also matches the last name component
//! at any depth (like `tar --exclude`).

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::{Error, Result};

#[derive(Default)]
struct Patterns {
    full: Option<GlobSet>,
    base: Option<GlobSet>,
}

impl Patterns {
    fn new(patterns: &[String]) -> Result<Self> {
        if patterns.is_empty() {
            return Ok(Self::default());
        }
        let mut full = GlobSetBuilder::new();
        let mut base = GlobSetBuilder::new();
        let mut has_base = false;
        for p in patterns {
            let p = p.trim_end_matches('/');
            let glob = GlobBuilder::new(p)
                .literal_separator(false)
                .backslash_escape(true)
                .build()
                .map_err(|e| Error::msg(format!("invalid pattern {p:?}: {e}")))?;
            full.add(glob.clone());
            if !p.contains('/') {
                base.add(glob);
                has_base = true;
            }
        }
        let build = |b: GlobSetBuilder| b.build().map_err(|e| Error::msg(e.to_string()));
        Ok(Self { full: Some(build(full)?), base: if has_base { Some(build(base)?) } else { None } })
    }

    fn is_empty(&self) -> bool {
        self.full.is_none()
    }

    /// Directory names may carry a trailing `/`; `dir/*` then also matches the directory itself.
    fn matches(&self, name: &str) -> bool {
        let trimmed = name.trim_end_matches('/');
        if self.full.as_ref().is_some_and(|s| s.is_match(name) || s.is_match(trimmed)) {
            return true;
        }
        let name = trimmed;
        let base = name.rsplit('/').next().unwrap_or(name);
        self.base.as_ref().is_some_and(|s| s.is_match(base))
    }
}

#[derive(Default)]
pub struct Filter {
    include: Patterns,
    exclude: Patterns,
}

impl Filter {
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self> {
        Ok(Self { include: Patterns::new(include)?, exclude: Patterns::new(exclude)? })
    }

    pub fn excluded(&self, name: &str) -> bool {
        self.exclude.matches(name)
    }

    /// True when there are no include patterns or one of them matches.
    pub fn included(&self, name: &str) -> bool {
        self.include.is_empty() || self.include.matches(name)
    }

    pub fn accepts(&self, name: &str) -> bool {
        self.included(name) && !self.excluded(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(inc: &[&str], exc: &[&str]) -> Filter {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Filter::new(&v(inc), &v(exc)).unwrap()
    }

    #[test]
    fn exclude_by_basename_and_path() {
        let f = f(&[], &["*.o", "node_modules", "build/tmp"]);
        assert!(f.excluded("src/a.o"));
        assert!(f.excluded("x/y/node_modules"));
        assert!(f.excluded("build/tmp"));
        assert!(!f.excluded("other/build/tmp"));
        assert!(!f.excluded("src/a.c"));
        assert!(f.accepts("src/a.c"));
        let d = super::Filter::new(&["dir/*".to_string()], &[]).unwrap();
        assert!(d.included("dir/"));
        assert!(d.included("dir/x"));
        assert!(!d.included("dir"));
    }

    #[test]
    fn include_patterns() {
        let f = f(&["*.rs"], &["target/*"]);
        assert!(f.accepts("src/main.rs"));
        assert!(!f.accepts("README.md"));
        assert!(!f.accepts("target/x.rs"));
        let empty = Filter::default();
        assert!(empty.accepts("anything"));
    }
}
