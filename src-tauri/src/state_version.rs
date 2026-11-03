use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static LOG_VERSION: AtomicU64 = AtomicU64::new(0);
static POOL_VERSION: AtomicU64 = AtomicU64::new(0);
static CHANNEL_VERSION: AtomicU64 = AtomicU64::new(0);
static TOKEN_VERSION: AtomicU64 = AtomicU64::new(0);

fn version_ref(module: &str) -> &AtomicU64 {
    match module {
        "log" => &LOG_VERSION,
        "pool" => &POOL_VERSION,
        "channel" => &CHANNEL_VERSION,
        "token" => &TOKEN_VERSION,
        _ => panic!("Unknown module: {module}"),
    }
}

/// Increment version number of the specified module. Called after each data write.
pub fn bump(module: &str) {
    version_ref(module).fetch_add(1, Ordering::Release);
}

/// Return current version number of the specified module
pub fn current(module: &str) -> u64 {
    version_ref(module).load(Ordering::Acquire)
}

/// Return version numbers of all modules (for HTTP handler use)
pub fn all() -> HashMap<&'static str, u64> {
    let mut m = HashMap::new();
    m.insert("log", LOG_VERSION.load(Ordering::Acquire));
    m.insert("pool", POOL_VERSION.load(Ordering::Acquire));
    m.insert("channel", CHANNEL_VERSION.load(Ordering::Acquire));
    m.insert("token", TOKEN_VERSION.load(Ordering::Acquire));
    m
}

/// Response struct for version numbers of all modules
#[derive(Debug, Clone, Serialize)]
pub struct StatVersionsResponse {
    pub log: u64,
    pub pool: u64,
    pub channel: u64,
    pub token: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrayVersions {
    pub pool: u64,
    pub channel: u64,
    pub token: u64,
}

impl TrayVersions {
    pub fn has_changed_since(self, previous: Self) -> bool {
        self.pool != previous.pool
            || self.channel != previous.channel
            || self.token != previous.token
    }
}

pub fn tray_versions() -> TrayVersions {
    TrayVersions {
        pool: current("pool"),
        channel: current("channel"),
        token: current("token"),
    }
}

#[cfg(test)]
mod tests {
    use super::TrayVersions;

    #[test]
    fn tray_versions_ignore_log_only_changes() {
        let previous = TrayVersions {
            pool: 1,
            channel: 2,
            token: 3,
        };
        let current = TrayVersions {
            pool: 1,
            channel: 2,
            token: 3,
        };

        assert!(!current.has_changed_since(previous));
    }

    #[test]
    fn tray_versions_detect_pool_channel_or_token_changes() {
        let previous = TrayVersions {
            pool: 1,
            channel: 2,
            token: 3,
        };

        assert!(TrayVersions {
            pool: 2,
            ..previous
        }
        .has_changed_since(previous));
        assert!(TrayVersions {
            channel: 3,
            ..previous
        }
        .has_changed_since(previous));
        assert!(TrayVersions {
            token: 4,
            ..previous
        }
        .has_changed_since(previous));
    }
}
