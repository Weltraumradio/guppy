//! Directory jump targets: frequently/recently visited directories (own list plus
//! zoxide, if installed) and subdirectories of the current directory, fuzzy-searched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
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
    /// Scores from zoxide, if available; filled in the background at startup.
    zoxide: Arc<OnceLock<HashMap<PathBuf, f64>>>,
    /// Subdirectories of the current directory, scanned in the background.
    nearby: Arc<Mutex<(PathBuf, Vec<PathBuf>)>>,
    indexing: Arc<AtomicBool>,
    /// Checked in the background whether stored targets still exist, so an
    /// unreachable NAS never blocks the search.
    alive: Arc<Mutex<HashMap<PathBuf, bool>>>,
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
        let me = Self {
            visits,
            zoxide: Default::default(),
            nearby: Default::default(),
            indexing: Default::default(),
            alive: Default::default(),
            db,
            home: std::env::var("HOME").unwrap_or_default(),
        };
        me.check_alive();
        me
    }

    /// Checks all stored targets in the background, local ones first.
    /// Also loads the zoxide scores first, so starting zoxide doesn't delay startup.
    fn check_alive(&self) {
        let own: Vec<PathBuf> = self.visits.keys().cloned().collect();
        let alive = self.alive.clone();
        let zoxide = self.zoxide.clone();
        std::thread::spawn(move || {
            let z = zoxide.get_or_init(load_zoxide);
            let mut paths: Vec<PathBuf> = own.into_iter().chain(z.keys().cloned()).collect();
            paths.sort_by_key(|p| crate::platform::is_network(p));
            paths.dedup();
            for p in paths {
                let ok = p.is_dir();
                alive.lock().unwrap().insert(p, ok);
            }
        });
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

    /// Scans subdirectories of `root` in the background (depth 4, max 20,000; depth 2 on network drives).
    pub fn index_nearby(&self, root: &Path) {
        if self.nearby.lock().unwrap().0 == root {
            return;
        }
        let nearby = self.nearby.clone();
        let indexing = self.indexing.clone();
        indexing.store(true, Ordering::Relaxed);
        let root = root.to_path_buf();
        // Search shallower on network drives: every level costs network requests there.
        let (depth, max) = if crate::platform::is_network(&root) { (2, 5_000) } else { (4, 20_000) };
        *nearby.lock().unwrap() = (root.clone(), Vec::new());
        std::thread::spawn(move || {
            let mut found = Vec::new();
            let mut level = vec![root.clone()];
            for _ in 0..depth {
                let mut next = Vec::new();
                for dir in &level {
                    let Ok(rd) = std::fs::read_dir(dir) else { continue };
                    for e in rd.flatten() {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        // Hidden directories and build artifacts are rarely jump targets.
                        let skip = crate::platform::is_hidden(&name, None) || matches!(&*name, "target" | "node_modules" | "__pycache__");
                        if !skip && e.file_type().is_ok_and(|t| t.is_dir()) {
                            next.push(e.path());
                        }
                    }
                }
                found.extend(next.iter().cloned());
                if found.len() > max {
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

    fn zoxide(&self) -> &HashMap<PathBuf, f64> {
        static EMPTY: LazyLock<HashMap<PathBuf, f64>> = LazyLock::new(HashMap::new);
        self.zoxide.get().unwrap_or(&EMPTY)
    }

    /// Are subdirectories currently being scanned?
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
        own + self.zoxide().get(p).copied().unwrap_or(0.0)
    }

    /// Finds directories matching `query`. Empty query: sorted by frecency.
    pub fn search(&self, query: &str, limit: usize) -> Vec<PathBuf> {
        let mut candidates: Vec<PathBuf> = self.visits.keys().chain(self.zoxide().keys()).cloned().collect();
        candidates.extend(self.nearby.lock().unwrap().1.iter().cloned());
        candidates.sort();
        candidates.dedup();
        {
            // Unchecked targets stay; only ones known to be gone are dropped.
            let alive = self.alive.lock().unwrap();
            candidates.retain(|p| alive.get(p) != Some(&false));
        }

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
                // Fuzzy score counts; frecency breaks ties.
                Some((s as f64 + 25.0 * (1.0 + self.frecency(&p)).ln(), p))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().take(limit).map(|(_, p)| p).collect()
    }

    /// Path with `~` instead of the home directory.
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
