//! Usage history ("frecency"): how often and how recently each result was
//! opened, plus which result was picked for which query. Stored locally in
//! `%APPDATA%\Heimdall\history.json`; never leaves the machine.

use crate::items::{Action, Icon, Item, Kind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Use {
    count: u32,
    last: u64,
    title: String,
    subtitle: String,
    kind: String,
    target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pick {
    key: String,
    count: u32,
}

#[derive(Default, Serialize, Deserialize)]
pub struct History {
    items: HashMap<String, Use>,
    learned: HashMap<String, Pick>,
    #[serde(skip)]
    dirty: bool,
}

fn path() -> std::path::PathBuf {
    crate::config::dir().join("history.json")
}

fn now() -> u64 {
    heimdall_core::query::now_unix()
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::App => "app",
        Kind::Folder => "folder",
        _ => "file",
    }
}

impl History {
    pub fn load() -> History {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        std::fs::create_dir_all(crate::config::dir()).ok();
        if let Ok(b) = serde_json::to_vec(self) {
            std::fs::write(path(), b).ok();
        }
        self.dirty = false;
    }

    pub fn record(&mut self, query: &str, item: &Item) {
        if item.key.is_empty() {
            return;
        }
        let target = match &item.action {
            Action::Open(t) => t.clone(),
            _ => String::new(),
        };
        let u = self.items.entry(item.key.clone()).or_insert(Use {
            count: 0,
            last: 0,
            title: item.title.clone(),
            subtitle: item.subtitle.clone(),
            kind: kind_name(item.kind).into(),
            target: target.clone(),
        });
        u.count += 1;
        u.last = now();
        u.title = item.title.clone();
        u.subtitle = item.subtitle.clone();
        u.target = target;
        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            let p = self.learned.entry(q).or_insert(Pick { key: item.key.clone(), count: 0 });
            if p.key == item.key {
                p.count += 1;
            } else {
                *p = Pick { key: item.key.clone(), count: 1 };
            }
        }
        if self.items.len() > 2000 {
            self.prune();
        }
        self.dirty = true;
    }

    fn prune(&mut self) {
        let mut v: Vec<(String, f32)> = self.items.iter().map(|(k, u)| (k.clone(), Self::weight(u))).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (k, _) in v.into_iter().skip(1500) {
            self.items.remove(&k);
        }
        let items = &self.items;
        self.learned.retain(|_, p| items.contains_key(&p.key));
    }

    fn weight(u: &Use) -> f32 {
        let days = now().saturating_sub(u.last) as f32 / 86_400.0;
        (u.count as f32 + 1.0).ln() / (1.0 + days / 7.0)
    }

    /// Ranking boost for `key` under the current query.
    pub fn boost(&self, query: &str, key: &str) -> i32 {
        if key.is_empty() {
            return 0;
        }
        let mut b = self.items.get(key).map(|u| (Self::weight(u) * 45.0).min(140.0) as i32).unwrap_or(0);
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return b;
        }
        if let Some(p) = self.learned.get(&q) {
            if p.key == key {
                return b + 320 + 40 * p.count.min(4) as i32;
            }
        }
        // Picked before for a longer query that starts with what's typed now.
        if self.learned.iter().any(|(lq, p)| p.key == key && lq.starts_with(&q)) {
            b += 160;
        }
        b
    }

    /// Most-used recent targets, for the empty-query view.
    pub fn top(&self, n: usize) -> Vec<Item> {
        let mut v: Vec<(&String, &Use)> = self.items.iter().filter(|(_, u)| !u.target.is_empty()).collect();
        v.sort_by(|a, b| Self::weight(b.1).total_cmp(&Self::weight(a.1)));
        v.into_iter()
            .take(n)
            .map(|(key, u)| {
                let kind = match u.kind.as_str() {
                    "app" => Kind::App,
                    "folder" => Kind::Folder,
                    _ => Kind::File,
                };
                let mut it = Item::new(&u.title, &u.subtitle, kind, Icon::Shell(u.target.clone()), Action::Open(u.target.clone()))
                    .with_key(key.clone());
                if kind != Kind::App {
                    it.path = Some(u.target.clone());
                }
                it
            })
            .collect()
    }

    pub fn forget(&mut self, key: &str) {
        self.items.remove(key);
        self.learned.retain(|_, p| p.key != key);
        self.dirty = true;
    }
}
