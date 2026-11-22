//! History and bookmarks in SQLite. Writes go to a background thread so the UI
//! can display history asynchronously.

use rusqlite::{Connection, params};
use std::{cell::RefCell, path::Path, sync::mpsc, time::SystemTime};

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS history (url TEXT NOT NULL, title TEXT NOT NULL, visited INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS history_visited ON history (visited);
CREATE TABLE IF NOT EXISTS bookmarks (url TEXT PRIMARY KEY, title TEXT NOT NULL, added INTEGER NOT NULL);
";

/// Latest visit to one URL.
#[derive(Debug, Clone)]
pub struct Visit {
    pub url: String,
    pub title: String,
    /// Unix seconds.
    pub when: i64,
}

enum Cmd {
    Visit(String, String),
    /// Title known after the visit was recorded.
    Title(String, String),
    Bookmark(String, String),
    Unbookmark(String),
    ClearHistory,
    /// Remove every visit to one URL.
    Forget(String),
    History(u32, futures_channel::oneshot::Sender<Vec<Visit>>),
    /// History matching typed text, for address-bar suggestions.
    Suggest(String, u32, futures_channel::oneshot::Sender<Vec<Visit>>),
    Stop,
}

pub struct Store {
    writer: RefCell<Option<std::thread::JoinHandle<()>>>,
    tx: mpsc::Sender<Cmd>,
    /// (url, title), newest first. Kept in memory so the star updates instantly.
    pub bookmarks: RefCell<Vec<(String, String)>>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch(SCHEMA)?;
    Ok(db)
}

fn apply(db: &Connection, cmd: Cmd) -> rusqlite::Result<usize> {
    match cmd {
        Cmd::Stop => unreachable!(),
        Cmd::History(limit, reply) => {
            let _ = reply.send(history(db, limit));
            Ok(0)
        }
        Cmd::Suggest(query, limit, reply) => {
            let _ = reply.send(suggest(db, &query, limit));
            Ok(0)
        }
        Cmd::Visit(url, title) => db.execute(
            "INSERT INTO history VALUES (?1, ?2, ?3)",
            params![url, title, now()],
        ),
        Cmd::Title(url, title) => db.execute(
            "UPDATE history SET title = ?2 WHERE url = ?1 AND title = ''",
            params![url, title],
        ),
        Cmd::Bookmark(url, title) => db.execute(
            "INSERT INTO bookmarks VALUES (?1, ?2, ?3) ON CONFLICT(url) DO UPDATE SET title = excluded.title",
            params![url, title, now()],
        ),
        Cmd::Unbookmark(url) => db.execute("DELETE FROM bookmarks WHERE url = ?1", [url]),
        Cmd::ClearHistory => db.execute("DELETE FROM history", []),
        Cmd::Forget(url) => db.execute("DELETE FROM history WHERE url = ?1", [url]),
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let db = open_db(path).map_err(|e| e.to_string())?;
        let writer = open_db(path).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        let writer = std::thread::Builder::new()
            .name("mallow-db".into())
            .spawn(move || {
                for cmd in rx {
                    if matches!(cmd, Cmd::Stop) {
                        break;
                    }
                    if let Err(e) = apply(&writer, cmd) {
                        eprintln!("mallow: database write failed: {e}");
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let bookmarks = db
            .prepare("SELECT url, title FROM bookmarks ORDER BY added DESC")
            .and_then(|mut s| s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            writer: RefCell::new(Some(writer)),
            tx,
            bookmarks: RefCell::new(bookmarks),
        })
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn visit(&self, url: &str, title: &str) {
        self.send(Cmd::Visit(url.into(), title.into()));
    }

    /// Fill in the title of untitled visits to `url`.
    pub fn title(&self, url: &str, title: &str) {
        self.send(Cmd::Title(url.into(), title.into()));
    }

    pub fn clear_history(&self) {
        self.send(Cmd::ClearHistory);
    }

    pub fn forget(&self, url: &str) {
        self.send(Cmd::Forget(url.into()));
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.bookmarks.borrow().iter().any(|(u, _)| u == url)
    }

    /// Add a bookmark, or rename it in place if it exists.
    pub fn bookmark(&self, url: &str, title: &str) {
        let mut list = self.bookmarks.borrow_mut();
        match list.iter_mut().find(|(u, _)| u == url) {
            Some(entry) => entry.1 = title.into(),
            None => list.insert(0, (url.into(), title.into())),
        }
        self.send(Cmd::Bookmark(url.into(), title.into()));
    }

    pub fn unbookmark(&self, url: &str) {
        self.bookmarks.borrow_mut().retain(|(u, _)| u != url);
        self.send(Cmd::Unbookmark(url.into()));
    }

    pub fn shutdown(&self) {
        if let Some(writer) = self.writer.borrow_mut().take() {
            self.send(Cmd::Stop);
            writer.join().expect("database worker panicked");
        }
    }

    pub fn history(&self, limit: u32) -> futures_channel::oneshot::Receiver<Vec<Visit>> {
        let (reply, result) = futures_channel::oneshot::channel();
        self.send(Cmd::History(limit, reply));
        result
    }
}

impl Store {
    /// Visited pages whose URL or title contains `query`. Sites whose host
    /// starts with it rank first, then the most visited.
    pub fn suggest(&self, query: &str, limit: u32) -> futures_channel::oneshot::Receiver<Vec<Visit>> {
        let (reply, result) = futures_channel::oneshot::channel();
        self.send(Cmd::Suggest(query.to_lowercase(), limit, reply));
        result
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn history(db: &Connection, limit: u32) -> Vec<Visit> {
    db.prepare("SELECT url, title, MAX(visited) AS v FROM history GROUP BY url ORDER BY v DESC LIMIT ?1")
        .and_then(|mut s| {
            s.query_map([limit], |r| {
                Ok(Visit {
                    url: r.get(0)?,
                    title: r.get(1)?,
                    when: r.get(2)?,
                })
            })?
            .collect()
        })
        .unwrap_or_else(|e| {
            eprintln!("mallow: history query failed: {e}");
            Vec::new()
        })
}

fn suggest(db: &Connection, query: &str, limit: u32) -> Vec<Visit> {
    let like = format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    db.prepare(
        "SELECT url, title, MAX(visited) AS v FROM history
         WHERE url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\'
         GROUP BY url
         ORDER BY (instr(url, '://' || ?2) > 0 OR instr(url, '://www.' || ?2) > 0) DESC, COUNT(*) DESC, v DESC
         LIMIT ?3",
    )
    .and_then(|mut s| {
        s.query_map(params![like, query, limit], |r| {
            Ok(Visit {
                url: r.get(0)?,
                title: r.get(1)?,
                when: r.get(2)?,
            })
        })?
        .collect()
    })
    .unwrap_or_else(|e| {
        eprintln!("mallow: suggestion query failed: {e}");
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let path = std::env::temp_dir().join(format!("mallow-db-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let s = Store::open(&path).unwrap();
            s.bookmark("https://a", "A");
            s.bookmark("https://b", "B");
            s.bookmark("https://a", "Renamed");
            assert!(s.is_bookmarked("https://a"));
            s.visit("https://a", "A");
            s.visit("https://b", "B");
            s.visit("https://a", "A2");
            s.visit("https://c", "");
            s.title("https://c", "Late title");
        }
        let s = Store::open(&path).unwrap();
        assert!(s.is_bookmarked("https://a"));
        // rename updated the title without duplicating the row
        let marks = s.bookmarks.borrow().clone();
        assert_eq!(marks.len(), 2);
        assert!(marks.contains(&("https://a".to_string(), "Renamed".to_string())));
        let h = gtk::glib::MainContext::new().block_on(s.history(10)).unwrap();
        assert_eq!(h.len(), 3);
        assert!(h.iter().any(|v| v.url == "https://a" && v.when > 0));
        assert!(h.iter().any(|v| v.url == "https://c" && v.title == "Late title"));
        s.forget("https://c");
        s.visit("https://mail.example/inbox", "Inbox");
        s.visit("https://other.example/?q=gmail", "Search 100% off_");
        s.visit("https://www.gmail.example/", "Mail");
        let found = |q| gtk::glib::MainContext::new().block_on(s.suggest(q, 10)).unwrap();
        let urls: Vec<_> = found("GMA").into_iter().map(|v| v.url).collect();
        // host prefix ranks above a mere substring match; case-insensitive
        assert_eq!(
            urls,
            ["https://www.gmail.example/", "https://other.example/?q=gmail"]
        );
        // LIKE wildcards in the query are literal
        assert_eq!(found("100%").len(), 1);
        assert_eq!(found("f_").len(), 1);
        assert!(found("o%f").is_empty());
        s.forget("https://mail.example/inbox");
        s.forget("https://other.example/?q=gmail");
        s.forget("https://www.gmail.example/");
        s.forget("https://b");
        let h = gtk::glib::MainContext::new().block_on(s.history(10)).unwrap();
        assert_eq!(h.len(), 1);
        s.unbookmark("https://a");
        s.unbookmark("https://b");
        assert!(!s.is_bookmarked("https://a"));
        s.clear_history();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert!(
            gtk::glib::MainContext::new()
                .block_on(s.history(10))
                .unwrap()
                .is_empty()
        );
        assert!(!s.is_bookmarked("https://a"));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
