//! 本机 SQLite：终端实例、设置与账户状态（用户数据先存本地，不上传服务端）。
//! 迁移用 `PRAGMA user_version`，每个版本在一个事务里执行。

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE instances (
        id           TEXT PRIMARY KEY,
        name         TEXT NOT NULL,
        workdir      TEXT NOT NULL,
        launch       TEXT NOT NULL DEFAULT 'shell',
        command      TEXT NOT NULL DEFAULT '',
        auto_start   INTEGER NOT NULL DEFAULT 0,
        want_running INTEGER NOT NULL DEFAULT 0,
        port         INTEGER NOT NULL UNIQUE,
        created_at   TEXT NOT NULL,
        updated_at   TEXT NOT NULL
     );",
    // v2：每个终端可以选用不同的 shell（空 = 跟随默认）
    "ALTER TABLE instances ADD COLUMN shell TEXT NOT NULL DEFAULT '';",
];

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceRow {
    pub id: String,
    pub name: String,
    pub workdir: String,
    /// `shell` | `codex` | `claude` | `custom`
    pub launch: String,
    pub command: String,
    /// 使用的 shell：空为默认（Windows 依次 pwsh → Windows PowerShell → cmd；其他系统为登录 shell），
    /// 否则是内置选项的 ID（如 `pwsh`、`cmd`、`git-bash`）或用户填写的命令行（如 `bash`、`/usr/bin/fish -l`）
    #[serde(default)]
    pub shell: String,
    pub auto_start: bool,
    pub want_running: bool,
    pub port: u16,
    pub created_at: String,
    pub updated_at: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("无法打开数据库 {}", path.display()))?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: usize = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql).with_context(|| format!("数据库迁移 v{} 失败", i + 1))?;
            tx.pragma_update(None, "user_version", i + 1)?;
            tx.commit()?;
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&conn)?)
    }

    // ---------- kv ----------

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> =
            self.with(|c| c.query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0)).optional())?;
        Ok(match raw {
            Some(s) => Some(serde_json::from_str(&s).with_context(|| format!("设置 {key} 已损坏"))?),
            None => None,
        })
    }

    pub fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let s = serde_json::to_string(value)?;
        self.with(|c| c.execute("INSERT INTO kv(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, s]))?;
        Ok(())
    }

    pub fn remove(&self, key: &str) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM kv WHERE key=?1", [key]))?;
        Ok(())
    }

    // ---------- instances ----------

    const COLS: &'static str = "id,name,workdir,launch,command,auto_start,want_running,port,created_at,updated_at,shell";

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<InstanceRow> {
        Ok(InstanceRow {
            id: r.get(0)?,
            name: r.get(1)?,
            workdir: r.get(2)?,
            launch: r.get(3)?,
            command: r.get(4)?,
            auto_start: r.get::<_, i64>(5)? != 0,
            want_running: r.get::<_, i64>(6)? != 0,
            port: r.get(7)?,
            created_at: r.get(8)?,
            updated_at: r.get(9)?,
            shell: r.get(10)?,
        })
    }

    pub fn instances(&self) -> Result<Vec<InstanceRow>> {
        self.with(|c| {
            let mut st = c.prepare(&format!("SELECT {} FROM instances ORDER BY created_at, id", Self::COLS))?;
            let rows = st.query_map([], Self::row)?.collect::<rusqlite::Result<Vec<_>>>();
            rows
        })
    }

    pub fn instance(&self, id: &str) -> Result<Option<InstanceRow>> {
        self.with(|c| c.query_row(&format!("SELECT {} FROM instances WHERE id=?1", Self::COLS), [id], Self::row).optional())
    }

    pub fn insert_instance(&self, i: &InstanceRow) -> Result<()> {
        self.with(|c| {
            c.execute(
                &format!("INSERT INTO instances({}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", Self::COLS),
                params![i.id, i.name, i.workdir, i.launch, i.command, i.auto_start as i64, i.want_running as i64, i.port, i.created_at, i.updated_at, i.shell],
            )
        })?;
        Ok(())
    }

    pub fn update_instance(&self, i: &InstanceRow) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE instances SET name=?2, workdir=?3, launch=?4, command=?5, auto_start=?6, want_running=?7, port=?8, updated_at=?9, shell=?10 WHERE id=?1",
                params![i.id, i.name, i.workdir, i.launch, i.command, i.auto_start as i64, i.want_running as i64, i.port, i.updated_at, i.shell],
            )
        })?;
        Ok(())
    }

    pub fn set_want_running(&self, id: &str, want: bool) -> Result<()> {
        self.with(|c| c.execute("UPDATE instances SET want_running=?2 WHERE id=?1", params![id, want as i64]))?;
        Ok(())
    }

    pub fn delete_instance(&self, id: &str) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM instances WHERE id=?1", [id]))?;
        Ok(())
    }

    pub fn used_ports(&self) -> Result<Vec<u16>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT port FROM instances")?;
            let rows = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<u16>>>();
            rows
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_and_instances() {
        let s = Store::memory().unwrap();
        assert_eq!(s.get::<String>("a").unwrap(), None);
        s.set("a", &"x".to_string()).unwrap();
        s.set("a", &"y".to_string()).unwrap();
        assert_eq!(s.get::<String>("a").unwrap().as_deref(), Some("y"));
        s.remove("a").unwrap();
        assert_eq!(s.get::<String>("a").unwrap(), None);

        let mut i = InstanceRow {
            id: "abcd2345".into(),
            name: "t".into(),
            workdir: "/tmp".into(),
            launch: "shell".into(),
            command: String::new(),
            shell: String::new(),
            auto_start: false,
            want_running: false,
            port: 41001,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        };
        s.insert_instance(&i).unwrap();
        i.name = "u".into();
        i.shell = "bash -l".into();
        s.update_instance(&i).unwrap();
        s.set_want_running(&i.id, true).unwrap();
        let got = s.instance(&i.id).unwrap().unwrap();
        assert_eq!(got.name, "u");
        assert_eq!(got.shell, "bash -l");
        assert!(got.want_running);
        assert_eq!(s.used_ports().unwrap(), vec![41001]);
        s.delete_instance(&i.id).unwrap();
        assert!(s.instances().unwrap().is_empty());
    }
}
