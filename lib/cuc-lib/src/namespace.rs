use std::fmt::Display;

#[derive(Debug, Clone)]
pub struct NameSpace {
    scope: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NameSpaceView<'me> {
    scope: &'me [String],
}

impl NameSpace {
    const SEPARATOR: &'static str = "::";
    const FUNC_SEPARATOR: &'static str = "_";

    pub fn root() -> Self {
        Self { scope: vec![] }
    }

    pub fn is_root(&self) -> bool {
        self.scope.is_empty()
    }

    pub fn view(&self) -> NameSpaceView {
        NameSpaceView {
            scope: self.scope.as_slice(),
        }
    }

    pub fn join<S>(mut self, other: S) -> Self
    where
        S: Into<String>,
    {
        let other = other.into();
        if !other.trim().is_empty() {
            self.scope.push(other);
        }
        self
    }

    pub fn parent(&self) -> NameSpaceView {
        if !self.scope.is_empty() {
            NameSpaceView {
                scope: &self.scope[..self.scope.len() - 1],
            }
        } else {
            NameSpaceView { scope: &[] }
        }
    }

    pub fn display(&self) -> String {
        self.scope.join(Self::SEPARATOR)
    }
}

impl Default for NameSpace {
    fn default() -> Self {
        Self::root()
    }
}

impl From<NameSpaceView<'_>> for NameSpace {
    fn from(value: NameSpaceView) -> Self {
        NameSpace {
            scope: value.scope.to_vec(),
        }
    }
}

impl NameSpaceView<'_> {
    pub fn is_root(&self) -> bool {
        self.scope.is_empty()
    }

    pub fn parent(&self) -> NameSpaceView {
        if !self.scope.is_empty() {
            NameSpaceView {
                scope: &self.scope[..self.scope.len() - 1],
            }
        } else {
            NameSpaceView { scope: &[] }
        }

        // assert_eq!(parent_scope("a_b_c_"), "a_b");
        // assert_eq!(parent_scope("a_b_c"), "a_b");
        // assert_eq!(parent_scope("a_"), ""); // only one level
        // assert_eq!(parent_scope("abc"), ""); // no underscores
    }

    pub fn display(&self) -> String {
        self.scope.join(NameSpace::SEPARATOR)
    }

    pub fn as_func_str(&self) -> String {
        self.scope.join(NameSpace::FUNC_SEPARATOR)
    }

    fn join_func_str(this: &mut String, other: impl AsRef<str>) {
        let other = other.as_ref();
        if !other.is_empty() {
            *this += other;
            *this += NameSpace::FUNC_SEPARATOR;
        }
    }

    pub fn flag_func_name<S>(&self, name: S) -> String
    where
        S: AsRef<str>,
    {
        let mut func_name = String::from("_flag_");
        Self::join_func_str(&mut func_name, self.as_func_str());
        func_name += name.as_ref();
        func_name
    }

    pub fn global_flag_func_name(&self) -> String {
        let mut func_name = String::from("_global_flags_");
        func_name += &self.as_func_str();
        func_name
    }

    pub fn cmd_func_name<S>(&self, name: S) -> String
    where
        S: AsRef<str>,
    {
        let mut func_name = String::from("_cmd_");
        Self::join_func_str(&mut func_name, self.as_func_str());
        func_name += &name.as_ref();
        func_name
    }
}

impl Display for NameSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display())
    }
}

impl Display for NameSpaceView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display())
    }
}

pub fn slugify<S>(input: S) -> String
where
    S: AsRef<str>,
{
    input
        .as_ref()
        .chars()
        .filter(|c| c.is_alphanumeric() || c == &'_')
        .collect()
}

