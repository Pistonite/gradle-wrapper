//! Minimal Java `.properties` parser, plus Gradle version extraction.

use std::collections::BTreeMap;

use cu::pre::*;

/// Type guard for a version string that is validated
#[derive(Deref, Debug, Clone, Display)]
#[display("{}", self.0)]
pub struct ValidatedVersion(String);
impl ValidatedVersion {
    /// Pull the Gradle version out of a distribution URL.
    ///
    /// Accepts both `-bin.zip` and `-all.zip`; the caller always downloads `-bin`.
    /// Only the file name is inspected, so the host portion is irrelevant — we never
    /// use the URL itself.
    pub fn try_from_url(url: &str) -> cu::Result<Self> {
        let file = url.rsplit('/').next().unwrap_or(url);
        let rest = cu::check!(
            file.strip_prefix("gradle-"),
            "distribution file name {file:?} does not start with 'gradle-'"
        )?;
        let version = rest
            .strip_suffix("-bin.zip")
            .or_else(|| rest.strip_suffix("-all.zip"));
        let version = cu::check!(
            version,
            "distribution file name {file:?} does not end with '-bin.zip' or '-all.zip'"
        )?;
        Self::try_new(version.to_string())
    }
    /// Validate the version, rejecting anything that isn't a plausible Gradle version.
    ///
    /// This is a security check, not a cosmetic one: the version is interpolated
    /// straight into a services.gradle.org URL and into cache file names, so a value
    /// containing `/`, `..`, or a query separator could redirect the download or
    /// escape the cache directory.
    pub fn try_new(v: String) -> cu::Result<Self> {
        if v.is_empty() {
            cu::bail!("empty version");
        }
        if !v.starts_with(|c: char| c.is_ascii_digit()) {
            cu::bail!("version {v:?} does not start with a digit");
        }
        // Covers 9.6.1, 8.5, 9.0.0-rc-1, 8.0-milestone-2.
        if let Some(bad) = v.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
            cu::bail!("version {v:?} contains an illegal character at byte {bad}");
        }
        if v.contains("..") {
            cu::bail!("version {v:?} contains '..'");
        }
        Ok(Self(v))
    }
}

/// Parse Java `.properties` text into key/value pairs.
///
/// Implements the parts of the format Gradle actually emits: `#`/`!` comments,
/// `=`/`:`/whitespace separators, backslash escapes, and line continuations.
pub fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut lines = text.lines().peekable();

    while let Some(first) = lines.next() {
        let trimmed = first.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }

        // Join continuation lines: a line ends with a continuation if it has an
        // odd number of trailing backslashes.
        let mut logical = trimmed.to_string();
        while ends_with_odd_backslash(&logical) {
            logical.pop(); // drop the trailing backslash
            match lines.next() {
                // Leading whitespace on a continuation line is discarded.
                Some(next) => logical.push_str(next.trim_start()),
                None => break,
            }
        }

        if let Some((k, v)) = split_pair(&logical) {
            out.insert(k, v);
        }
    }

    out
}

fn ends_with_odd_backslash(s: &str) -> bool {
    s.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1
}

/// Split one logical line into an unescaped key and value.
fn split_pair(line: &str) -> Option<(String, String)> {
    let mut key = String::new();
    let mut chars = line.chars().peekable();
    let mut sep_seen = false;

    // Key runs until the first unescaped separator.
    while let Some(c) = chars.next() {
        match c {
            '\\' => push_escape(&mut key, &mut chars),
            '=' | ':' => {
                sep_seen = true;
                break;
            }
            c if c.is_whitespace() => break,
            c => key.push(c),
        }
    }

    if key.is_empty() {
        return None;
    }

    // Skip whitespace, then at most one separator, then more whitespace.
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if !sep_seen && (c == '=' || c == ':') {
            sep_seen = true;
            chars.next();
        } else {
            break;
        }
    }

    let mut value = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => push_escape(&mut value, &mut chars),
            c => value.push(c),
        }
    }

    Some((key, value))
}

