#![warn(clippy::all, clippy::pedantic)]
/*!
This crate is for expanding environment variables and tilde's (`~`) with support for fallback
values.

## Examples

```rust
# use anyhow::{self, Context};
# use directories_next;
# use std::path::PathBuf;
# use expandenv::expand_by_path_components;
#
# fn main() -> anyhow::Result<()> {
# const TEST_ENVVAR_KEY: &str = "FOO";
# const TEST_ENVVAR_VALUE: &str = "bar";
#
# let test_envvar_value = unsafe {
#     std::env::set_var(TEST_ENVVAR_KEY, TEST_ENVVAR_VALUE);
#     let value = std::env::var(TEST_ENVVAR_KEY).context("failed to set test envvar")?;
#     assert_eq!(
#         TEST_ENVVAR_VALUE, value,
#         "failed to set {TEST_ENVVAR_KEY}={TEST_ENVVAR_VALUE}, got '{value}' instead."
#     );
#
#     std::path::PathBuf::from(value)
# };
#
// when you expand a variable, it returns the value
let envvar_value = expand_by_path_components("$FOO")?;
# assert_eq!(test_envvar_value, envvar_value);

// if expansion fails, you can return an error...
assert!(expand_by_path_components("$MISSING_VAR").is_err());
// or try to parse a fallback value
let envvar_value = expand_by_path_components("${MISSING_VAR:-some/path}")?;
# assert_eq!(PathBuf::from("some/path"), envvar_value);
// and nest them as much as you want! this example returns the value of `$FOO`
let envvar_value = expand_by_path_components("${MISSING_VAR:-${ANOTHER_MISSING_VAR:-$FOO}}")?;
# assert_eq!(test_envvar_value, envvar_value);

// it's not limited; you can expand an entire path!
// the `~` expands to your home directory for simplicity
let path = expand_by_path_components("~/${MISSING_VAR:-$FOO}/file.txt")?;
# let base_dirs = directories_next::BaseDirs::new().context("failed to find home dir")?;
# let home = base_dirs.home_dir();
# assert_eq!(home.join(TEST_ENVVAR_VALUE).join("file.txt"), path);
#
# Ok(())
# }
```
*/

use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    path::PathBuf,
    sync::LazyLock,
};

use directories_next::BaseDirs;
use regex::Regex;

use crate::errors::ExpandError;

pub mod errors;

/// Lazy wrapper around [`directories_next::BaseDirs::new`].
static BASE_DIRS: LazyLock<BaseDirs> =
    LazyLock::new(|| BaseDirs::new().expect("failed to locate users home directory"));

/// Mimic's the behavior of [`PathBuf::components`] while respecting curly braces.
fn __parse_path_components_with_braces(s: &str) -> Vec<OsString> {
    let mut brace_depth: u8 = 0;
    let mut components = Vec::<OsString>::new();
    let mut comp = OsString::new();
    let mut comp_is_envvar = false;
    let mut char_iter = s.chars().peekable();

    // if path starts with path sep, consider it a path component
    // and remove it from the iterator
    if char_iter.peek().copied() == Some(std::path::MAIN_SEPARATOR) {
        components.push(std::path::MAIN_SEPARATOR.to_string().into());
        let _ = char_iter.next();
    }

    for c in char_iter {
        match c {
            std::path::MAIN_SEPARATOR if brace_depth == 0 => {
                components.push(comp.clone());
                comp.clear();
                comp_is_envvar = false;
                continue;
            }

            '$' => comp_is_envvar = true,
            '{' if comp_is_envvar => brace_depth = brace_depth.saturating_add(1),
            '}' if comp_is_envvar => brace_depth = brace_depth.saturating_sub(1),

            _ => {}
        } // match c

        comp.push(c.to_string());
    } // for c in char_iter

    components.push(comp.clone());

    components
}

