//! Rules for files that backups leave out, in the syntax of the Windows
//! registry list `FilesNotToBackup`:
//!
//! ```text
//! %SystemRoot%\Memory.dmp
//! \System Volume Information\*{3808876B-C176-4e48-B7AE-04046E6CC752} /s
//! $UserProfile$\AppData\Local\Temp\* /s
//! ```
//!
//! A path is absolute (`C:\…`, possibly after expanding variables) or
//! relative to the root of every volume (`\…`). The last component may
//! contain `*` and `?`; `/s` extends the rule to all subdirectories.

/// One rule, resolved for a particular volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRule {
    /// Directory relative to the volume root, without leading or trailing
    /// backslash; empty for the root itself.
    pub dir: String,
    /// File name pattern with `*` and `?`, matched case-insensitively.
    pub pattern: String,
    /// Also match in all subdirectories of `dir`.
    pub recursive: bool,
}

impl PathRule {
    /// Resolves one registry entry for the volume mounted at `mount_points`
    /// (e.g. `["C:\\"]`). `expand` replaces `%NAME%` variables; `profiles`
    /// are the absolute user profile folders `$UserProfile$` stands for.
    /// Returns no rules if the entry does not concern this volume.
    pub fn parse(
        entry: &str,
        mount_points: &[String],
        profiles: &[String],
        expand: impl Fn(&str) -> Option<String>,
    ) -> Vec<PathRule> {
        let entry = entry.trim();
        let (path, recursive) = match entry.len().checked_sub(3) {
            Some(cut) if entry[cut..].eq_ignore_ascii_case(" /s") => {
                (entry[..cut].trim_end(), true)
            }
            _ => (entry, false),
        };
        let Some(path) = expand_variables(path, &expand) else {
            return Vec::new();
        };

        let paths: Vec<String> = if starts_with_ignore_case(&path, "$UserProfile$") {
            let rest = &path["$UserProfile$".len()..];
            profiles
                .iter()
                .map(|profile| format!("{}{rest}", profile.trim_end_matches('\\')))
                .collect()
        } else {
            vec![path]
        };

        paths
            .iter()
            .filter_map(|path| relative_to_volume(path, mount_points))
            .filter_map(|relative| {
                let relative = relative.trim_matches('\\');
                let (dir, pattern) = relative.rsplit_once('\\').unwrap_or(("", relative));
                // DOS semantics: "*.*" also matches names without a dot.
                let pattern = if pattern == "*.*" { "*" } else { pattern };
                (!pattern.is_empty()).then(|| PathRule {
                    dir: dir.to_string(),
                    pattern: pattern.to_string(),
                    recursive,
                })
            })
            .collect()
    }

    pub fn matches(&self, file_name: &str) -> bool {
        wildcard_match(&self.pattern, file_name)
    }

    /// True if the pattern names one thing rather than matching many.
    pub fn is_literal(&self) -> bool {
        !self.pattern.contains(['*', '?'])
    }
}

/// Replaces `%NAME%` with `expand(NAME)`; `None` if a variable is unknown.
fn expand_variables(text: &str, expand: &impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        let end = start + 1 + rest[start + 1..].find('%')?;
        result.push_str(&rest[..start]);
        result.push_str(&expand(&rest[start + 1..end])?);
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Some(result)
}

/// `C:\x` on a volume mounted at `C:\` becomes `\x`; `\x` stays.
fn relative_to_volume(path: &str, mount_points: &[String]) -> Option<String> {
    if path.starts_with('\\') && !path.starts_with("\\\\") {
        return Some(path.to_string());
    }
    mount_points.iter().find_map(|mount| {
        let mount = mount.trim_end_matches('\\');
        let rest = path.get(mount.len()..)?;
        (starts_with_ignore_case(path, mount) && (rest.is_empty() || rest.starts_with('\\')))
            .then(|| format!("\\{}", rest.trim_start_matches('\\')))
    })
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// `*` matches any run of characters, `?` exactly one; case-insensitive.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let name: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    let (mut p, mut n) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                backtrack = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match backtrack {
                // Let the last `*` swallow one more character.
                Some((star, matched)) => {
                    p = star + 1;
                    n = matched + 1;
                    backtrack = Some((star, matched + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name.to_ascii_lowercase().as_str() {
            "systemroot" | "windir" => Some(r"C:\Windows".into()),
            "temp" => Some(r"C:\Users\anna\AppData\Local\Temp".into()),
            _ => None,
        }
    }

    fn parse(entry: &str) -> Vec<PathRule> {
        let mounts = [r"C:\".to_string()];
        let profiles = [r"C:\Users\anna".to_string(), r"C:\Users\ben".to_string()];
        PathRule::parse(entry, &mounts, &profiles, env)
    }

    fn rule(dir: &str, pattern: &str, recursive: bool) -> PathRule {
        PathRule {
            dir: dir.into(),
            pattern: pattern.into(),
            recursive,
        }
    }

    #[test]
    fn root_relative_entry_applies_to_every_volume() {
        assert_eq!(
            parse(r"\System Volume Information\*{3808876B-C176-4e48-B7AE-04046E6CC752} /s"),
            vec![rule(
                "System Volume Information",
                "*{3808876B-C176-4e48-B7AE-04046E6CC752}",
                true
            )]
        );
        assert_eq!(
            parse(r"\Pagefile.sys"),
            vec![rule("", "Pagefile.sys", false)]
        );
    }

    #[test]
    fn variables_are_expanded_and_mapped_to_the_volume() {
        assert_eq!(
            parse(r"%SystemRoot%\Memory.dmp"),
            vec![rule("Windows", "Memory.dmp", false)]
        );
        assert_eq!(
            parse(r"%windir%\softwaredistribution\*.* /S"),
            vec![rule(r"Windows\softwaredistribution", "*", true)]
        );
        assert!(parse(r"%UNKNOWN%\x").is_empty());
    }

    #[test]
    fn user_profile_expands_to_every_profile() {
        assert_eq!(
            parse(r"$UserProfile$\AppData\Local\Temp\* /s"),
            vec![
                rule(r"Users\anna\AppData\Local\Temp", "*", true),
                rule(r"Users\ben\AppData\Local\Temp", "*", true),
            ]
        );
    }

    #[test]
    fn entries_on_other_volumes_are_ignored() {
        assert!(parse(r"D:\data\x.log").is_empty());
        assert!(parse(r"\\server\share\x").is_empty());
        assert_eq!(
            parse(r"c:\windows\x.log"),
            vec![rule("windows", "x.log", false)]
        );
        // "C:\Windowsold" must not count as inside "C:\Windows".
        let mounts = [r"C:\Windows\".to_string()];
        assert!(PathRule::parse(r"C:\Windowsold\x", &mounts, &[], env).is_empty());
    }

    #[test]
    fn wildcards_match_like_windows() {
        assert!(wildcard_match("*", "anything.txt"));
        assert!(wildcard_match("*.dmp", "MEMORY.DMP"));
        assert!(wildcard_match(
            "*{3808876B-C176-4e48-B7AE-04046E6CC752}",
            "{abc}{3808876b-c176-4e48-b7ae-04046e6cc752}"
        ));
        assert!(wildcard_match("a?c", "abc"));
        assert!(wildcard_match("*a*b*", "xxaxxbxx"));
        assert!(!wildcard_match("*.dmp", "memory.dmp.bak"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(!wildcard_match("pagefile.sys", "swapfile.sys"));
    }
}
