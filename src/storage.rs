use crate::model::{SharedAppState, SkipEvent, ThresholdConfig, MAX_IN_MEMORY_EVENTS};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedStore {
    pub config: ThresholdConfig,
    pub events: Vec<SkipEvent>,
}

fn dir_is_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".write_probe_{}", std::process::id()));
    match fs::write(&probe, b"") {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub fn default_store_path() -> PathBuf {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            if let Some(target_dir) = exe_dir.parent() {
                if let Some(project_root) = target_dir.parent() {
                    if project_root.join("Cargo.toml").is_file() {
                        return project_root.join("data").join("mouseskip_store.json");
                    }
                }
            }

            let portable_dir = exe_dir.join("data");
            if dir_is_writable(&portable_dir) {
                return portable_dir.join("mouseskip_store.json");
            }
        }
    }

    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("MouseSkip")
            .join("mouseskip_store.json");
    }

    PathBuf::from("data").join("mouseskip_store.json")
}

pub fn load_store(path: &Path) -> PersistedStore {
    let Ok(bytes) = fs::read(path) else {
        return PersistedStore::default();
    };

    match serde_json::from_slice::<PersistedStore>(&bytes) {
        Ok(mut store) => {
            store.config.clamp_in_place();
            if store.events.len() > MAX_IN_MEMORY_EVENTS {
                let overflow = store.events.len() - MAX_IN_MEMORY_EVENTS;
                store.events.drain(0..overflow);
            }
            store
        }
        Err(err) => {
            let backup_path = path.with_extension("json.corrupt");
            eprintln!(
                "Warning: Failed to parse store at {} ({err}). Backing up to {} and using defaults.",
                path.display(),
                backup_path.display()
            );
            let _ = fs::rename(path, &backup_path);
            PersistedStore::default()
        }
    }
}

pub fn save_store(path: &Path, config: &ThresholdConfig, events: &[SkipEvent]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let trimmed_events: Vec<SkipEvent> = if events.len() > MAX_IN_MEMORY_EVENTS {
        events[events.len() - MAX_IN_MEMORY_EVENTS..].to_vec()
    } else {
        events.to_vec()
    };

    let mut cfg = config.clone();
    cfg.clamp_in_place();

    let payload = PersistedStore {
        config: cfg,
        events: trimmed_events,
    };

    let json = serde_json::to_vec_pretty(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, &json)?;
    if fs::rename(&tmp_path, path).is_err() {
        fs::write(path, &json)?;
        let _ = fs::remove_file(&tmp_path);
    }

    Ok(())
}

fn snapshot_and_write(state: &SharedAppState, path: &Path) -> io::Result<()> {
    let _io_guard = state
        .store_io_lock
        .lock()
        .map_err(|_| io::Error::other("store_io_lock poisoned"))?;

    state.dirty.store(false, Ordering::Release);

    let cfg = state
        .config
        .read()
        .map_err(|_| io::Error::other("config lock poisoned"))?
        .clone();
    let events = state
        .events
        .lock()
        .map_err(|_| io::Error::other("events lock poisoned"))?
        .clone();

    save_store(path, &cfg, &events)
}

pub fn flush_store_now(state: &SharedAppState, path: &Path) {
    if let Err(err) = snapshot_and_write(state, path) {
        eprintln!("Failed to flush MouseSkip store: {err}");
        state.dirty.store(true, Ordering::Release);
    }
}

pub struct PersistenceGuard {
    stop_pair: Arc<(Mutex<bool>, Condvar)>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl PersistenceGuard {
    pub fn shutdown(mut self) {
        self.signal_stop();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }

    fn signal_stop(&self) {
        let (lock, cvar) = &*self.stop_pair;
        if let Ok(mut stopped) = lock.lock() {
            *stopped = true;
            cvar.notify_all();
        }
    }
}

impl Drop for PersistenceGuard {
    fn drop(&mut self) {
        self.signal_stop();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn spawn_persistence_worker(state: Arc<SharedAppState>, path: PathBuf) -> PersistenceGuard {
    let stop_pair = Arc::new((Mutex::new(false), Condvar::new()));
    let stop_clone = Arc::clone(&stop_pair);

    let join_handle = thread::Builder::new()
        .name("mouseskip-persistence".to_string())
        .spawn(move || {
            let (lock, cvar) = &*stop_clone;
            loop {
                let Ok(guard) = lock.lock() else {
                    break;
                };
                if *guard {
                    break;
                }
                let (guard, _) = match cvar.wait_timeout(guard, Duration::from_secs(3)) {
                    Ok(res) => res,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if *guard {
                    break;
                }
                drop(guard);

                if state.dirty.load(Ordering::Acquire) {
                    if let Err(err) = snapshot_and_write(&state, &path) {
                        eprintln!("Failed to persist MouseSkip store: {err}");
                        state.dirty.store(true, Ordering::Release);
                    }
                }
            }
        })
        .ok();

    PersistenceGuard {
        stop_pair,
        join_handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SkipKind;

    #[test]
    fn roundtrip_save_and_load_store() {
        let dir = std::env::temp_dir().join(format!(
            "mouseskip_storage_test_{}",
            std::process::id()
        ));
        let path = dir.join("mouseskip_store.json");
        let _ = fs::remove_dir_all(&dir);

        let cfg = ThresholdConfig {
            dropout_threshold_ms: 12.5,
            spinout_threshold_px: 1500.0,
            active_pause_cutoff_ms: 200.0,
            monitoring_enabled: true,
            vibrancy_mode: "mica".to_string(),
        };
        let events = vec![SkipEvent {
            id: 42,
            timestamp_ms: 1_760_000_000_123,
            kind: SkipKind::PollingDropout,
            dt_ms: 19.8,
            dx: 7,
            dy: -3,
            delta_px: 7.61577,
            estimated_hz: 1000,
            threshold_used: 12.5,
        }];

        save_store(&path, &cfg, &events).expect("save_store failed");
        let loaded = load_store(&path);

        assert_eq!(loaded.config, cfg);
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].id, 42);
        assert_eq!(loaded.events[0].kind, SkipKind::PollingDropout);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn persistence_worker_and_flush_synchronize_cleanly() {
        let dir = std::env::temp_dir().join(format!(
            "mouseskip_sync_test_{}",
            std::process::id()
        ));
        let path = dir.join("mouseskip_store.json");
        let _ = fs::remove_dir_all(&dir);

        let state = SharedAppState::new(ThresholdConfig::default(), Vec::new());
        let guard = spawn_persistence_worker(Arc::clone(&state), path.clone());
        state.mark_dirty();
        flush_store_now(&state, &path);
        guard.shutdown();

        assert!(path.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_store_backs_up_corrupt_json() {
        let dir = std::env::temp_dir().join(format!(
            "mouseskip_corrupt_test_{}",
            std::process::id()
        ));
        let path = dir.join("mouseskip_store.json");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, b"{corrupt json").unwrap();

        let loaded = load_store(&path);
        assert_eq!(loaded.config, ThresholdConfig::default());
        assert!(loaded.events.is_empty());
        assert!(path.with_extension("json.corrupt").is_file());

        let _ = fs::remove_dir_all(&dir);
    }
}
