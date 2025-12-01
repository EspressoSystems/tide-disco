use itertools::Itertools;
use snafu::Snafu;
use std::{
    collections::{btree_map::Entry, BTreeMap},
    ops::Index,
};

pub use crate::join;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Module<Api> {
    pub(crate) prefix: Vec<String>,
    pub(crate) versions: BTreeMap<u64, Api>,
}

impl<Api> Module<Api> {
    fn new(prefix: Vec<String>) -> Self {
        Self {
            prefix,
            versions: Default::default(),
        }
    }

    pub(crate) fn path(&self) -> String {
        self.prefix.join("/")
    }
}

#[derive(Clone, Debug, Snafu, PartialEq, Eq)]
pub enum DispatchError {
    #[snafu(display("duplicate module {prefix} v{version}"))]
    ModuleAlreadyExists { prefix: String, version: u64 },
    #[snafu(display("module {prefix} cannot be a prefix of module {conflict}"))]
    ConflictingModules { prefix: String, conflict: String },
}

/// Mapping from route prefixes to APIs.
#[derive(Debug)]
pub(crate) enum Trie<Api> {
    Branch {
        /// The route prefix represented by this node.
        prefix: Vec<String>,
        /// APIs with this prefix, indexed by the next route segment.
        children: BTreeMap<String, Box<Self>>,
    },
    Leaf {
        /// APIs available at this prefix, sorted by version.
        module: Module<Api>,
    },
}

impl<Api> Default for Trie<Api> {
    fn default() -> Self {
        Self::Branch {
            prefix: vec![],
            children: Default::default(),
        }
    }
}

impl<Api> Trie<Api> {
    /// Whether this is a singleton [`Trie`].
    ///
    /// A singleton [`Trie`] is one with only one module, registered under the empty prefix. Note
    /// that any [`Trie`] with a module with an empty prefix must be singleton, because no other
    /// modules would be permitted: the empty prefix is a prefix of every other module path.
    pub(crate) fn is_singleton(&self) -> bool {
        matches!(self, Self::Leaf { .. })
    }

    /// Insert a new API with a certain version under the given prefix.
    pub(crate) fn insert<I>(
        &mut self,
        prefix: I,
        version: u64,
        api: Api,
    ) -> Result<(), DispatchError>
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let mut prefix = prefix.into_iter().map(|segment| segment.into());

        // Traverse to a leaf matching `prefix`.
        let mut curr = self;
        while let Some(segment) = prefix.next() {
            // If there are more segments in the prefix, we must be at a branch.
            match curr {
                Self::Branch { prefix, children } => {
                    // Move to the child associated with the next path segment, inserting an empty
                    // child if this is the first module we've seen that has this path as a prefix.
                    curr = children.entry(segment.clone()).or_insert_with(|| {
                        let mut prefix = prefix.clone();
                        prefix.push(segment);
                        Box::new(Trie::Branch {
                            prefix,
                            children: Default::default(),
                        })
                    });
                }
                Self::Leaf { module } => {
                    // If there is a leaf here, then there is already a module registered which is a
                    // prefix of the new module. This is not allowed.
                    return Err(DispatchError::ConflictingModules {
                        prefix: module.path(),
                        conflict: join!(&module.path(), &segment, &prefix.join("/")),
                    });
                }
            }
        }

        // If we have reached the end of the prefix, we must be at either a leaf or a temporary
        // empty branch that we can turn into a leaf.
        if let Self::Branch { prefix, children } = curr {
            if children.is_empty() {
                *curr = Self::Leaf {
                    module: Module::new(prefix.clone()),
                };
            } else {
                // If we have a non-trival branch at the end of the desired prefix, there is already
                // a module registered for which `prefix` is a strict prefix of the registered path.
                // This is not allowed. To give a useful error message, follow the existing trie
                // down to a leaf so we can give an example of a module which conflicts with this
                // prefix.
                let prefix = prefix.join("/");
                let conflict = loop {
                    match curr {
                        Self::Branch { children, .. } => {
                            curr = children
                                .values_mut()
                                .next()
                                .expect("malformed dispatch trie: empty branch");
                        }
                        Self::Leaf { module } => {
                            break module.path();
                        }
                    }
                };
                return Err(DispatchError::ConflictingModules { prefix, conflict });
            }
        }
        let Self::Leaf { module } = curr else {
            unreachable!();
        };

