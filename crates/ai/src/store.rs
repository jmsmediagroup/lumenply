//! Where downloaded models live, and the download itself.
//!
//! Each model has a folder named by its key (`<dir>/mobile-sam/`). A file
//! is fetched over HTTPS into `<name>.part` next to its final place,
//! hashed while it streams, and renamed only once its length and SHA-256
//! match the registry; anything else (a network error, a wrong hash, the
//! cancel flag) deletes the partial file. A model counts as installed when
//! all its files are present with their pinned sizes; [`ModelStore::verify`]
//! re-hashes them.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::registry::{ModelFile, ModelId};
use crate::{AiError, Result};

/// Read size of the download loop; the cancel flag is checked this often.
const CHUNK: usize = 1 << 16;

/// The folder of downloaded models.
#[derive(Clone, Debug)]
pub struct ModelStore {
    dir: PathBuf,
}

impl ModelStore {
    /// A store in `dir` (created on the first download).
    pub fn new(dir: impl Into<PathBuf>) -> ModelStore {
        ModelStore { dir: dir.into() }
    }

    /// The app's default store, `~/.lumenply/models` (`%USERPROFILE%` on
    /// Windows), or `None` without a home folder.
    pub fn default_dir() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(".lumenply").join("models"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The folder holding `id`'s files.
    pub fn model_dir(&self, id: ModelId) -> PathBuf {
        self.dir.join(id.key())
    }

    /// Where `file` of model `id` is installed.
    pub fn file_path(&self, id: ModelId, file: &ModelFile) -> PathBuf {
        self.model_dir(id).join(file.name)
    }

    /// Where CoreML keeps the compiled form of `id`'s files, so later
    /// loads skip the compile.
    pub(crate) fn compile_cache(&self, id: ModelId) -> PathBuf {
        self.model_dir(id).join("coreml-cache")
    }

    /// All of `id`'s files are present with their pinned sizes.
    pub fn installed(&self, id: ModelId) -> bool {
        id.info().files.iter().all(|f| {
            fs::metadata(self.file_path(id, f))
                .map(|m| m.is_file() && m.len() == f.bytes)
                .unwrap_or(false)
        })
    }

    /// Bytes `id` takes on disk (its files plus any compiled cache), for
    /// Preferences.
    pub fn disk_bytes(&self, id: ModelId) -> u64 {
        fn walk(p: &Path) -> u64 {
            match fs::symlink_metadata(p) {
                Ok(m) if m.is_dir() => fs::read_dir(p)
                    .map(|rd| rd.flatten().map(|e| walk(&e.path())).sum())
                    .unwrap_or(0),
                Ok(m) => m.len(),
                Err(_) => 0,
            }
        }
        walk(&self.model_dir(id))
    }

    /// Re-hash `id`'s installed files against the registry.
    pub fn verify(&self, id: ModelId) -> Result<()> {
        for f in id.info().files {
            let path = self.file_path(id, f);
            if !path.is_file() {
                return Err(AiError::NotInstalled(id.to_string()));
            }
            let actual = sha256_file(&path)?;
            if actual != f.sha256 {
                return Err(AiError::Checksum {
                    file: f.name.into(),
                    expected: f.sha256.into(),
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Download whatever of `id` is missing. `progress(done, total)` is
    /// called as bytes arrive (over all of the model's files, counting the
    /// ones already present as done); setting `cancel` stops the download
    /// within one read and removes the partial file.
    pub fn download(&self, id: ModelId, progress: &dyn Fn(u64, u64), cancel: &AtomicBool) -> Result<()> {
        let info = id.info();
        fs::create_dir_all(self.model_dir(id))?;
        self.prune(id);
        let total = info.total_bytes;
        let mut done = 0u64;
        progress(0, total);
        for f in info.files {
            let dest = self.file_path(id, f);
            if fs::metadata(&dest).is_ok_and(|m| m.len() == f.bytes) {
                done += f.bytes;
                progress(done, total);
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            let mut body = http_get(f.url)?;
            let base = done;
            save_verified(&mut body, &dest, f, &mut |n| progress(base + n, total), cancel)?;
            done += f.bytes;
        }
        Ok(())
    }

    /// Delete files in `id`'s folder that the registry no longer lists
    /// (an earlier version's weights, partial downloads); the compiled
    /// cache stays.
    fn prune(&self, id: ModelId) {
        let keep: Vec<&str> = id.info().files.iter().map(|f| f.name).collect();
        let Ok(entries) = fs::read_dir(self.model_dir(id)) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let listed = name.to_str().is_some_and(|n| keep.contains(&n));
            if !listed && e.file_type().is_ok_and(|t| t.is_file()) {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    /// Delete `id`'s folder (files, partial downloads and compiled cache).
    pub fn remove(&self, id: ModelId) -> Result<()> {
        match fs::remove_dir_all(self.model_dir(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// The partial file a download of `dest` writes to.
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Stream `src` into `dest` through a `.part` file, hashing on the way;
/// rename it into place only if it holds exactly `file.bytes` bytes with
/// `file.sha256`. `progress` gets the bytes written so far. On any error,
/// including `cancel`, the partial file is removed.
pub(crate) fn save_verified(
    src: &mut dyn Read,
    dest: &Path,
    file: &ModelFile,
    progress: &mut dyn FnMut(u64),
    cancel: &AtomicBool,
) -> Result<()> {
    let part = part_path(dest);
    let result = (|| {
        let mut out = fs::File::create(&part)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut written = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(AiError::Download(format!("{}: {e}", file.name))),
            };
            written += n as u64;
            if written > file.bytes {
                return Err(AiError::Download(format!(
                    "{}: more than the expected {} bytes",
                    file.name, file.bytes
                )));
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            progress(written);
        }
        if written != file.bytes {
            return Err(AiError::Download(format!(
                "{}: got {written} of {} bytes",
                file.name, file.bytes
            )));
        }
        let actual = hex(&hasher.finalize());
        if actual != file.sha256 {
            return Err(AiError::Checksum {
                file: file.name.into(),
                expected: file.sha256.into(),
                actual,
            });
        }
        out.sync_all()?;
        drop(out);
        fs::rename(&part, dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// A body reader for `url` (redirects followed, HTTPS only).
fn http_get(url: &str) -> Result<Box<dyn Read + Send>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .user_agent(format!("lumenply/{}", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .build()
        .into();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| AiError::Download(format!("{url}: {e}")))?;
    Ok(Box::new(resp.into_body().into_reader()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file, lower-case hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder unique to the test.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lumenply-ai-store-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// "hello world\n" and its SHA-256 (as `shasum -a 256` prints it).
    const HELLO: &[u8] = b"hello world\n";
    const HELLO_SHA: &str = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";

    fn hello_file() -> ModelFile {
        ModelFile {
            name: "hello.onnx",
            url: "https://example.invalid/hello.onnx",
            bytes: HELLO.len() as u64,
            sha256: HELLO_SHA,
            avoid: &[],
        }
    }

    #[test]
    fn a_matching_file_is_renamed_into_place() {
        let dir = scratch("ok");
        let dest = dir.join("hello.onnx");
        let mut seen = Vec::new();
        save_verified(
            &mut &HELLO[..],
            &dest,
            &hello_file(),
            &mut |n| seen.push(n),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), HELLO);
        assert!(!part_path(&dest).exists());
        assert_eq!(seen.last(), Some(&12));
        assert_eq!(sha256_file(&dest).unwrap(), HELLO_SHA);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_corrupted_file_fails_the_hash_and_leaves_nothing() {
        let dir = scratch("corrupt");
        let dest = dir.join("hello.onnx");
        // Same length, one byte flipped.
        let bad = b"hello World\n";
        let err = save_verified(
            &mut &bad[..],
            &dest,
            &hello_file(),
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
        match err {
            AiError::Checksum {
                file,
                expected,
                actual,
            } => {
                assert_eq!(file, "hello.onnx");
                assert_eq!(expected, HELLO_SHA);
                // `printf 'hello World\n' | shasum -a 256`
                assert_eq!(
                    actual,
                    "0c23d0ceae909c42439cbb3069887888cb829f6c9d2c93966c944c65a6b6ed59"
                );
            }
            e => panic!("expected a checksum error, got {e}"),
        }
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        // A short file is refused before hashing.
        let err = save_verified(
            &mut &HELLO[..5],
            &dest,
            &hello_file(),
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(err.to_string().contains("got 5 of 12 bytes"), "{err}");
        assert!(!part_path(&dest).exists());
        let _ = fs::remove_dir_all(dir);
    }

    /// Hands out a few bytes, then sets the cancel flag as if the user
    /// pressed Cancel mid-download.
    struct CancelAfter<'a> {
        data: &'a [u8],
        flag: &'a AtomicBool,
    }

    impl Read for CancelAfter<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = 4.min(self.data.len()).min(buf.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            self.flag.store(true, Ordering::Relaxed);
            Ok(n)
        }
    }

    #[test]
    fn cancel_removes_the_partial_file() {
        let dir = scratch("cancel");
        let dest = dir.join("hello.onnx");
        let flag = AtomicBool::new(false);
        let mut src = CancelAfter {
            data: HELLO,
            flag: &flag,
        };
        let mut seen = Vec::new();
        let err = save_verified(&mut src, &dest, &hello_file(), &mut |n| seen.push(n), &flag).unwrap_err();
        assert!(matches!(err, AiError::Cancelled), "{err}");
        // One chunk of 4 bytes landed before the flag was seen.
        assert_eq!(seen, vec![4]);
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn installed_checks_sizes_and_remove_deletes_the_folder() {
        let dir = scratch("installed");
        let store = ModelStore::new(&dir);
        let id = ModelId::MobileSam;
        assert!(!store.installed(id));
        fs::create_dir_all(store.model_dir(id)).unwrap();
        let files = id.info().files;
        // Right names, wrong sizes: not installed.
        for f in files {
            fs::write(store.file_path(id, f), b"x").unwrap();
        }
        assert!(!store.installed(id));
        assert_eq!(store.disk_bytes(id), 2);
        // Files the registry no longer lists go before a download; the
        // listed ones and the compiled cache stay.
        fs::write(store.model_dir(id).join("old_weights.onnx"), b"stale").unwrap();
        fs::write(store.model_dir(id).join("x.onnx.part"), b"half").unwrap();
        fs::create_dir_all(store.compile_cache(id)).unwrap();
        assert_eq!(store.disk_bytes(id), 11);
        store.prune(id);
        assert_eq!(store.disk_bytes(id), 2);
        assert!(store.compile_cache(id).is_dir());
        assert!(matches!(store.verify(id), Err(AiError::Checksum { .. })));
        store.remove(id).unwrap();
        assert!(!store.model_dir(id).exists());
        store.remove(id).unwrap(); // removing twice is fine
        let _ = fs::remove_dir_all(dir);
    }
}