pub fn expand<S: ToString + ?Sized>(s: &S) -> Result<String, ExpandError> {
    static ENVVAR_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        /*
         * There are three capture groups:
         * 1. The environment variable (minus the $)
         *    ([\w_][\w\d_]*)
         * 2. Ignore this group
         * 3. The fallback value
         *    (.*?)
         */
        Regex::new(r"\$\{?([a-zA-Z_]\w*)(:-(.*?))?\}?").expect("invalid envvar regex")
    });

    let s = s.to_string();
    #[cfg(windows)]
    let s = s.replace('/', "\\");
    let mut expanded = s.clone();
    let mut total_offset: isize = 0;

    for capture in ENVVAR_REGEX.captures_iter(&s) {
        let full_match = capture.get_match();
        let mtch = capture
            .get(1)
            .and_then(|m| if m.is_empty() { None } else { Some(m) })
            .ok_or(ExpandError::EmptyEnvvarCapture)?;
        let envvar = mtch.as_str();

        #[cfg(debug_assertions)]
        println!("expanding envvar '{envvar:?}'");

        let envvar_value = if let Some(value) = std::env::var_os(envvar) {
            #[cfg(debug_assertions)]
            println!("{envvar:?}={}", value.display());

            value.to_string_lossy().to_string()
        } else if let Some(fallback) = capture.get(3) {
            let fallback = fallback.as_str();
            #[cfg(debug_assertions)]
            println!("failed to expand '{envvar:?}', found fallback '{fallback:?}'");

            expand(fallback)?
        } else {
            return Err(ExpandError::EnvvarReadError(envvar.to_string()));
        };

        println!("found envvar: {envvar_value:?}");

        let mut range_to_replace = full_match.range();
        range_to_replace.start = range_to_replace.start.saturating_add_signed(total_offset);
        range_to_replace.end = range_to_replace.end.saturating_add_signed(total_offset);
        // this should never overflow because WHY would you be using this on a string with 2.1
        // BILLION characters??
        let old_len = isize::try_from(expanded.len())
            .unwrap_or_else(|err| panic!("failed to cast usize -> isize: {err:?}"));
        eprintln!("expanded={expanded:?} ({})", expanded.len());
        expanded.replace_range(dbg!(range_to_replace.clone()), &envvar_value);
        let new_len = isize::try_from(expanded.len())
            .unwrap_or_else(|err| panic!("failed to cast usize -> isize: {err:?}"));
        total_offset += new_len - old_len;
    }

    Ok(expanded)
}