pub fn arg_complete_func_name<S>(complete_name: S) -> String
where
    S: AsRef<str>,
{
    let mut func_name = String::from("_complete_arg_");
    func_name += &slugify(complete_name.as_ref());
    func_name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(NameSpace::SEPARATOR, "::");
        assert_eq!(NameSpace::FUNC_SEPARATOR, "_");
    }

    #[test]
    fn test_root() {
        let ns = NameSpace::root();
        assert!(ns.scope.is_empty());
        assert!(ns.is_root());
    }

    #[test]
    fn test_is_root() {
        let root_ns = NameSpace::root();
        assert!(root_ns.is_root());

        let non_root_ns = NameSpace::root().join("test");
        assert!(!non_root_ns.is_root());
    }

    #[test]
    fn test_view() {
        let ns = NameSpace::root().join("a").join("b");
        let view = ns.view();
        assert_eq!(view.scope, &["a", "b"]);

        let root_ns = NameSpace::root();
        let root_view = root_ns.view();
        assert_eq!(root_view.scope, &[] as &[String]);
    }

    #[test]
    fn test_join_string() {
        let ns = NameSpace::root().join("test");
        assert_eq!(ns.scope, vec!["test"]);
        assert!(!ns.is_root());
    }

    #[test]
    fn test_join_empty_string() {
        let ns = NameSpace::root().join("");
        assert!(ns.is_root()); // Empty string should not be added
        assert_eq!(ns.scope, Vec::<String>::new());
    }

    #[test]
    fn test_join_whitespace_only() {
        let ns = NameSpace::root().join("   ");
        assert!(ns.is_root()); // Whitespace-only should not be added
        assert_eq!(ns.scope, Vec::<String>::new());
    }

    #[test]
    fn test_join_whitespace_around_content() {
        let ns = NameSpace::root().join("  test  ");
        assert_eq!(ns.scope, vec!["  test  "]); // trim() only checks, doesn't modify
    }

    #[test]
    fn test_join_chaining() {
        let ns = NameSpace::root().join("a").join("b").join("c");
        assert_eq!(ns.scope, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_join_mixed_empty() {
        let ns = NameSpace::root()
            .join("a")
            .join("")
            .join("b")
            .join("   ")
            .join("c");
        assert_eq!(ns.scope, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parent_empty_scope() {
        let ns = NameSpace::root();
        let parent = ns.parent();
        assert_eq!(parent.scope, &[] as &[String]);
    }

    #[test]
    fn test_parent_single_level() {
        let ns = NameSpace::root().join("a");
        let parent = ns.parent();
        assert_eq!(parent.scope, &[] as &[String]);
    }

    #[test]
    fn test_parent_multi_level() {
        let ns = NameSpace::root().join("a").join("b").join("c");
        let parent = ns.parent();
        assert_eq!(parent.scope, &["a", "b"]);
    }

    #[test]
    fn test_display_empty() {
        let ns = NameSpace::root();
        assert_eq!(ns.display(), "");
    }

    #[test]
    fn test_display_single() {
        let ns = NameSpace::root().join("test");
        assert_eq!(ns.display(), "test");
    }

    #[test]
    fn test_display_multiple() {
        let ns = NameSpace::root().join("a").join("b").join("c");
        assert_eq!(ns.display(), "a::b::c");
    }

    #[test]
    fn test_display_with_special_chars() {
        let ns = NameSpace::root().join("a_b").join("c::d").join("e-f");
        assert_eq!(ns.display(), "a_b::c::d::e-f");
    }

    #[test]
    fn test_complex_workflow() {
        let ns = NameSpace::root()
            .join("module")
            .join("submodule")
            .join("function");

        assert!(!ns.is_root());
        assert_eq!(ns.display(), "module::submodule::function");

        let parent = ns.parent();
        assert_eq!(parent.scope, &["module", "submodule"]);

        let view = ns.view();
        assert_eq!(view.scope, &["module", "submodule", "function"]);
    }

    #[test]
    fn test_edge_cases() {
        let ns1 = NameSpace::root().join("\t");
        assert!(ns1.is_root());

        let ns2 = NameSpace::root().join("\n");
        assert!(ns2.is_root());

        let ns3 = NameSpace::root().join(" \t \n ");
        assert!(ns3.is_root());
    }
}
