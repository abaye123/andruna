//! User-visible strings, one resource file per language in `res/`.
//!
//! The files are `key = value` lines; `\n` in a value is a line break and
//! lines starting with `#` are comments. The language follows the user's
//! Windows display language; English is the fallback for other languages
//! and for keys a translation lacks.

const EN: &str = include_str!("../res/strings.en.txt");
const HE: &str = include_str!("../res/strings.he.txt");

const LANG_HEBREW: u16 = 0x0D;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    He,
}

impl Lang {
    pub fn current() -> Self {
        if crate::sys::ui_primary_language() == LANG_HEBREW {
            Self::He
        } else {
            Self::En
        }
    }

    pub fn is_rtl(self) -> bool {
        self == Self::He
    }

    fn table(self) -> &'static str {
        match self {
            Self::En => EN,
            Self::He => HE,
        }
    }
}

fn lookup(table: &str, key: &str) -> Option<String> {
    table.lines().find_map(|l| {
        let l = l.trim_start_matches('\u{feff}');
        if l.starts_with('#') {
            return None;
        }
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim().replace("\\n", "\n"))
    })
}

/// The string `key` in `lang` with `{name}` placeholders replaced.
pub fn text(lang: Lang, key: &str, args: &[(&str, &str)]) -> String {
    let mut s = lookup(lang.table(), key)
        .or_else(|| lookup(EN, key))
        .unwrap_or_else(|| key.to_string());
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(table: &str) -> Vec<String> {
        let mut k: Vec<String> = table
            .lines()
            .filter(|l| !l.starts_with('#') && l.contains('='))
            .map(|l| l.split_once('=').unwrap().0.trim().to_string())
            .collect();
        k.sort();
        k
    }

    #[test]
    fn every_language_has_every_key() {
        assert!(!keys(EN).is_empty());
        assert_eq!(keys(EN), keys(HE));
    }

    #[test]
    fn placeholders_are_filled() {
        let t = text(Lang::En, "host_title", &[("package", "com.example.app")]);
        assert!(t.contains("com.example.app"), "{t}");
        let t = text(Lang::He, "host_title", &[("package", "com.example.app")]);
        assert!(t.contains("com.example.app"), "{t}");
        assert_eq!(text(Lang::En, "no_such_key", &[]), "no_such_key");
    }
}