/// Handle one backslash escape, having already consumed the backslash.
fn push_escape(out: &mut String, chars: &mut std::iter::Peekable<std::str::Chars>) {
    let Some(c) = chars.next() else { return };
    match c {
        't' => out.push('\t'),
        'n' => out.push('\n'),
        'r' => out.push('\r'),
        'f' => out.push('\u{000C}'),
        'u' => {
            let hex: String = chars.by_ref().take(4).collect();
            match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                Some(decoded) => out.push(decoded),
                // Malformed \u: keep the text rather than losing it silently.
                None => {
                    out.push_str("\\u");
                    out.push_str(&hex);
                }
            }
        }
        // Everything else, including \: \= \\ and \<space>, is a literal.
        c => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim output of `gradle wrapper --gradle-version 9.6.1`
    const REAL: &str = "\
distributionBase=GRADLE_USER_HOME
distributionPath=wrapper/dists
distributionUrl=https\\://services.gradle.org/distributions/gradle-9.6.1-bin.zip
networkTimeout=10000
retries=0
retryBackOffMs=500
validateDistributionUrl=true
zipStoreBase=GRADLE_USER_HOME
zipStorePath=wrapper/dists
";

    fn parse_get(text: &str, key: &str) -> Option<String> {
        parse(text).get(key).cloned()
    }

    #[test]
    fn parses_real_gradle_output() {
        // The escaped colon must survive as a normal URL.
        assert_eq!(
            parse_get(REAL, "distributionUrl").as_deref(),
            Some("https://services.gradle.org/distributions/gradle-9.6.1-bin.zip")
        );
        assert_eq!(parse_get(REAL, "networkTimeout").as_deref(), Some("10000"));
        assert_eq!(parse_get(REAL, "missing"), None);
    }

    #[test]
    fn extracts_version_from_real_output() -> cu::Result<()> {
        let url = parse_get(REAL, "distributionUrl").unwrap();
        assert_eq!(ValidatedVersion::try_from_url(&url)?.as_str(), "9.6.1");
        Ok(())
    }

    #[test]
    fn accepts_all_distribution() {
        assert_eq!(
            ValidatedVersion::try_from_url(
                "https://services.gradle.org/distributions/gradle-8.5-all.zip"
            )
            .unwrap()
            .as_str(),
            "8.5"
        );
    }

    #[test]
    fn accepts_prerelease_versions() -> cu::Result<()> {
        for (url, want) in [
            ("d/gradle-9.0.0-rc-1-bin.zip", "9.0.0-rc-1"),
            ("d/gradle-8.0-milestone-2-bin.zip", "8.0-milestone-2"),
        ] {
            assert_eq!(
                ValidatedVersion::try_from_url(url)?.as_str(),
                want,
                "for {url}"
            );
        }
        Ok(())
    }

    #[test]
    fn colon_and_whitespace_separators() {
        assert_eq!(parse_get("a:1", "a").as_deref(), Some("1"));
        assert_eq!(parse_get("a 1", "a").as_deref(), Some("1"));
        assert_eq!(parse_get("a = 1", "a").as_deref(), Some("1"));
        assert_eq!(parse_get("  a   :   1  ", "a").as_deref(), Some("1  "));
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let text = "# a=1\n\n! b=2\n   # c=3\nd=4\n";
        assert_eq!(parse_get(text, "a"), None);
        assert_eq!(parse_get(text, "b"), None);
        assert_eq!(parse_get(text, "c"), None);
        assert_eq!(parse_get(text, "d").as_deref(), Some("4"));
    }

    #[test]
    fn escapes_and_continuations() {
        assert_eq!(parse_get(r"a=x\=y", "a").as_deref(), Some("x=y"));
        assert_eq!(parse_get(r"a\:b=v", r"a:b").as_deref(), Some("v"));
        assert_eq!(parse_get(r"a=x\\y", "a").as_deref(), Some(r"x\y"));
        assert_eq!(parse_get(r"a=x\ty", "a").as_deref(), Some("x\ty"));
        assert_eq!(parse_get(r"a=A", "a").as_deref(), Some("A"));
        // Trailing backslash continues onto the next line, indent stripped.
        assert_eq!(parse_get("a=one\\\n   two", "a").as_deref(), Some("onetwo"));
        // An even number of backslashes is not a continuation.
        assert_eq!(parse_get("a=one\\\\\nb=2", "a").as_deref(), Some(r"one\"));
    }

    #[test]
    fn rejects_unparseable_urls() {
        // A URL with no recognisable version must error, never guess.
        for bad in [
            "https://evil.example.com/gradle.zip",
            "https://services.gradle.org/distributions/gradle-9.6.1.zip",
            "https://services.gradle.org/distributions/notgradle-9.6.1-bin.zip",
            "",
        ] {
            assert!(
                ValidatedVersion::try_from_url(bad).is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_malicious_versions() {
        // The version is interpolated into a URL and into cache paths, so these
        // must not survive validation.
        for bad in [
            "d/gradle-../../etc/passwd-bin.zip",
            "d/gradle-9.6.1?x=y-bin.zip",
            "d/gradle-9.6.1/../evil-bin.zip",
            "d/gradle--bin.zip",
        ] {
            assert!(
                ValidatedVersion::try_from_url(bad).is_err(),
                "should reject {bad:?}"
            );
        }
    }
}