/// Convert a `&str` slice into a `PathBuf`, expanding envvars and the leading tilde `~`, if it
/// is there.
///
/// The tilde (`~`) expands into the users home directory as defined by [`directories_next::BaseDirs::home_dir`].
///
/// Environment variables expand into their value, optionally expanding a fallback value if the var
/// cannot be read. Envvars may contain letters, numbers, and underscores (`_`), but they _must_ start
/// with either a letter or an underscore after the dollar sign (`$`). Although more complicated
/// syntax is technically allowed by most programming languages, I will not be supporting anything
/// other than this basic structure because this is what most shells support and if you're doing
/// something different, ask yourself why.
///
/// # Arguments
///
/// - `s`: String to expand and convert
///
/// # Errors
///
/// An error is returned if:
///
/// - An envvar cannot be expanded
/// - You don't have a home directory
pub fn expand_by_path_components<S: AsRef<str>>(s: S) -> Result<PathBuf, ExpandError> {
    static ENVVAR_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        /*
         * There are three capture groups:
         * 1. The environment variable (minus the $)
         *    ([\w_][\w\d_]*)
         * 2. Ignore this group
         * 3. The fallback value
         *    (.*?)
         */
        Regex::new(r"^\$\{?([a-zA-Z_]\w*)(:-(.*?))?\}?$").expect("invalid envvar regex")
    });

    let s = s.as_ref();
    #[cfg(windows)]
    let s = &s.replace('/', "\\");
    let comp_strs = __parse_path_components_with_braces(s);
    let mut expanded_comps = VecDeque::with_capacity(comp_strs.len());

    for comp in comp_strs {
        let path = if let Some(captures) = ENVVAR_REGEX.captures(&comp.to_string_lossy()) {
            let envvar = captures
                .get(1)
                .and_then(|m| if m.is_empty() { None } else { Some(m.as_str()) })
                .ok_or(ExpandError::EmptyEnvvarCapture)?;

            #[cfg(debug_assertions)]
            println!("expanding envvar '{envvar:?}'");

            let envvar_value = if let Some(value) = std::env::var_os(envvar) {
                #[cfg(debug_assertions)]
                println!("{envvar:?}={}", value.display());

                value
            } else if let Some(fallback) = captures.get(3) {
                let fallback = fallback.as_str();
                #[cfg(debug_assertions)]
                println!("failed to expand '{envvar:?}', found fallback '{fallback:?}'");

                expand_by_path_components(fallback)?.into_os_string()
            } else {
                return Err(ExpandError::EnvvarReadError(envvar.to_string()));
            };

            PathBuf::from(envvar_value)
        } else {
            PathBuf::from(comp)
        }; // if let Some(captures) = ...
        expanded_comps.extend(path.components().map(|c| c.as_os_str().to_os_string()));
    } // for comp in comp_strs

    #[cfg(debug_assertions)]
    println!("comps={expanded_comps:?}");

    if let Some(front) = expanded_comps.front()
        && front.as_os_str() == OsStr::new("~")
    {
        let home = BASE_DIRS.home_dir();
        expanded_comps.pop_front();
        for comp in PathBuf::from(home).components().rev() {
            expanded_comps.push_front(comp.as_os_str().to_os_string());
        }
    }

    // WARN: there is currently a bug with [`PathBuf::from_iter`] where, for whatever reason, it combines the first two components into one.
    // Converting to a string seems to work fine for now.
    let path_str = expanded_comps
        .into_iter()
        .collect::<Vec<_>>()
        .join(OsStr::new(std::path::MAIN_SEPARATOR_STR));
    Ok(PathBuf::from(path_str))
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    const TEST_ENVVAR_KEY: &str = "__SHELLEXPAND_TEST_ENVVAR";
    const TEST_ENVVAR_VALUE: &str = "test_value";

    fn set_and_check_envvar<K: AsRef<OsStr>, V: AsRef<OsStr>>(
        key: K,
        in_value: V,
    ) -> anyhow::Result<String> {
        let key = key.as_ref();
        let in_value = in_value.as_ref();
        let test_envvar_value = unsafe {
            std::env::set_var(key, in_value);
            let out_value =
                std::env::var_os(key).context("failed to get test envvar after setting")?;
            assert_eq!(
                in_value, out_value,
                "failed to set {key:?}={in_value:?}, got '{out_value:?}' instead."
            );

            out_value
        };

        Ok(test_envvar_value.to_string_lossy().to_string())
    }

    fn set_test_envvar() -> anyhow::Result<String> {
        set_and_check_envvar(TEST_ENVVAR_KEY, TEST_ENVVAR_VALUE)
    }

    #[test]
    fn test_expand_string_error() -> anyhow::Result<()> {
        const BAD_KEY: &str = "NO_WAY_YOU_HAVE_DEFINED_THIS";
        set_test_envvar().context("failed to set test envvar")?;
        let test_str = format!("expand this: ${{{BAD_KEY}}}");

        match expand(&test_str) {
            Ok(_) => anyhow::bail!("expansion succeeded unexpectedly"),
            Err(ExpandError::EnvvarReadError(envvar)) if envvar == BAD_KEY => Ok(()),
            Err(err) => anyhow::bail!(err),
        }
    }

    #[test]
    fn test_expand_string_fallback() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = format!("expand this: {TEST_ENVVAR_VALUE}");
        let test_str =
            format!("expand this: ${{NO_WAY_YOU_HAVE_DEFINED_THIS:-${TEST_ENVVAR_KEY}}}");

        let actual = expand(&test_str).context("failed to expand test string")?;
        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_multiple_string() -> anyhow::Result<()> {
        const TEST_KEY2: &str = "__TEST_KEY2";
        const TEST_VALUE2: &str = "test value2";
        set_test_envvar().context("failed to set test envvar")?;
        set_and_check_envvar(TEST_KEY2, TEST_VALUE2).context("failed to set test envvar2")?;
        let expected = format!("expand this: {TEST_ENVVAR_VALUE} {TEST_VALUE2}");
        let test_str = format!("expand this: ${{{TEST_ENVVAR_KEY}}} ${{{TEST_KEY2}}}");

        let actual = expand(&test_str).context("failed to expand test string")?;
        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_string() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = format!("expand this: {TEST_ENVVAR_VALUE}");
        let test_str = format!("expand this: ${{{TEST_ENVVAR_KEY}}}");

        let actual = expand(&test_str).context("failed to expand test string")?;
        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_fails_to_expand_non_existent_envvar() {
        const TEST_ENVVAR: &str = "NO_WAY_YOU_HAVE_DEFINED_THIS";

        match expand_by_path_components(format!("${TEST_ENVVAR}/some/file")) {
            Err(ExpandError::EnvvarReadError(envvar)) => assert_eq!(envvar, TEST_ENVVAR),
            res => panic!("expected error, got {res:?}"),
        }
    }

    #[test]
    fn test_parses_path_with_braces() {
        #[cfg(windows)]
        let expected = vec!["${within\\braces}", "file"];
        #[cfg(not(windows))]
        let expected = vec!["${within/braces}", "file"];
        let path = expected.join(std::path::MAIN_SEPARATOR_STR);
        let actual = __parse_path_components_with_braces(&path);

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_parses_path_with_braces_but_no_dollar_sign() {
        let expected = vec!["{within", "braces}", "file"];
        let path = expected.join(std::path::MAIN_SEPARATOR_STR);
        let actual = __parse_path_components_with_braces(&path);

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_expand_tilde() -> anyhow::Result<()> {
        let home = BASE_DIRS.home_dir();
        let expected = home.join("path").join("to").join("file");
        let actual = expand_by_path_components("~/path/to/file")?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_absolute_path() -> anyhow::Result<()> {
        let home = BASE_DIRS.home_dir();
        let expected = home.join("path").join("to").join("file");
        let actual = expand_by_path_components(expected.to_string_lossy())?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_envvar() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = PathBuf::from(format!("{TEST_ENVVAR_VALUE}/some/file"));
        let actual = expand_by_path_components(format!("${TEST_ENVVAR_KEY}/some/file"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_envvar_in_middle() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;

        let expected = PathBuf::from(format!("path/to/{TEST_ENVVAR_VALUE}/some/file"));
        let actual = expand_by_path_components(format!("path/to/${TEST_ENVVAR_KEY}/some/file"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_envvar_with_braces() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = PathBuf::from(format!("{TEST_ENVVAR_VALUE}/some/file"));
        let actual = expand_by_path_components(format!("${{{TEST_ENVVAR_KEY}}}/some/file"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_fallback_envvar() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = PathBuf::from(format!("{TEST_ENVVAR_VALUE}/some/file"));
        let actual = expand_by_path_components(format!(
            "${{NO_WAY_YOU_HAVE_DEFINED_THIS:-${TEST_ENVVAR_KEY}}}/some/file"
        ))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_nested_fallback_envvars() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = PathBuf::from(format!("{TEST_ENVVAR_VALUE}/some/file"));
        // braces are important! otherwise, it's ambiguous
        let actual = expand_by_path_components(format!(
            "${{MISSING1:-${{MISSING2:-${{MISSING3:-${TEST_ENVVAR_KEY}}}}}}}/some/file"
        ))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_fallback_tilde() -> anyhow::Result<()> {
        let home = BASE_DIRS.home_dir();
        let expected = home.join("some").join("file");
        let actual = expand_by_path_components("${NO_WAY_YOU_HAVE_DEFINED_THIS:-~}/some/file")?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_fallback_has_tilde_components() -> anyhow::Result<()> {
        let home = BASE_DIRS.home_dir();
        let expected = home.join(home).join("some").join("file");

        let actual = expand_by_path_components("${NO_WAY_YOU_HAVE_DEFINED_THIS:-~/some}/file")?;

        assert_eq!(expected, actual);

        Ok(())
    }
}
