#![warn(clippy::all, clippy::pedantic)]
/*!
This crate is for expanding environment variables and tilde's (`~`) with support for fallback
values.

## Examples

```rust
# use anyhow::{self, Context};
# use directories_next;
# use std::path::PathBuf;
# use expandenv::expand;
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
let envvar_value = expand("$FOO")?;
# assert_eq!(test_envvar_value, envvar_value);

// if expansion fails, you can return an error...
assert!(expand("$MISSING_VAR").is_err());
// or try to parse a fallback value
let envvar_value = expand("${MISSING_VAR:-some/path}")?;
# assert_eq!(PathBuf::from("some/path"), envvar_value);
// and nest them as much as you want! this example returns the value of `$FOO`
let envvar_value = expand("${MISSING_VAR:-${ANOTHER_MISSING_VAR:-$FOO}}")?;
# assert_eq!(test_envvar_value, envvar_value);

// it's not limited; you can expand an entire path!
// the `~` expands to your home directory for simplicity
let path = expand("~/${MISSING_VAR:-$FOO}/file.txt")?;
# let base_dirs = directories_next::BaseDirs::new().context("failed to find home dir")?;
# let home = base_dirs.home_dir();
# assert_eq!(home.join(TEST_ENVVAR_VALUE).join("file.txt"), path);
#
# Ok(())
# }
```
*/

use std::{path::PathBuf, sync::LazyLock};

use bstr::{BString, ByteSlice, ByteVec};
use directories_next::BaseDirs;
use regex::Regex;

use crate::errors::ExpandError;

pub mod errors;

/// Lazy wrapper around [`directories_next::BaseDirs::new`].
static BASE_DIRS: LazyLock<BaseDirs> =
    LazyLock::new(|| BaseDirs::new().expect("failed to locate users home directory"));

/// Mimic's the behavior of [`PathBuf::components`] by extracting environment variables as their
/// own components in a [`Vec<BString>`].
///
/// # Arguments
///
/// `s` - Input string
fn __parse_string_components<B: AsRef<[u8]>>(s: B) -> Vec<BString> {
    let s = s.as_ref();

    // how many braces have opened
    let mut brace_count: u8 = 0;
    let mut components = vec![];
    let mut current_component = BString::new(vec![]);
    let mut parse_as_envvar = false;

    for c in s.chars() {
        match c {
            '$' => {
                // start of envvar, save current component
                components.push(current_component.clone());
                current_component.clear();
                current_component.push_char(c);
                parse_as_envvar = true;
                continue;
            }

            '{' if parse_as_envvar => brace_count = brace_count.saturating_add(1),

            '}' if parse_as_envvar && brace_count == 1 => {
                // end of braced envvar, save as component
                current_component.push_char(c);
                components.push(current_component.clone());
                current_component.clear();
                parse_as_envvar = false;
                brace_count -= 1;
                continue;
            }
            '}' if parse_as_envvar => brace_count = brace_count.saturating_sub(1),

            c if parse_as_envvar && brace_count == 0 && !c.is_alphanumeric() => {
                // end of envvar without braces, save as component
                current_component.push_char(c);
                components.push(current_component.clone());
                current_component.clear();
                parse_as_envvar = false;
                continue;
            }

            _ => {}
        }

        current_component.push_char(c);
    }

    components.push(current_component);

    components
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
pub fn expand<S: AsRef<[u8]>>(s: S) -> Result<PathBuf, ExpandError> {
    static ENVVAR_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        /*
         * Capture groups:
         * "envvar": The environment variable name
         * "fallback": The fallback value, in its entirety
         */
        Regex::new(r"(?<envvar>[a-zA-Z_]\w*)(?::-(?<fallback>.*?\}))?")
            .expect("invalid envvar regex")
    });

    let bs = bstr::B(s.as_ref());
    let comp_strs = __parse_string_components(bs);

    for comp in comp_strs {
        if !comp[0] == b'$' {
            continue;
        }

        let trimmed = if comp[1] == b'{' {
            // remove surrounding ${...}
            &comp[2..comp.len() - 1]
        } else {
            // remove $...
            &comp[1..]
        };
    }

    // TODO: maybe fancy-regex crate for lookbehind?
    todo!()
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    const TEST_ENVVAR_KEY: &str = "__EXPANDENV_TEST_VAR";
    const TEST_ENVVAR_VALUE: &str = "test_value";

    fn set_test_envvar() -> anyhow::Result<String> {
        let test_envvar_value = unsafe {
            std::env::set_var(TEST_ENVVAR_KEY, TEST_ENVVAR_VALUE);
            let value = std::env::var(TEST_ENVVAR_KEY)
                .context("failed to get test envvar after setting")?;
            assert_eq!(
                TEST_ENVVAR_VALUE, value,
                "failed to set {TEST_ENVVAR_KEY}={TEST_ENVVAR_VALUE}, got '{value}' instead."
            );

            value
        };

        Ok(test_envvar_value)
    }

    #[test]
    fn test_fails_to_expand_non_existent_envvar() {
        const TEST_ENVVAR: &str = "NO_WAY_YOU_HAVE_DEFINED_THIS";

        match expand(format!("${TEST_ENVVAR}")) {
            Err(ExpandError::EnvvarReadError(envvar)) => assert_eq!(envvar, TEST_ENVVAR),
            res => panic!("expected error, got {res:?}"),
        }
    }

    #[test]
    fn test_parses_string_with_braces() {
        let expected = vec!["this is a ", "${within braces}", " string"];
        let expected_str = expected.join("");
        let actual = __parse_string_components(expected_str);

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_parses_string_with_braces_but_no_dollar_sign() {
        let expected = vec!["this is a {within braces} string"];
        let expected_str = expected.join("");
        let actual = __parse_string_components(expected_str);

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_expand_envvar() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = TEST_ENVVAR_VALUE.to_string();
        let actual = expand(format!("${TEST_ENVVAR_KEY}"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_envvar_in_middle_of_path() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;

        let expected = PathBuf::from(format!("path/to/{TEST_ENVVAR_VALUE}/some/file"));
        let actual = expand(format!("path/to/${TEST_ENVVAR_KEY}/some/file"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_envvar_in_middle_of_string() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;

        let expected = format!("oh look! {TEST_ENVVAR_VALUE}! a wild envvar!");
        let actual = expand(format!("oh look! ${TEST_ENVVAR_KEY}! a wild envvar!"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_envvar_with_braces_in_middle_of_string() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;

        let expected = format!("oh look! {TEST_ENVVAR_VALUE}! a wild envvar!");
        let actual = expand(format!("oh look! ${{{TEST_ENVVAR_KEY}}}! a wild envvar!"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_envvar_with_braces() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = TEST_ENVVAR_VALUE.to_string();
        let actual = expand(format!("${{{TEST_ENVVAR_KEY}}}"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_fallback_envvar() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = TEST_ENVVAR_VALUE.to_string();
        let actual = expand(format!(
            "${{NO_WAY_YOU_HAVE_DEFINED_THIS:-${TEST_ENVVAR_KEY}}}"
        ))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_expand_nested_fallback_envvars() -> anyhow::Result<()> {
        set_test_envvar().context("failed to set test envvar")?;
        let expected = TEST_ENVVAR_VALUE.to_string();
        // braces are important! otherwise, it's ambiguous
        let actual = expand(format!(
            "${{MISSING1:-${{MISSING2:-${{MISSING3:-${TEST_ENVVAR_KEY}}}}}}}"
        ))?;

        assert_eq!(expected, actual);

        Ok(())
    }
}