        // Insert the new API, as long as there isn't already an API with the same version in this
        // module.
        let Entry::Vacant(e) = module.versions.entry(version) else {
            return Err(DispatchError::ModuleAlreadyExists {
                prefix: module.path(),
                version,
            });
        };
        e.insert(api);
        Ok(())
    }

    /// Get the module named by `prefix`.
    ///
    /// This function is similar to [`search`](Self::search), except the given `prefix` must exactly
    /// match the prefix under which a module is registered.
    pub(crate) fn get<I>(&self, prefix: I) -> Option<&Module<Api>>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let mut iter = prefix.into_iter();
        let module = self.traverse(&mut iter)?;
        // Check for exact match.
        if iter.next().is_some() {
            None
        } else {
            Some(module)
        }
    }

    /// Get the supported versions of the API identified by the given request path.
    ///
    /// If a prefix of `path` uniquely identifies a registered module, the module (with all
    /// supported versions) is returned.
    pub(crate) fn search<I>(&self, path: I) -> Option<&Module<Api>>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.traverse(&mut path.into_iter())
    }

    /// Iterate over registered modules and their supported versions.
    pub(crate) fn iter(&self) -> Iter<'_, Api> {
        Iter { stack: vec![self] }
    }

    /// Internal implementation of `get` and `search`.
    ///
    /// Returns the matching module and advances the iterator past all the segments used in the
    /// match.
    fn traverse<I>(&self, iter: &mut I) -> Option<&Module<Api>>
    where
        I: Iterator,
        I::Item: AsRef<str>,
    {
        let mut curr = self;
        loop {
            match curr {
                Self::Branch { children, .. } => {
                    // Traverse to the next child based on the next segment in the path.
                    let segment = iter.next()?;
                    curr = children.get(segment.as_ref())?;
                }
                Self::Leaf { module } => return Some(module),
            }
        }
    }
}

pub(crate) struct Iter<'a, Api> {
    stack: Vec<&'a Trie<Api>>,
}

impl<'a, Api> Iterator for Iter<'a, Api> {
    type Item = &'a Module<Api>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.stack.pop()? {
                Trie::Branch { children, .. } => {
                    // Push children onto the stack and start visiting them. We add them in reverse
                    // order so that we will visit the lexicographically first children first.
                    self.stack
                        .extend(children.values().rev().map(|boxed| &**boxed));
                }
                Trie::Leaf { module } => return Some(module),
            }
        }
    }
}

impl<'a, Api> IntoIterator for &'a Trie<Api> {
    type IntoIter = Iter<'a, Api>;
    type Item = &'a Module<Api>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<I, Api> Index<I> for Trie<Api>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    type Output = Module<Api>;

    fn index(&self, index: I) -> &Self::Output {
        self.get(index).unwrap()
    }
}

/// Split a path prefix into its segments.
///
/// Leading and trailing slashes are ignored. That is, `/prefix/` yields only the single segment
/// `prefix`, with no preceding or following empty segments.
pub(crate) fn split(s: &str) -> impl '_ + Iterator<Item = &str> {
    s.split('/').filter(|seg| !seg.is_empty())
}

/// Join two path strings, ensuring there are no leading or trailing slashes.
pub(crate) fn join(s1: &str, s2: &str) -> String {
    let s1 = s1.strip_prefix('/').unwrap_or(s1);
    let s1 = s1.strip_suffix('/').unwrap_or(s1);
    let s2 = s2.strip_prefix('/').unwrap_or(s2);
    let s2 = s2.strip_suffix('/').unwrap_or(s2);
    if s1.is_empty() {
        s2.to_string()
    } else if s2.is_empty() {
        s1.to_string()
    } else {
        format!("{s1}/{s2}")
    }
}

#[macro_export]
macro_rules! join {
    () => { String::new() };
    ($s:expr) => { $s };
    ($head:expr$(, $($tail:expr),*)?) => {
        $crate::dispatch::join($head, &$crate::join!($($($tail),*)?))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_empty_trie() {
        let t = Trie::<()>::default();
        assert_eq!(t.iter().next(), None);
        assert_eq!(t.get(["mod"]), None);
    }

    #[test]
    fn test_branch_trie() {
        let mut t = Trie::default();

        let mod_a = Module {
            prefix: vec!["mod".into(), "a".into()],
            versions: [(0, 0)].into(),
        };
        let mod_b = Module {
            prefix: vec!["mod".into(), "b".into()],
            versions: [(1, 1)].into(),
        };

        t.insert(["mod", "a"], 0, 0).unwrap();
        t.insert(["mod", "b"], 1, 1).unwrap();

        assert_eq!(t.iter().collect::<Vec<_>>(), [&mod_a, &mod_b]);

        assert_eq!(t.search(["mod", "a", "route"]), Some(&mod_a));
        assert_eq!(t.get(["mod", "a"]), Some(&mod_a));
        assert_eq!(t.get(["mod", "a", "route"]), None);

        assert_eq!(t.search(["mod", "b", "route"]), Some(&mod_b));
        assert_eq!(t.get(["mod", "b"]), Some(&mod_b));
        assert_eq!(t.get(["mod", "b", "route"]), None);

        // Cannot register a module which is a prefix or suffix of the already registered modules.
        t.insert(["mod"], 0, 0).unwrap_err();
        t.insert(Vec::<String>::new(), 0, 0).unwrap_err();
        t.insert(["mod", "a", "b"], 0, 0).unwrap_err();
    }

    #[test]
    fn test_null_prefix() {
        let mut t = Trie::default();

        let module = Module {
            prefix: vec![],
            versions: [(0, 0)].into(),
        };
        t.insert(Vec::<String>::new(), 0, 0).unwrap();

        assert_eq!(t.iter().collect::<Vec<_>>(), [&module]);
        assert_eq!(t.search(["anything"]), Some(&module));
        assert_eq!(t.get(Vec::<String>::new()), Some(&module));
        assert_eq!(t.get(["anything"]), None);

        // Any other module has the null module as a prefix and is thus not allowed.
        t.insert(["anything"], 1, 1).unwrap_err();
    }
}
