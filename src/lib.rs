//! Search engine. No GUI dependencies when built with --no-default-features.
use encoding_rs::{UTF_16BE, UTF_16LE, WINDOWS_1251};
use regex::{Regex, RegexBuilder};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc,
    },
};
use walkdir::WalkDir;

pub const DEFAULT_EXTENSIONS: &str = "txt log md rst rs py cs php js ts jsx tsx java c cpp h hpp go rb swift kt sh bat ps1 sql html htm css scss xml svg json yaml yml toml ini cfg config env";
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Encoding {
    Auto,
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1251,
}
impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Авто",
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Windows1251 => "Windows-1251",
        }
    }
}
#[derive(Clone)]
pub struct Options {
    pub roots: Vec<PathBuf>,
    pub query: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub invert: bool,
    pub recursive: bool,
    pub extensions: String,
    pub encoding: Encoding,
    pub max_mb: u64,
    pub max_results: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            roots: vec![],
            query: String::new(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            invert: false,
            recursive: true,
            extensions: DEFAULT_EXTENSIONS.into(),
            encoding: Encoding::Auto,
            max_mb: 50,
            max_results: 100_000,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
    pub encoding: String,
}
#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub scanned: usize,
    pub skipped: usize,
    pub matched_lines: usize,
    pub stopped: bool,
    pub limited: bool,
}
pub enum Event {
    Hits(Vec<Hit>),
    Warning(String),
    Progress(Stats),
    Done(Stats),
}
pub fn matcher(o: &Options) -> Result<Regex, String> {
    if o.query.is_empty() {
        return Err("Введіть текст для пошуку.".into());
    }
    let mut pattern = if o.regex {
        o.query.clone()
    } else {
        regex::escape(&o.query)
    };
    if o.whole_word {
        pattern = format!(r"\b(?:{})\b", pattern);
    }
    RegexBuilder::new(&pattern)
        .case_insensitive(!o.case_sensitive)
        .build()
        .map_err(|e| format!("Некоректний регулярний вираз: {e}"))
}
pub fn allowed(path: &Path, filters: &str) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    filters
        .split(|c: char| c.is_whitespace() || c == ';' || c == ',')
        .filter(|s| !s.is_empty())
        .any(|s| {
            let s = s.trim().to_lowercase();
            s == "*"
                || s == "*.*"
                || s == name
                || s.trim_start_matches("*.").trim_start_matches('.') == ext
                || (s == "env" && name == ".env")
        })
}
pub fn decode(bytes: &[u8], encoding: Encoding) -> Result<(String, &'static str), String> {
    let (enc, data) = match encoding {
        Encoding::Auto if bytes.starts_with(&[0xff, 0xfe]) => (Encoding::Utf16Le, &bytes[2..]),
        Encoding::Auto if bytes.starts_with(&[0xfe, 0xff]) => (Encoding::Utf16Be, &bytes[2..]),
        Encoding::Auto => {
            let data = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
            if let Ok(s) = std::str::from_utf8(data) {
                if s.contains('\0') {
                    return Err(
                        "Двійковий файл або UTF-16 без BOM: оберіть кодування вручну".into(),
                    );
                }
                return Ok((s.into(), "UTF-8"));
            }
            (Encoding::Windows1251, data)
        }
        Encoding::Utf8 => (
            encoding,
            bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes),
        ),
        Encoding::Utf16Le => (encoding, bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes)),
        Encoding::Utf16Be => (encoding, bytes.strip_prefix(&[0xfe, 0xff]).unwrap_or(bytes)),
        _ => (encoding, bytes),
    };
    if matches!(enc, Encoding::Utf16Le | Encoding::Utf16Be) && data.len() % 2 != 0 {
        return Err("Непарна кількість байтів UTF-16".into());
    }
    let text = match enc {
        Encoding::Utf8 => String::from_utf8(data.to_vec()).map_err(|_| "Некоректний UTF-8")?,
        _ => {
            let decoder = match enc {
                Encoding::Utf16Le => UTF_16LE,
                Encoding::Utf16Be => UTF_16BE,
                _ => WINDOWS_1251,
            };
            let (s, _, bad) = decoder.decode(data);
            if bad {
                return Err("Пошкоджене або несумісне кодування".into());
            }
            s.into_owned()
        }
    };
    if text.contains('\0') {
        return Err("Двійковий вміст пропущено".into());
    }
    Ok((text, enc.label()))
}
pub fn search(o: Options, re: Regex, cancel: Arc<AtomicBool>, tx: SyncSender<Event>) {
    let mut stats = Stats::default();
    let mut seen = HashSet::new();
    let mut batch = Vec::new();
    'roots: for root in &o.roots {
        let walker =
            WalkDir::new(root)
                .follow_links(false)
                .max_depth(if root.is_file() || !o.recursive {
                    1
                } else {
                    usize::MAX
                });
        for entry in walker {
            if cancel.load(Ordering::Relaxed) {
                stats.stopped = true;
                break 'roots;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    stats.skipped += 1;
                    if tx.send(Event::Warning(e.to_string())).is_err() {
                        return;
                    }
                    continue;
                }
            };
            if !entry.file_type().is_file() || !allowed(entry.path(), &o.extensions) {
                continue;
            }
            let path = entry.path();
            let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            if !seen.insert(key) {
                continue;
            }
            let loaded = (|| -> Result<(String, &'static str), String> {
                let file = File::open(path).map_err(|e| e.to_string())?;
                let cap = o.max_mb.max(1).saturating_mul(1024 * 1024);
                if file.metadata().map_err(|e| e.to_string())?.len() > cap {
                    return Err(format!("Розмір перевищує {} МБ", o.max_mb));
                }
                let mut bytes = Vec::new();
                file.take(cap + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if bytes.len() as u64 > cap {
                    return Err("Файл збільшився понад ліміт".into());
                }
                decode(&bytes, o.encoding)
            })();
            match loaded {
                Err(e) => {
                    stats.skipped += 1;
                    if tx
                        .send(Event::Warning(format!("{}: {e}", path.display())))
                        .is_err()
                    {
                        return;
                    }
                }
                Ok((text, encoding)) => {
                    stats.scanned += 1;
                    for (index, line) in text.lines().enumerate() {
                        if cancel.load(Ordering::Relaxed) {
                            stats.stopped = true;
                            break 'roots;
                        }
                        if re.is_match(line) != o.invert {
                            stats.matched_lines += 1;
                            batch.push(Hit {
                                path: path.into(),
                                line: index + 1,
                                text: line.into(),
                                encoding: encoding.into(),
                            });
                            if batch.len() >= 128
                                && tx.send(Event::Hits(std::mem::take(&mut batch))).is_err()
                            {
                                return;
                            }
                            if stats.matched_lines >= o.max_results.max(1) {
                                stats.limited = true;
                                break 'roots;
                            }
                        }
                    }
                }
            }
            if tx.send(Event::Progress(stats.clone())).is_err() {
                return;
            }
        }
    }
    if !batch.is_empty() && tx.send(Event::Hits(batch)).is_err() {
        return;
    }
    let _ = tx.send(Event::Done(stats));
}
pub fn export(path: &Path, hits: &[Hit], csv_format: bool) -> Result<(), String> {
    let mut file = File::create(path).map_err(|e| e.to_string())?;
    file.write_all(&[0xef, 0xbb, 0xbf])
        .map_err(|e| e.to_string())?;
    if csv_format {
        let mut w = csv::WriterBuilder::new().delimiter(b';').from_writer(file);
        w.write_record(["Файл", "Рядок", "Текст", "Кодування"])
            .map_err(|e| e.to_string())?;
        for h in hits {
            w.write_record([
                h.path.to_string_lossy().as_ref(),
                &h.line.to_string(),
                &h.text,
                &h.encoding,
            ])
            .map_err(|e| e.to_string())?;
        }
        w.flush().map_err(|e| e.to_string())?;
    } else {
        for h in hits {
            writeln!(file, "{}:{}: {}", h.path.display(), h.line, h.text)
                .map_err(|e| e.to_string())?
        }
        file.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_case_and_whole_word() {
        let o = Options {
            query: "помилка".into(),
            whole_word: true,
            ..Default::default()
        };
        let r = matcher(&o).unwrap();
        assert!(r.is_match("ПОМИЛКА сталася"));
        assert!(!r.is_match("помилками"));
    }
    #[test]
    fn literal_and_invalid_regex() {
        let mut o = Options {
            query: "[".into(),
            ..Default::default()
        };
        assert!(matcher(&o).unwrap().is_match("["));
        o.regex = true;
        assert!(matcher(&o).is_err());
    }
    #[test]
    fn encodings() {
        let s = "Україна: помилка";
        let (b, _, _) = WINDOWS_1251.encode(s);
        assert_eq!(decode(&b, Encoding::Auto).unwrap().0, s);
        for (enc, big) in [(Encoding::Utf16Le, false), (Encoding::Utf16Be, true)] {
            let mut b = if big {
                vec![0xfe, 0xff]
            } else {
                vec![0xff, 0xfe]
            };
            for c in s.encode_utf16() {
                b.extend(if big {
                    c.to_be_bytes()
                } else {
                    c.to_le_bytes()
                })
            }
            assert_eq!(decode(&b, Encoding::Auto).unwrap().0, s);
            assert_eq!(decode(&b, enc).unwrap().0, s);
        }
        assert!(decode(b"a\0b", Encoding::Auto).is_err());
    }
    #[test]
    fn extensions() {
        assert!(allowed(Path::new("a.RS"), "*.rs;txt"));
        assert!(allowed(Path::new(".env"), DEFAULT_EXTENSIONS));
        assert!(!allowed(Path::new("a.pdf"), DEFAULT_EXTENSIONS));
    }
    #[test]
    fn search_integration() {
        let dir = std::env::temp_dir().join(format!("mygrep-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "помилка\nOK\nПомилка\n").unwrap();
        std::fs::write(dir.join("sub/b.rs"), "помилка").unwrap();
        std::fs::write(dir.join("skip.pdf"), "помилка").unwrap();
        let o = Options {
            roots: vec![dir.clone(), dir.join("a.txt")],
            query: "помилка".into(),
            ..Default::default()
        };
        let re = matcher(&o).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(100);
        search(o.clone(), re, Arc::new(AtomicBool::new(false)), tx);
        let events: Vec<_> = rx.iter().collect();
        let done = events
            .iter()
            .find_map(|e| {
                if let Event::Done(s) = e {
                    Some(s)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(done.scanned, 2);
        assert_eq!(done.matched_lines, 3);
        let mut limited = o.clone();
        limited.max_results = 1;
        let (tx, rx) = std::sync::mpsc::sync_channel(100);
        search(
            limited.clone(),
            matcher(&limited).unwrap(),
            Arc::new(AtomicBool::new(false)),
            tx,
        );
        assert!(rx
            .iter()
            .any(|e| matches!(e, Event::Done(Stats { limited: true, .. }))));
        let (tx, rx) = std::sync::mpsc::sync_channel(100);
        search(
            o.clone(),
            matcher(&o).unwrap(),
            Arc::new(AtomicBool::new(true)),
            tx,
        );
        assert!(rx
            .iter()
            .any(|e| matches!(e, Event::Done(Stats { stopped: true, .. }))));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn csv_quoting() {
        let p = std::env::temp_dir().join(format!("mygrep-export-{}.csv", std::process::id()));
        export(
            &p,
            &[Hit {
                path: "a.txt".into(),
                line: 2,
                text: "a;\"b\"".into(),
                encoding: "UTF-8".into(),
            }],
            true,
        )
        .unwrap();
        let bytes = std::fs::read(&p).unwrap();
        let mut r = csv::ReaderBuilder::new()
            .delimiter(b';')
            .from_reader(&bytes[3..]);
        assert_eq!(&r.records().next().unwrap().unwrap()[2], "a;\"b\"");
        std::fs::remove_file(p).unwrap();
    }
}
