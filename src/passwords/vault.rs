//! Format v1 wraps a random data key with a master-password-derived key.
//! Account metadata and passwords have separate authenticated envelopes.
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305,
    aead::{Aead, Payload},
};
use rand::{RngCore, rngs::OsRng};
use rusqlite::{Connection, OpenFlags, params};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use zeroize::Zeroizing;

pub type Result<T> = std::result::Result<T, &'static str>;
pub type Secret = Zeroizing<String>;
type Key = Zeroizing<[u8; 32]>;
pub type Account = (i64, String, String);
const BAD: &str = "Incorrect master password or damaged vault.";
const IO: &str = "Vault storage unavailable or damaged.";

// libsodium ABI. Prefer the development library when installed.
#[cfg_attr(
    sodium_soname,
    link(name = "libsodium.so.26", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(sodium_soname), link(name = "sodium"))]
unsafe extern "C" {
    fn sodium_init() -> i32;
    fn crypto_pwhash_alg_argon2id13() -> i32;
    fn crypto_pwhash(
        out: *mut u8,
        len: u64,
        pass: *const u8,
        passlen: u64,
        salt: *const u8,
        ops: u64,
        memory: usize,
        alg: i32,
    ) -> i32;
}

fn derive(pass: &str, salt: &[u8]) -> Result<Key> {
    if salt.len() != 16 {
        return Err(BAD);
    }
    let mut key = Key::default();
    // All buffers remain valid for this synchronous call. Salt length is checked above.
    let ok = unsafe {
        sodium_init() >= 0
            && crypto_pwhash(
                key.as_mut_ptr(),
                32,
                pass.as_ptr(),
                pass.len() as u64,
                salt.as_ptr(),
                3,
                64 * 1024 * 1024,
                crypto_pwhash_alg_argon2id13(),
            ) == 0
    };
    ok.then_some(key).ok_or("Key derivation failed.")
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| "System randomness unavailable.")?;
    Ok(bytes)
}
fn seal(key: &Key, aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>> {
    let nonce = random::<24>()?;
    let mut result = nonce.to_vec();
    result.extend(
        XChaCha20Poly1305::new((&**key).into())
            .encrypt((&nonce).into(), Payload { msg: bytes, aad })
            .map_err(|_| IO)?,
    );
    Ok(result)
}
fn open(key: &Key, aad: &[u8], bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if bytes.len() < 40 {
        return Err(BAD);
    }
    XChaCha20Poly1305::new((&**key).into())
        .decrypt(
            bytes[..24].into(),
            Payload {
                msg: &bytes[24..],
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| BAD)
}
const HEADER: &[u8] = b"Mallow vault v1 Argon2id13 m65536 t3";
pub struct Vault {
    db: Connection,
    key: Key,
}
impl Vault {
    pub fn create(path: &Path, pass: &str) -> Result<Self> {
        if pass.chars().count() < 12 {
            return Err("Use at least 12 characters for the master password.");
        }
        let salt = random::<16>()?;
        let key = Zeroizing::new(random()?);
        let wrapped = seal(&derive(pass, &salt)?, HEADER, key.as_ref())?;
        std::fs::create_dir_all(path.parent().ok_or(IO)?).map_err(|_| IO)?;
        // Build the schema in a private sibling, then publish it whole. An interrupted
        // create leaves only a stray temporary file, never a partial vault at `path`.
        let tmp = path.with_extension(format!("tmp{:x}", OsRng.next_u64()));
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        // On Windows the per-user profile folder's ACL already keeps it private.
        #[cfg(unix)]
        file.mode(0o600);
        file.open(&tmp).map_err(|_| IO)?;
        let built = (|| {
            let mut db =
                Connection::open_with_flags(&tmp, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|_| IO)?;
            let tx = db.transaction().map_err(|_| IO)?;
            tx.execute_batch("CREATE TABLE header(version INTEGER PRIMARY KEY,salt BLOB NOT NULL,wrapped BLOB NOT NULL); CREATE TABLE accounts(id INTEGER PRIMARY KEY AUTOINCREMENT, meta BLOB NOT NULL, secret BLOB NOT NULL);").map_err(|_|IO)?;
            tx.execute("INSERT INTO header VALUES(1,?,?)", params![salt, wrapped])
                .map_err(|_| IO)?;
            tx.commit().map_err(|_| IO)?;
            db.close().map_err(|_| IO)?;
            // Write access: Windows refuses to flush a read-only handle.
            std::fs::OpenOptions::new()
                .write(true)
                .open(&tmp)
                .and_then(|f| f.sync_all())
                .map_err(|_| IO)?;
            // hard_link fails if `path` exists, so an existing vault is never replaced.
            std::fs::hard_link(&tmp, path).map_err(|_| "Vault already exists or could not be created.")
        })();
        let _ = std::fs::remove_file(&tmp);
        built?;
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|_| IO)?;
        Ok(Self { db, key })
    }
    pub fn unlock(path: &Path, pass: &str) -> Result<Self> {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|_| IO)?;
        let (salt, wrapped): (Vec<u8>, Vec<u8>) = db
            .query_row("SELECT salt,wrapped FROM header WHERE version=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|_| IO)?;
        let raw = open(&derive(pass, &salt)?, HEADER, &wrapped)?;
        Ok(Self {
            db,
            key: Zeroizing::new(raw.as_slice().try_into().map_err(|_| BAD)?),
        })
    }
    pub fn change(&self, pass: &str) -> Result<()> {
        if pass.chars().count() < 12 {
            return Err("Use at least 12 characters.");
        }
        let salt = random::<16>()?;
        let wrapped = seal(&derive(pass, &salt)?, HEADER, self.key.as_ref())?;
        self.db
            .execute(
                "UPDATE header SET salt=?,wrapped=? WHERE version=1",
                params![salt, wrapped],
            )
            .map_err(|_| IO)?;
        Ok(())
    }
    pub fn list(&self) -> Result<Vec<Account>> {
        let mut q = self
            .db
            .prepare("SELECT id,meta FROM accounts ORDER BY id")
            .map_err(|_| IO)?;
        let mut rows = q.query([]).map_err(|_| IO)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|_| IO)? {
            let id: i64 = row.get(0).map_err(|_| IO)?;
            let bytes: Vec<u8> = row.get(1).map_err(|_| IO)?;
            let (origin, user) =
                serde_json::from_slice(&open(&self.key, format!("meta:{id}").as_bytes(), &bytes)?)
                    .map_err(|_| BAD)?;
            out.push((id, origin, user));
        }
        Ok(out)
    }
    pub fn put(&mut self, origin: &str, user: &str, password: &str) -> Result<()> {
        if super::origin(origin).as_deref() != Some(origin) || password.is_empty() {
            return Err("Enter an HTTPS origin and a password.");
        }
        let existing = self
            .list()?
            .into_iter()
            .find(|(_, o, u)| o == origin && u == user);
        self.write(existing.map(|a| a.0), origin, user, password)
    }
    pub fn edit(&mut self, account: &Account, user: &str, password: &str) -> Result<()> {
        if self
            .list()?
            .iter()
            .any(|(id, o, u)| *id != account.0 && o == &account.1 && u == user)
        {
            return Err("Another saved account already uses this username on this site.");
        }
        let previous = self.password(account)?;
        self.write(
            Some(account.0),
            &account.1,
            user,
            if password.is_empty() { &previous } else { password },
        )
    }
    fn write(&mut self, id: Option<i64>, origin: &str, user: &str, password: &str) -> Result<()> {
        let tx = self.db.transaction().map_err(|_| IO)?;
        let id = if let Some(id) = id {
            id
        } else {
            tx.execute("INSERT INTO accounts(meta,secret) VALUES(x'',x'')", [])
                .map_err(|_| IO)?;
            tx.last_insert_rowid()
        };
        let metadata = Zeroizing::new(serde_json::to_vec(&(origin, user)).map_err(|_| IO)?);
        let meta = seal(&self.key, format!("meta:{id}").as_bytes(), &metadata)?;
        let aad = serde_json::to_vec(&(id, origin, user)).map_err(|_| IO)?;
        let secret = seal(&self.key, &aad, password.as_bytes())?;
        tx.execute(
            "UPDATE accounts SET meta=?,secret=? WHERE id=?",
            params![meta, secret, id],
        )
        .map_err(|_| IO)?;
        tx.commit().map_err(|_| IO)
    }
    pub fn password(&self, account: &Account) -> Result<Secret> {
        let blob: Vec<u8> = self
            .db
            .query_row("SELECT secret FROM accounts WHERE id=?", [account.0], |r| {
                r.get(0)
            })
            .map_err(|_| IO)?;
        let aad = serde_json::to_vec(account).map_err(|_| IO)?;
        let mut raw = open(&self.key, &aad, &blob)?;
        String::from_utf8(std::mem::take(&mut *raw))
            .map(Zeroizing::new)
            .map_err(|_| BAD)
    }
    pub fn delete(&self, id: Option<i64>) -> Result<()> {
        self.db
            .execute("DELETE FROM accounts WHERE ?1 IS NULL OR id=?1", [id])
            .map_err(|_| IO)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_change_lock_and_tampering() {
        let path = std::env::temp_dir().join(format!("mallow-vault-test-{:x}.db", OsRng.next_u64()));
        let old = "synthetic master one";
        let new = "synthetic master two";
        let mut v = Vault::create(&path, old).unwrap();
        assert!(Vault::create(&path, new).is_err());
        for name in ["alice", "bob"] {
            v.put("https://example.test", name, "synthetic secret").unwrap();
        }
        v.put("https://example.test", "alice", "synthetic update")
            .unwrap();
        let accounts = v.list().unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(v.password(&accounts[0]).unwrap().as_str() == "synthetic update");
        v.change(new).unwrap();
        drop(v);
        assert!(Vault::unlock(&path, old).is_err());
        let v = Vault::unlock(&path, new).unwrap();
        assert!(v.password(&accounts[1]).unwrap().as_str() == "synthetic secret");
        v.db.execute(
            "UPDATE accounts SET secret=(SELECT secret FROM accounts WHERE id=1) WHERE id=2",
            [],
        )
        .unwrap();
        assert!(v.password(&accounts[1]).is_err());
        v.delete(Some(accounts[1].0)).unwrap();
        assert_eq!(v.list().unwrap().len(), 1);
        v.delete(None).unwrap();
        drop(v);
        assert!(Vault::unlock(&path, new).unwrap().list().unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
