//! English / Portuguese (Brazil) texts. The default comes from the build
//! (`--features pt` for the Portuguese installer); the `language` setting in
//! config.toml overrides it at runtime.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En = 0,
    Pt = 1,
}

pub const DEFAULT: Lang = if cfg!(feature = "pt") { Lang::Pt } else { Lang::En };

static CURRENT: AtomicU8 = AtomicU8::new(DEFAULT as u8);

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Pt => "pt",
        }
    }

    pub fn parse(s: &str) -> Lang {
        match s.trim().to_ascii_lowercase().as_str() {
            "pt" | "pt-br" | "pt_br" => Lang::Pt,
            "en" => Lang::En,
            _ => DEFAULT,
        }
    }
}

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn get() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == Lang::Pt as u8 { Lang::Pt } else { Lang::En }
}

/// Title of the OBS dock, per language. The installer recognises both.
pub fn dock_title(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Dynamic Delay",
        Lang::Pt => "Delay dinâmico",
    }
}

/// `t!("english {x}", "português {x}")` formats the text of the current language.
#[macro_export]
macro_rules! t {
    ($en:literal, $pt:literal $(,)?) => {
        match $crate::i18n::get() {
            $crate::i18n::Lang::En => format!($en),
            $crate::i18n::Lang::Pt => format!($pt),
        }
    };
    ($en:literal, $pt:literal, $($arg:tt)+) => {
        match $crate::i18n::get() {
            $crate::i18n::Lang::En => format!($en, $($arg)+),
            $crate::i18n::Lang::Pt => format!($pt, $($arg)+),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_language() {
        let n = 3;
        set(Lang::Pt);
        assert_eq!(crate::t!("{n} files", "{n} arquivos"), "3 arquivos");
        set(Lang::En);
        assert_eq!(crate::t!("{n} files", "{n} arquivos"), "3 files");
        assert_eq!(Lang::parse("PT-BR"), Lang::Pt);
        set(DEFAULT);
    }
}
