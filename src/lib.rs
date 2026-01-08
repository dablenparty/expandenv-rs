#![warn(clippy::all, clippy::pedantic)]
/*!
This crate is for expanding environment variables in strings with support for fallback values.

## Examples

```rust
# use anyhow::{self, Context};
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

# let test_envvar_value2 = unsafe {
#     std::env::set_var("BAR", "baz");
#     let value = std::env::var("BAR").context("failed to set test envvar")?;
#     assert_eq!(
#         value, "baz",
#         "failed to set BAR=baz, got '{value}' instead."
#     );
#
#     value
# };
// here's where it shines: you can expand an entire string!
let expanded_str = expand("holy mackerel there's a ${MISSING_VAR:-$FOO} and even a $BAR!!")?;
# assert_eq!(
#   format!("holy mackerel there's a {} and even a {test_envvar_value2}!!", test_envvar_value.display()),
#   expanded_str
# );
#
# Ok(())
# }
```
*/

use std::env;

use bstr::{BString, ByteSlice, ByteVec};

use crate::errors::ExpandError;

pub mod errors;

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
    let mut char_iter = s.chars().peekable();

    while let Some(c) = char_iter.next() {
        match c {
            '\\' => {
                // escape next character
                if let Some(c) = char_iter.next() {
                    current_component.push_char(c);
                }
                continue;
            }
            '$' if !parse_as_envvar && char_iter.peek().is_none_or(|c| *c != '$') => {
                if !current_component.is_empty() {
                    // start of envvar, save current component
                    components.push(current_component.clone());
                    current_component.clear();
                }
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

            c if parse_as_envvar && brace_count == 0 && !(c.is_alphanumeric() || c == '_') => {
                // end of envvar without braces, add braces and save as component
                current_component.insert_char(1, '{');
                current_component.push_char('c');
                components.push(current_component.clone());
                current_component.clear();
                current_component.push_char(c);
                parse_as_envvar = false;
                continue;
            }

            _ => {}
        }

        current_component.push_char(c);
    }

    if !current_component.is_empty() {
        components.push(current_component);
    }

    components
}

/// Parse through a byte slice, expanding envvars along the way.
///
/// Environment variables expand into their value, optionally expanding a fallback value if the var
/// cannot be read. Envvars may contain letters, numbers, and underscores (`_`), but they _must_ start
/// with either a letter or an underscore after the dollar sign (`$`). Although more complicated
/// syntax is technically allowed by most programming languages, I will not be supporting anything
/// other than this basic structure because this is what most shells support and if you're doing
/// something different, ask yourself why. A dollar sign preceded by a backslash (`\$`) is escaped
/// and thus not expanded.
///
/// # Arguments
///
/// - `s`: String to expand
///
/// # Errors
///
/// An error is returned if:
///
/// - An envvar cannot be expanded
pub fn expand<S: AsRef<[u8]>>(s: S) -> Result<String, ExpandError> {
    let s = s.as_ref();
    let mut expanded_str = BString::new(Vec::with_capacity(s.len()));
    let mut maybe_fallback = None;
    let comp_strs = dbg!(__parse_string_components(s));

    for comp in comp_strs {
        if comp.is_empty() || comp[0] != b'$' || comp[1] == b'$' {
            expanded_str.push_str(comp);
            continue;
        }

        let envvar_name = if comp[1] == b'{' {
            // remove a brace level
            let inner = &comp[2..comp.len() - 1];
            if let Some((envvar, fallback)) = inner.split_once_str(":-") {
                maybe_fallback = Some(fallback.to_vec());
                envvar
            } else {
                inner
            }
        } else {
            &comp[1..]
        };

        let str_to_add = match env::var(dbg!(envvar_name.to_os_str_lossy())) {
            Ok(value) => value,
            Err(env::VarError::NotPresent) if maybe_fallback.is_some() => {
                // this is guarded
                #[allow(clippy::missing_panics_doc)]
                expand(maybe_fallback.as_ref().unwrap())?
            }
            Err(env::VarError::NotPresent) => {
                return Err(ExpandError::EnvvarReadError(
                    envvar_name.to_str_lossy().to_string(),
                ));
            }
            Err(err) => return Err(err.into()),
        };
        expanded_str.push_str(str_to_add);
    }

    Ok(expanded_str.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
    fn test_ignore_envvar_with_braces_in_middle_of_string() -> anyhow::Result<()> {
        let expected = format!("oh look! ${{{TEST_ENVVAR_KEY}}}! a wild envvar!");
        let actual = expand(format!("oh look! \\${{{TEST_ENVVAR_KEY}}}! a wild envvar!"))?;

        assert_eq!(expected, actual);

        Ok(())
    }

    #[test]
    fn test_ignore_envvar_in_middle_of_string() -> anyhow::Result<()> {
        let expected = format!("oh look! ${TEST_ENVVAR_KEY}! a wild envvar!");
        let actual = expand(format!("oh look! \\${TEST_ENVVAR_KEY}! a wild envvar!"))?;

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
    fn test_expand_fallback() -> anyhow::Result<()> {
        const EXPECTED: &str = "/path/to/file";
        let actual = expand(format!("${{NO_WAY_YOU_HAVE_DEFINED_THIS:-{EXPECTED}}}"))?;

        assert_eq!(EXPECTED, actual);

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
