//! Watches the open directories and the Omarchy theme for changes.

use std::path::PathBuf;
use std::time::Duration;

use iced::Subscription;
use iced::futures::{SinkExt, StreamExt};
use notify_debouncer_mini::notify::{self, EventKind, RecursiveMode, Watcher};

/// What is watched; when it changes, watching restarts.
#[derive(Hash, Clone)]
pub struct Targets {
    pub dirs: Vec<PathBuf>,
}

/// Yields changed paths, batched (200 ms).
pub fn changes(targets: Targets) -> Subscription<Vec<PathBuf>> {
    Subscription::run_with(targets, |t| {
        let dirs = t.dirs.clone();
        iced::stream::channel(16, async move |mut out: iced::futures::channel::mpsc::Sender<Vec<PathBuf>>| {
            let (tx, mut rx) = iced::futures::channel::mpsc::unbounded();
            let Ok(mut watcher) = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                // Only real changes: read accesses (e.g. from our own size
                // computation) would otherwise trigger endless recomputation.
                if let Ok(e) = res
                    && matches!(e.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_))
                    && !matches!(e.kind, EventKind::Modify(notify::event::ModifyKind::Metadata(notify::event::MetadataKind::AccessTime)))
                {
                    let _ = tx.unbounded_send(e.paths);
                }
            }) else {
                return;
            };
            for d in &dirs {
                // Not recursive: changes deep in subdirectories are covered by Ctrl+R.
                let _ = watcher.watch(d, RecursiveMode::NonRecursive);
            }
            // Pass on in batches: collect for 200 ms after the first event.
            while let Some(mut paths) = rx.next().await {
                smol::Timer::after(Duration::from_millis(200)).await;
                while let Ok(more) = rx.try_recv() {
                    paths.extend(more);
                }
                paths.sort();
                paths.dedup();
                if out.send(paths).await.is_err() {
                    break;
                }
            }
            drop(watcher);
        })
    })
}
