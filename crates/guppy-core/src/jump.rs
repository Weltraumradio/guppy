//! Ordner-Sprungziele: häufig/kürzlich besuchte Ordner (eigene Liste plus
//! zoxide, falls installiert) und Unterordner des aktuellen Ordners, unscharf durchsucht.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

#[derive(Clone, Copy)]
struct Visit {
    count: f64,
    last: u64,
}

pub struct Jumper {
    visits: HashMap<PathBuf, Visit>,
    /// Punkte aus zoxide, falls vorhanden.
    zoxide: HashMap<PathBuf, f64>,
    /// Unterordner des aktuellen Ordners, im Hintergrund eingelesen.
    nearby: Arc<Mutex<(PathBuf, Vec<PathBuf>)>>,
    indexing: Arc<AtomicBool>,
    db: Option<PathBuf>,
    home: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Jumper {
    pub fn load() -> Self {
        let db = dirs::data_dir().map(|d| d.join("guppy/frecency.tsv"));
        let visits = db
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| {
                s.lines()
                    .filter_map(|l| {
                        let mut it = l.splitn(3, '\t');
                        let count = it.next()?.parse().ok()?;
                        let last = it.next()?.parse().ok()?;
                        Some((PathBuf::from(it.next()?), Visit { count, last }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            visits,
            zoxide: load_zoxide(),
            nearby: Default::default(),
            indexing: Default::default(),
            db,
            home: std::env::var("HOME").unwrap_or_default(),
        }
    }

    pub fn visit(&mut self, dir: &Path) {
        let v = self.visits.entry(dir.to_path_buf()).or_insert(Visit { count: 0.0, last: 0 });
        v.count += 1.0;
        v.last = now();
        self.save();
    }

    fn save(&self) {
        let Some(db) = &self.db else { return };
        let _ = std::fs::create_dir_all(db.parent().unwrap_or(Path::new("/")));
        let out: String = self
            .visits
            .iter()
            .map(|(p, v)| format!("{}\t{}\t{}\n", v.count, v.last, p.display()))
            .collect();
        let _ = std::fs::write(db, out);
    }

    /// Liest Unterordner von `root` (bis Tiefe 4, max. 20 000) im Hintergrund ein.
    pub fn index_nearby(&self, root: &Path) {
        if self.nearby.lock().unwrap().0 == root {
            return;
        }
        let nearby = self.nearby.clone();
        let indexing = self.indexing.clone();
        indexing.store(true, Ordering::Relaxed);
        let root = root.to_path_buf();
        *nearby.lock().unwrap() = (root.clone(), Vec::new());
        std::thread::spawn(move || {
            let mut found = Vec::new();
            let mut level = vec![root.clone()];
            for _ in 0..4 {
                let mut next = Vec::new();
                for dir in &level {
                    let Ok(rd) = std::fs::read_dir(dir) else { continue };
                    for e in rd.flatten() {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        // Versteckte Ordner und Build-Artefakte sind selten Sprungziele.
                        let skip = crate::platform::is_hidden(&name, None) || matches!(&*name, "target" | "node_modules" | "__pycache__");
                        if !skip && e.file_type().is_ok_and(|t| t.is_dir()) {
                            next.push(e.path());
                        }
                    }
                }
                found.extend(next.iter().cloned());
                if found.len() > 20_000 {
                    break;
                }
                level = next;
            }
            let mut n = nearby.lock().unwrap();
            if n.0 == root {
                n.1 = found;
                indexing.store(false, Ordering::Relaxed);
            }
        });
    }

    /// Werden gerade Unterordner eingelesen?
    pub fn indexing(&self) -> bool {
        self.indexing.load(Ordering::Relaxed)
    }

    fn frecency(&self, p: &Path) -> f64 {
        let own = self.visits.get(p).map_or(0.0, |v| {
            let age = now().saturating_sub(v.last);
            let w = match age {
                0..3_600 => 4.0,
                3_600..86_400 => 2.0,
                86_400..604_800 => 1.0,
                _ => 0.5,
            };
            v.count * w
        });
        own + self.zoxide.get(p).copied().unwrap_or(0.0)
    }

    /// Sucht Ordner zu `query`. Leere Suche: nach Häufigkeit sortiert.
    pub fn search(&self, query: &str, limit: usize) -> Vec<PathBuf> {
        let mut candidates: Vec<PathBuf> = self.visits.keys().chain(self.zoxide.keys()).cloned().collect();
        candidates.extend(self.nearby.lock().unwrap().1.iter().cloned());
        candidates.sort();
        candidates.dedup();
        candidates.retain(|p| p.is_dir());

        let query = query.trim();
        if query.is_empty() {
            let mut v: Vec<_> = candidates.into_iter().map(|p| (self.frecency(&p), p)).collect();
            v.sort_by(|a, b| b.0.total_cmp(&a.0));
            return v.into_iter().filter(|(s, _)| *s > 0.0).take(limit).map(|(_, p)| p).collect();
        }

        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let mut buf = Vec::new();
        let mut scored: Vec<(f64, PathBuf)> = candidates
            .into_iter()
            .filter_map(|p| {
                let shown = self.display(&p);
                let s = pattern.score(Utf32Str::new(&shown, &mut buf), &mut matcher)?;
                // Unschärfe-Treffer zählt, Häufigkeit gibt den Ausschlag bei Gleichstand.
                Some((s as f64 + 25.0 * (1.0 + self.frecency(&p)).ln(), p))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().take(limit).map(|(_, p)| p).collect()
    }

    /// Pfad mit `~` statt Home-Verzeichnis.
    pub fn display(&self, p: &Path) -> String {
        let s = p.display().to_string();
        match s.strip_prefix(&self.home) {
            Some(rest) if !self.home.is_empty() => format!("~{rest}"),
            _ => s,
        }
    }
}

fn load_zoxide() -> HashMap<PathBuf, f64> {
    let Ok(out) = std::process::Command::new("zoxide").args(["query", "--list", "--score"]).output() else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (score, path) = l.trim().split_once(' ')?;
            Some((PathBuf::from(path.trim()), score.parse().ok()?))
        })
        .collect()
}
