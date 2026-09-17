//! A directory a peer can address, and nothing outside it.
//!
//! # Why the check and the open have to be one thing
//!
//! The obvious version of this joins the peer's path to the root,
//! canonicalises it, and checks the answer still starts with the root. **That
//! is a race, and the race is winnable.** Between the check and the open a
//! component can become a symbolic link: something places `root/data` as a
//! link to `/etc`, and `root/data/cron.d/x` is now a path that passed every
//! string test there was.
//!
//! So nothing here ever builds a path and opens it. Every operation is
//! relative to a **handle** — a directory this process already holds open,
//! which cannot be swapped for something else after the fact — and links are
//! refused at every step rather than resolved.
//!
//! Two ways of doing that, and both are race-free:
//!
//! - [`HowConfined::Kernel`] — `openat2` with `RESOLVE_BENEATH` and
//!   `RESOLVE_NO_SYMLINKS`. The kernel walks the path itself and refuses to
//!   leave the directory. One syscall, and the guarantee is the operating
//!   system's rather than this file's.
//! - [`HowConfined::Handles`] — one `openat` per component with `O_NOFOLLOW`
//!   and `O_DIRECTORY`, each relative to the handle before it. More syscalls,
//!   the same property, and it works on every Unix rather than Linux 5.6 and
//!   later.
//!
//! Both are exercised: the escape tests run against each in turn, because a
//! fallback nothing tests is a fallback that does not work.

use std::path::{Path, PathBuf};

use crate::{FileError, SafePath};

/// How this machine is keeping a peer inside the directory.
///
/// Reported so a session can say it out loud. An operator on a host where the
/// kernel is doing it and one where this code is doing it are in different
/// situations, and only one of them can find that out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HowConfined {
    /// `openat2` with `RESOLVE_BENEATH`: the kernel refuses to leave.
    Kernel,
    /// One open per component, links refused at each step.
    Handles,
    /// Components checked, then opened. **The only one with a window in it.**
    Checked,
}

impl HowConfined {
    /// A phrase for a session's own log.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Kernel => "the kernel refuses to leave the directory",
            Self::Handles => "each directory is held open, and links are refused",
            Self::Checked => "paths are checked before they are opened",
        }
    }

    /// Whether this one is free of a check-then-open window.
    #[must_use]
    pub const fn is_race_free(self) -> bool {
        matches!(self, Self::Kernel | Self::Handles)
    }
}

/// One thing a listing found.
///
/// Deliberately not [`esper_proto::FileEntry`]: this crate answers about a
/// filesystem and the protocol decides what to say about it. A name that is
/// not valid UTF-8 exists on a real disk and has no representation on the
/// wire, and the conversion is where that is dealt with rather than here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: std::ffi::OsString,
    pub bytes: u64,
    pub directory: bool,
}

/// The directory transfers happen inside.
#[derive(Debug)]
pub struct Vault {
    /// The directory itself, held open.
    ///
    /// **Read only on Unix**, where confinement is this handle: every open
    /// goes through it with `openat`, so the directory cannot be swapped for a
    /// link between the check and the open. The fallback resolves paths as
    /// text and has nothing to do with it — but the handle is still held
    /// there, because dropping it would let the directory be replaced under a
    /// transfer in progress, and because a `Vault` that means two different
    /// things depending on the platform is worse than a field one platform
    /// does not read.
    #[cfg_attr(not(unix), allow(dead_code))]
    root: std::fs::File,
    /// Where it is, for a message to a person. **Never sent to a peer**: where
    /// a host keeps its files is the host's business.
    shown: PathBuf,
    how: HowConfined,
}

impl Vault {
    /// Open a root, creating it if it is not there.
    ///
    /// # Errors
    ///
    /// If the directory cannot be made or opened. A root that is a file, or a
    /// link to somewhere else, is refused: the whole arrangement rests on this
    /// handle being the place the machine's owner named.
    pub fn open(root: &Path) -> Result<Self, FileError> {
        // **Only what this makes, is made private.** A directory that was
        // already there belongs to whoever made it — a person's own working
        // directory is very often the destination of a copy, and narrowing it
        // underneath them would be this quietly changing something it was not
        // asked to touch.
        let ours = !root.exists();
        if let Err(e) = std::fs::create_dir_all(root)
            && e.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(FileError::NoRoot(format!("{}: {e}", root.display())));
        }
        if ours {
            // Owner-only, set after making it rather than left to a umask.
            // Files inside are unreadable by anybody else, but a directory
            // anybody can list is a directory that tells them what somebody is
            // moving — and a filename is often the whole of what is worth
            // knowing.
            platform::keep_to_ourselves(root);
        }
        // Not through a link, even at the top. A root that is a symlink is a
        // root somebody else can move.
        let here = std::fs::symlink_metadata(root)
            .map_err(|e| FileError::NoRoot(format!("{}: {e}", root.display())))?;
        if here.file_type().is_symlink() {
            return Err(FileError::NoRoot(format!(
                "{} is a link; the file directory has to be a real directory",
                root.display()
            )));
        }
        if !here.is_dir() {
            return Err(FileError::NoRoot(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let handle = std::fs::File::open(root)
            .map_err(|e| FileError::NoRoot(format!("{}: {e}", root.display())))?;
        let how = platform::best_confinement(&handle);
        Ok(Self {
            root: handle,
            shown: root.to_path_buf(),
            how,
        })
    }

    /// The same root, held the way an older kernel would have to hold it.
    ///
    /// **For the tests, and only for them.** A fallback nothing exercises is a
    /// fallback that does not work, and the interesting question about this
    /// one is whether it refuses everything the other refuses.
    #[cfg(all(test, unix))]
    fn open_as(root: &Path, how: HowConfined) -> Result<Self, FileError> {
        let mut vault = Self::open(root)?;
        vault.how = how;
        Ok(vault)
    }

    /// Where this is, for a person at this machine.
    #[must_use]
    pub fn shown(&self) -> &Path {
        &self.shown
    }

    /// How a peer is being kept inside it.
    #[must_use]
    pub const fn how(&self) -> HowConfined {
        self.how
    }

    /// Open a file to read, refusing anything that is not a plain file here.
    ///
    /// # Errors
    ///
    /// [`FileError::Outside`] for a path that leaves the root, traverses a
    /// link, or is not there — the three are deliberately one answer.
    pub fn open_read(&self, path: &SafePath) -> Result<std::fs::File, FileError> {
        platform::open_read(self, path)
    }

    /// Make a file that was not there before.
    ///
    /// # Errors
    ///
    /// [`FileError::Exists`] if something is already there — including a
    /// symbolic link, which is *not* replaced.
    pub fn create_new(&self, path: &SafePath) -> Result<std::fs::File, FileError> {
        platform::create_new(self, path)
    }

    /// Open a file that exists, to add to the end of it.
    ///
    /// # Errors
    ///
    /// If it is not there, or is not a plain file.
    pub fn open_append(&self, path: &SafePath) -> Result<std::fs::File, FileError> {
        platform::open_append(self, path)
    }

    /// What is in a directory inside this vault.
    ///
    /// **Opened as a directory through the same handle discipline as a file**,
    /// so a component that becomes a symbolic link between the check and the
    /// read cannot make this describe somewhere else. An empty path is the
    /// root of the vault itself.
    ///
    /// Anything that is neither a plain file nor a directory is left out
    /// rather than described: a peer can do nothing with a socket or a device
    /// node, and naming one only tells them what is on this machine.
    ///
    /// # Errors
    ///
    /// [`FileError::Outside`] if the path is not a directory in this vault.
    pub fn list(&self, path: &SafePath, most: usize) -> Result<(Vec<Listed>, bool), FileError> {
        platform::list(self, path, most)
    }

    /// How large the file is, if it is there and is a plain file.
    #[must_use]
    pub fn size_of(&self, path: &SafePath) -> Option<u64> {
        platform::size_of(self, path)
    }

    /// Whether there is anything at all at this path, link included.
    #[must_use]
    pub fn exists(&self, path: &SafePath) -> bool {
        platform::exists(self, path)
    }

    /// Make the directories above this path.
    ///
    /// # Errors
    ///
    /// If one of them exists and is not a directory, or cannot be made.
    pub fn make_parents(&self, path: &SafePath) -> Result<(), FileError> {
        platform::make_parents(self, path)
    }

    /// Move a file within the root.
    ///
    /// Both ends are inside, and on the same filesystem by construction, so
    /// this is the atomic rename that makes a finished transfer appear all at
    /// once.
    ///
    /// # Errors
    ///
    /// If either path leaves the root or the rename fails.
    pub fn rename(&self, from: &SafePath, to: &SafePath) -> Result<(), FileError> {
        platform::rename(self, from, to)
    }

    /// Delete a file. Missing is not an error: the point is that it is gone.
    ///
    /// # Errors
    ///
    /// If it is there and cannot be removed.
    pub fn remove(&self, path: &SafePath) -> Result<(), FileError> {
        platform::remove(self, path)
    }

    /// How long ago this file was last written, if it is there.
    ///
    /// Used to decide whether a partial transfer is worth resuming or is the
    /// remains of something nobody wants any more.
    #[must_use]
    pub fn age_of(&self, path: &SafePath) -> Option<std::time::Duration> {
        platform::age_of(self, path)
    }

    /// Bytes free on the filesystem the root is on.
    ///
    /// `None` when the platform will not say — in which case a caller checks
    /// nothing rather than inventing a number.
    #[must_use]
    pub fn free_space(&self) -> Option<u64> {
        platform::free_space(self)
    }

    /// Make sure a directory's own entry is on the disk.
    ///
    /// Called after the rename that publishes a finished transfer. Without it
    /// the file's contents are durable and the name is not, which after a
    /// power cut is a directory with nothing in it.
    pub fn sync_dir(&self, path: &SafePath) {
        platform::sync_dir(self, path);
    }
}

#[cfg(unix)]
mod platform {
    use std::os::fd::OwnedFd;

    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    use rustix::io::Errno;

    use super::{FileError, HowConfined, SafePath, Vault};

    /// What a file arriving over a session is created as.
    ///
    /// **Readable by the user who runs this and nobody else.** A file that
    /// came from another machine is not one to make world-readable on a shared
    /// host by default, and widening it afterwards is one command where
    /// narrowing it afterwards is a race.
    const OURS_ONLY: Mode = Mode::from_bits_truncate(0o600);

    /// And the directories made to hold them.
    const OURS_ONLY_DIR: Mode = Mode::from_bits_truncate(0o700);

    /// The flags every open here carries.
    ///
    /// `NOFOLLOW` so a link is refused rather than followed. `NONBLOCK` so a
    /// named pipe inside the root cannot hang this process — opening one
    /// without it waits for the other end, forever, and a peer that can make a
    /// pipe can then stop a session by asking for it.
    fn always() -> OFlags {
        OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
    }

    /// What `openat2` asks the kernel for: stay under the directory, and
    /// resolve no links at all.
    #[cfg(target_os = "linux")]
    fn beneath() -> rustix::fs::ResolveFlags {
        rustix::fs::ResolveFlags::BENEATH | rustix::fs::ResolveFlags::NO_SYMLINKS
    }

    /// Take the group and world bits off a directory this process made.
    pub(super) fn keep_to_ourselves(root: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let Ok(seen) = std::fs::metadata(root) else {
            return;
        };
        let mode = seen.permissions().mode();
        if mode & 0o077 == 0 {
            return;
        }
        let mut narrower = seen.permissions();
        narrower.set_mode(mode & 0o7700);
        if let Err(e) = std::fs::set_permissions(root, narrower) {
            tracing::debug!(error = %e, "could not narrow the file directory");
        }
    }

    /// What the kernel here will do.
    #[cfg(target_os = "linux")]
    pub(super) fn best_confinement(root: &std::fs::File) -> HowConfined {
        // Asked once, with the one path that certainly exists.
        match fs::openat2(
            root,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            beneath(),
        ) {
            Ok(_) => HowConfined::Kernel,
            // Before Linux 5.6, or behind a seccomp filter that does not know
            // the syscall. Neither is a reason to refuse to serve files: the
            // handle walk gives the same guarantee.
            Err(e) => {
                tracing::debug!(error = %e, "openat2 is not available; holding each directory open");
                HowConfined::Handles
            }
        }
    }

    /// **The other Unixes have no `openat2`**, and do not need one: the handle
    /// walk is race-free on all of them. This is where a `RESOLVE_BENEATH`
    /// equivalent would go if one arrived.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn best_confinement(_root: &std::fs::File) -> HowConfined {
        HowConfined::Handles
    }

    /// One component, checked to be one.
    ///
    /// **`O_NOFOLLOW` does not stop `..`.** It refuses a link and says nothing
    /// about a name the kernel resolves upwards on its own, so a walk that
    /// trusted its input would leave the directory by the front door. The name
    /// rules refuse `..` before anything gets here — and this layer refuses it
    /// too, because a second defence that depends on the first is one defence.
    fn plain_component(part: &str) -> Result<&str, FileError> {
        if part.is_empty() || part == "." || part == ".." || part.contains('/') {
            return Err(FileError::Outside);
        }
        Ok(part)
    }

    /// A handle to the root that can be walked from.
    fn from_root(vault: &Vault) -> Result<OwnedFd, FileError> {
        vault
            .root
            .try_clone()
            .map(OwnedFd::from)
            .map_err(|e| FileError::Failed(e.to_string()))
    }

    /// The directory holding this path, opened one component at a time.
    ///
    /// Every step is relative to a handle already held, and every step refuses
    /// a link. **There is nothing to swap**: a component that was a directory
    /// when it was opened is the directory that was opened, whatever the name
    /// points at afterwards.
    fn walk_to_parent(vault: &Vault, path: &SafePath, making: bool) -> Result<OwnedFd, FileError> {
        let mut dir = from_root(vault)?;
        let mut parts: Vec<&str> = path.components().collect();
        parts.pop();
        for part in parts {
            let part = plain_component(part)?;
            let step = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            dir = match fs::openat(&dir, part, step, Mode::empty()) {
                Ok(fd) => fd,
                Err(Errno::NOENT) if making => {
                    fs::mkdirat(&dir, part, OURS_ONLY_DIR).map_err(refusal)?;
                    fs::openat(&dir, part, step, Mode::empty()).map_err(refusal)?
                }
                Err(e) => return Err(refusal(e)),
            };
        }
        Ok(dir)
    }

    /// One syscall, and the refusal is the kernel's.
    #[cfg(target_os = "linux")]
    fn kernel_open(
        vault: &Vault,
        path: &SafePath,
        flags: OFlags,
        mode: Mode,
    ) -> Result<OwnedFd, FileError> {
        fs::openat2(
            &vault.root,
            path.as_str(),
            flags | always(),
            mode,
            beneath(),
        )
        .map_err(refusal)
    }

    /// Never reached where there is no `openat2`, because
    /// [`best_confinement`] never says there is one. Present so the walk is
    /// what runs rather than the build failing.
    #[cfg(not(target_os = "linux"))]
    fn kernel_open(
        vault: &Vault,
        path: &SafePath,
        flags: OFlags,
        mode: Mode,
    ) -> Result<OwnedFd, FileError> {
        walk_open(vault, path, flags, mode)
    }

    /// One open per component, refusing a link at each step.
    fn walk_open(
        vault: &Vault,
        path: &SafePath,
        flags: OFlags,
        mode: Mode,
    ) -> Result<OwnedFd, FileError> {
        let parent = walk_to_parent(vault, path, false)?;
        let name = plain_component(path.file_name())?;
        fs::openat(&parent, name, flags | always(), mode).map_err(refusal)
    }

    /// Open something inside the root, whichever way this machine can.
    fn open_within(
        vault: &Vault,
        path: &SafePath,
        flags: OFlags,
        mode: Mode,
    ) -> Result<std::fs::File, FileError> {
        let fd = match vault.how {
            HowConfined::Kernel => kernel_open(vault, path, flags, mode)?,
            HowConfined::Handles | HowConfined::Checked => walk_open(vault, path, flags, mode)?,
        };
        Ok(std::fs::File::from(fd))
    }

    /// Whether what was opened is a plain file.
    ///
    /// Checked on the **handle**, not on the name, so it is a fact about the
    /// thing now open rather than about something that had that name a moment
    /// ago. A device or a pipe inside the root is not a file to transfer.
    fn is_plain(file: &std::fs::File) -> bool {
        fs::fstat(file).is_ok_and(|s| FileType::from_raw_mode(s.st_mode) == FileType::RegularFile)
    }

    pub(super) fn list(
        vault: &Vault,
        path: &SafePath,
        most: usize,
    ) -> Result<(Vec<super::Listed>, bool), FileError> {
        // `DIRECTORY` makes the kernel refuse anything that is not one, so
        // "is it a directory" is answered by the open rather than by a check
        // beside it. The empty path opens the root itself.
        let dir = if path.as_str().is_empty() {
            fs::openat(
                &vault.root,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|_| FileError::Outside)?
        } else {
            OwnedFd::from(open_within(
                vault,
                path,
                OFlags::RDONLY | OFlags::DIRECTORY,
                Mode::empty(),
            )?)
        };

        let mut entries = Vec::new();
        let mut truncated = false;
        let reader = fs::Dir::read_from(&dir).map_err(|_| FileError::Outside)?;
        for entry in reader {
            let Ok(entry) = entry else { continue };
            let raw = entry.file_name().to_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            if entries.len() >= most {
                truncated = true;
                break;
            }
            let kind = entry.file_type();
            // **Only files and directories.** A peer can do nothing with a
            // socket or a device node, and naming one only says what is on
            // this machine. A symbolic link is left out too: what it points at
            // is a question this vault answers by refusing to follow it.
            let directory = match kind {
                FileType::Directory => true,
                FileType::RegularFile => false,
                _ => continue,
            };
            let name = std::os::unix::ffi::OsStringExt::from_vec(raw.to_vec());
            // The size comes from a stat through the same directory handle, so
            // it is the thing just listed rather than whatever has that name
            // by the time this asks.
            let bytes = if directory {
                0
            } else {
                fs::statat(&dir, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                    .map(|s| s.st_size.try_into().unwrap_or(0))
                    .unwrap_or(0)
            };
            entries.push(super::Listed {
                name,
                bytes,
                directory,
            });
        }
        Ok((entries, truncated))
    }

    pub(super) fn open_read(vault: &Vault, path: &SafePath) -> Result<std::fs::File, FileError> {
        let file = open_within(vault, path, OFlags::RDONLY, Mode::empty())?;
        if !is_plain(&file) {
            return Err(FileError::Outside);
        }
        Ok(file)
    }

    pub(super) fn create_new(vault: &Vault, path: &SafePath) -> Result<std::fs::File, FileError> {
        // `EXCL` is what makes this refuse rather than replace — including
        // refusing a symbolic link that is sitting where the file goes, which
        // is the shape of every "write through a link somebody planted"
        // there is.
        open_within(
            vault,
            path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            OURS_ONLY,
        )
    }

    pub(super) fn open_append(vault: &Vault, path: &SafePath) -> Result<std::fs::File, FileError> {
        let file = open_within(vault, path, OFlags::WRONLY, Mode::empty())?;
        if !is_plain(&file) {
            return Err(FileError::Outside);
        }
        Ok(file)
    }

    pub(super) fn size_of(vault: &Vault, path: &SafePath) -> Option<u64> {
        let file = open_read(vault, path).ok()?;
        let stat = fs::fstat(&file).ok()?;
        u64::try_from(stat.st_size).ok()
    }

    pub(super) fn exists(vault: &Vault, path: &SafePath) -> bool {
        // Without following: something *is* there even when it is a link to
        // nowhere, and a caller asking "is this taken" wants that answer.
        let Ok(parent) = walk_to_parent(vault, path, false) else {
            return false;
        };
        plain_component(path.file_name())
            .is_ok_and(|name| fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).is_ok())
    }

    pub(super) fn make_parents(vault: &Vault, path: &SafePath) -> Result<(), FileError> {
        walk_to_parent(vault, path, true).map(|_| ())
    }

    pub(super) fn rename(vault: &Vault, from: &SafePath, to: &SafePath) -> Result<(), FileError> {
        let old = walk_to_parent(vault, from, false)?;
        let new = walk_to_parent(vault, to, false)?;
        let (was, becomes) = (
            plain_component(from.file_name())?,
            plain_component(to.file_name())?,
        );
        fs::renameat(&old, was, &new, becomes).map_err(refusal)
    }

    pub(super) fn remove(vault: &Vault, path: &SafePath) -> Result<(), FileError> {
        let parent = walk_to_parent(vault, path, false)?;
        let name = plain_component(path.file_name())?;
        match fs::unlinkat(&parent, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(e) => Err(refusal(e)),
        }
    }

    pub(super) fn age_of(vault: &Vault, path: &SafePath) -> Option<std::time::Duration> {
        let file = open_read(vault, path).ok()?;
        let stat = fs::fstat(&file).ok()?;
        let written = u64::try_from(stat.st_mtime).ok()?;
        std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(written))?
            .elapsed()
            .ok()
    }

    pub(super) fn free_space(vault: &Vault) -> Option<u64> {
        let stat = fs::fstatvfs(&vault.root).ok()?;
        // What an unprivileged process may actually use, not what is unused:
        // a filesystem reserves a slice for root, and counting it is how a
        // transfer is accepted and then fails.
        stat.f_bavail.checked_mul(stat.f_frsize)
    }

    pub(super) fn sync_dir(vault: &Vault, path: &SafePath) {
        if let Ok(parent) = walk_to_parent(vault, path, false) {
            let _ = fs::fsync(&parent);
        }
    }

    /// **One answer for three questions.** Whether a path left the root, went
    /// through a link, or was simply not there are all facts about a
    /// filesystem the peer cannot see, and the answer to all three is the
    /// same: it is not a path you have.
    fn refusal(e: Errno) -> FileError {
        match e {
            Errno::NOENT | Errno::NOTDIR | Errno::LOOP | Errno::XDEV | Errno::ACCESS => {
                FileError::Outside
            }
            Errno::EXIST => FileError::Exists,
            other => FileError::Failed(other.to_string()),
        }
    }
}

#[cfg(not(unix))]
mod platform {
    //! The portable one, until each platform has its own.
    //!
    //! **This is the weak version and it is named as such.** Components are
    //! checked not to be links and then the path is opened, which leaves a
    //! window between the two. Windows arrives with its own backend in Phase
    //! 6, using a directory handle and `FILE_OPEN_REPARSE_POINT`; until then a
    //! session on a platform without one says which confinement it has, so an
    //! operator can know rather than assume.

    use std::path::PathBuf;

    use super::{FileError, HowConfined, SafePath, Vault};

    pub(super) fn best_confinement(_root: &std::fs::File) -> HowConfined {
        HowConfined::Checked
    }

    /// Not expressible without the platform's own access control, which
    /// arrives with the platform's own backend.
    pub(super) fn keep_to_ourselves(_root: &std::path::Path) {}

    /// One component, checked to be one.
    ///
    /// **`push` on a `PathBuf` resolves nothing and refuses nothing**: pushing
    /// `..` makes a path that goes up, and pushing an absolute one throws away
    /// everything before it. The name rules refuse both before anything gets
    /// here, and this refuses them again, because a second defence that
    /// depends on the first is one defence.
    fn plain_component(part: &str) -> Result<&str, FileError> {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('/')
            || part.contains('\\')
            || part.contains(':')
        {
            return Err(FileError::Outside);
        }
        Ok(part)
    }

    /// Join the path onto the root, having checked every step of it.
    fn within(vault: &Vault, path: &SafePath) -> Result<PathBuf, FileError> {
        let mut here = vault.shown.clone();
        let mut parts: Vec<&str> = path.components().collect();
        let last = parts.pop().unwrap_or_default();
        for part in parts {
            here.push(plain_component(part)?);
            match std::fs::symlink_metadata(&here) {
                Ok(m) if m.is_dir() => {}
                // A link, a plain file where a directory should be, or nothing.
                Ok(_) | Err(_) => return Err(FileError::Outside),
            }
        }
        here.push(plain_component(last)?);
        Ok(here)
    }

    /// The same, for a path whose directories may not exist yet.
    fn within_unchecked_tail(vault: &Vault, path: &SafePath) -> PathBuf {
        let mut here = vault.shown.clone();
        for part in path.components() {
            match plain_component(part) {
                Ok(part) => here.push(part),
                // Not a path this will make. The caller checks it properly
                // straight afterwards, and a directory made under a name like
                // this is a directory made somewhere else.
                Err(_) => return vault.shown.clone(),
            }
        }
        here
    }

    pub(super) fn list(
        vault: &Vault,
        path: &SafePath,
        most: usize,
    ) -> Result<(Vec<super::Listed>, bool), FileError> {
        let at = if path.as_str().is_empty() {
            vault.shown().to_path_buf()
        } else {
            within(vault, path)?
        };
        let seen = std::fs::symlink_metadata(&at).map_err(|_| FileError::Outside)?;
        if !seen.is_dir() {
            return Err(FileError::Outside);
        }
        let reader = std::fs::read_dir(&at).map_err(|_| FileError::Outside)?;
        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in reader.flatten() {
            if entries.len() >= most {
                truncated = true;
                break;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            // As above: only files and directories, and never through a link.
            let directory = if kind.is_dir() {
                true
            } else if kind.is_file() {
                false
            } else {
                continue;
            };
            let bytes = if directory {
                0
            } else {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            };
            entries.push(super::Listed {
                name: entry.file_name(),
                bytes,
                directory,
            });
        }
        Ok((entries, truncated))
    }

    pub(super) fn open_read(vault: &Vault, path: &SafePath) -> Result<std::fs::File, FileError> {
        let at = within(vault, path)?;
        let seen = std::fs::symlink_metadata(&at).map_err(|_| FileError::Outside)?;
        if !seen.is_file() {
            return Err(FileError::Outside);
        }
        std::fs::File::open(&at).map_err(|e| FileError::Failed(e.to_string()))
    }

    pub(super) fn create_new(vault: &Vault, path: &SafePath) -> Result<std::fs::File, FileError> {
        let at = within(vault, path)?;
        std::fs::File::create_new(&at).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => FileError::Exists,
            _ => FileError::Failed(e.to_string()),
        })
    }

    pub(super) fn open_append(vault: &Vault, path: &SafePath) -> Result<std::fs::File, FileError> {
        let at = within(vault, path)?;
        let seen = std::fs::symlink_metadata(&at).map_err(|_| FileError::Outside)?;
        if !seen.is_file() {
            return Err(FileError::Outside);
        }
        std::fs::OpenOptions::new()
            .write(true)
            .open(&at)
            .map_err(|e| FileError::Failed(e.to_string()))
    }

    pub(super) fn size_of(vault: &Vault, path: &SafePath) -> Option<u64> {
        let at = within(vault, path).ok()?;
        let seen = std::fs::symlink_metadata(&at).ok()?;
        seen.is_file().then_some(seen.len())
    }

    pub(super) fn exists(vault: &Vault, path: &SafePath) -> bool {
        within(vault, path).is_ok_and(|at| std::fs::symlink_metadata(at).is_ok())
    }

    pub(super) fn make_parents(vault: &Vault, path: &SafePath) -> Result<(), FileError> {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        let at = within_unchecked_tail(vault, &parent);
        std::fs::create_dir_all(&at).map_err(|e| FileError::Failed(e.to_string()))?;
        // Made, and then checked: a directory that was created through a link
        // somebody planted is one this refuses to use afterwards.
        within(vault, path).map(|_| ())
    }

    pub(super) fn rename(vault: &Vault, from: &SafePath, to: &SafePath) -> Result<(), FileError> {
        let old = within(vault, from)?;
        let new = within(vault, to)?;
        std::fs::rename(old, new).map_err(|e| FileError::Failed(e.to_string()))
    }

    pub(super) fn remove(vault: &Vault, path: &SafePath) -> Result<(), FileError> {
        let at = within(vault, path)?;
        match std::fs::remove_file(&at) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(FileError::Failed(e.to_string())),
        }
    }

    pub(super) fn age_of(vault: &Vault, path: &SafePath) -> Option<std::time::Duration> {
        let at = within(vault, path).ok()?;
        std::fs::symlink_metadata(at)
            .ok()?
            .modified()
            .ok()?
            .elapsed()
            .ok()
    }

    /// Not available without a syscall this build does not have.
    ///
    /// `None` rather than a guess: a caller that cannot find out checks
    /// nothing, which is honest, where a caller handed an invented number
    /// refuses transfers that would have fitted.
    pub(super) fn free_space(_vault: &Vault) -> Option<u64> {
        None
    }

    pub(super) fn sync_dir(_vault: &Vault, _path: &SafePath) {}
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::io::{Read as _, Write as _};

    use super::*;

    /// A directory that cleans itself up.
    struct Temp(PathBuf);

    impl Temp {
        fn new(what: &str) -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "esper-files-{what}-{stamp}-{:?}",
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&path).expect("mkdir");
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn p(text: &str) -> SafePath {
        SafePath::parse(text).expect("a path the rules accept")
    }

    /// **The suite that matters**, run against every way of confining a peer.
    ///
    /// The name rules cannot catch any of this: `link/passwd` is a perfectly
    /// well-formed relative path with no traversal in it, and what makes it an
    /// escape is a fact about the filesystem rather than about the string. So
    /// this is the other half of the defence, and it has to hold however the
    /// machine is doing it.
    #[cfg(unix)]
    fn escapes_are_refused(vault: &Vault, outside: &Path) {
        // Something worth stealing, outside the root.
        std::fs::write(outside.join("secret"), b"the secret").expect("write");
        std::fs::create_dir_all(outside.join("elsewhere")).expect("mkdir");

        let root = vault.shown().to_path_buf();

        // A link to a file outside.
        symlink(&outside.join("secret"), &root.join("stolen"));
        assert!(
            vault.open_read(&p("stolen")).is_err(),
            "read a file outside the root through a link"
        );
        assert!(
            vault.open_append(&p("stolen")).is_err(),
            "opened a file outside the root for writing through a link"
        );

        // A link to a directory outside, used as a component. **The one the
        // string rules cannot see.**
        symlink(&outside.join("elsewhere"), &root.join("out"));
        assert!(
            vault.create_new(&p("out/planted")).is_err(),
            "wrote outside the root through a directory link"
        );
        assert!(
            vault.open_read(&p("out/anything")).is_err(),
            "read outside the root through a directory link"
        );
        assert!(
            !outside.join("elsewhere/planted").exists(),
            "a file was created outside the root"
        );

        // A link that points back into the root is refused too. Following any
        // link means deciding which ones are safe, and that decision is the
        // bug.
        std::fs::write(root.join("inside"), b"here").expect("write");
        symlink(Path::new("inside"), &root.join("shortcut"));
        assert!(
            vault.open_read(&p("shortcut")).is_err(),
            "a link was followed because it happened to point somewhere allowed"
        );

        // A link sitting where a new file goes is not replaced.
        symlink(&outside.join("secret"), &root.join("target"));
        assert!(
            matches!(vault.create_new(&p("target")), Err(FileError::Exists)),
            "a planted link was overwritten instead of refused"
        );
        assert_eq!(
            std::fs::read(outside.join("secret")).expect("read"),
            b"the secret",
            "the file outside was written through the link"
        );

        // A link to nothing at all. Something *is* there — so it is taken —
        // and it still cannot be read or written through.
        symlink(Path::new("/nowhere-at-all-1234"), &root.join("dangling"));
        assert!(vault.exists(&p("dangling")));
        assert!(vault.open_read(&p("dangling")).is_err());
        assert!(matches!(
            vault.create_new(&p("dangling")),
            Err(FileError::Exists)
        ));

        // And a link pointing at the root's own parent, which is the shape a
        // traversal takes once the string rules have refused `..`.
        symlink(outside, &root.join("up"));
        assert!(vault.open_read(&p("up/secret")).is_err());
        assert!(vault.create_new(&p("up/new")).is_err());
        assert!(!outside.join("new").exists());

        // **A link in the middle that points somewhere allowed.** Following
        // any link at all means deciding which ones are safe, and that
        // decision is where the bug lives: this one stays inside the root, so
        // nothing about *where it goes* refuses it. What refuses it is that it
        // is a link.
        std::fs::create_dir_all(root.join("real")).expect("mkdir");
        std::fs::write(root.join("real/inner"), b"inner").expect("write");
        symlink(Path::new("real"), &root.join("via"));
        assert!(
            vault.open_read(&p("via/inner")).is_err(),
            "a link was followed because it pointed somewhere inside the root"
        );
        assert!(
            vault.create_new(&p("via/planted")).is_err(),
            "a file was written through a link because it pointed somewhere allowed"
        );
        assert!(!root.join("real/planted").exists());
        // The real directory works, so what was refused was the link and not
        // the directory.
        assert!(vault.open_read(&p("real/inner")).is_ok());

        // **What the layer below does when the layer above is wrong.** The
        // name rules refuse `..` and absolute paths, so nothing here should
        // ever see one — which is exactly why it is worth knowing that seeing
        // one would not be an escape.
        for pretend in [
            "../secret",
            "../../etc/passwd",
            "/etc/passwd",
            "a/../../secret",
        ] {
            let unchecked = SafePath::unchecked(pretend);
            assert!(
                vault.open_read(&unchecked).is_err(),
                "{pretend:?} escaped with the name rules bypassed"
            );
            assert!(
                vault.create_new(&unchecked).is_err(),
                "{pretend:?} was written to with the name rules bypassed"
            );
        }
        assert!(!outside.join("passwd").exists());
    }

    #[cfg(unix)]
    fn symlink(target: &Path, at: &Path) {
        let _ = std::fs::remove_file(at);
        std::os::unix::fs::symlink(target, at).expect("symlink");
    }

    /// The kernel doing it.
    #[test]
    #[cfg(unix)]
    fn nothing_escapes_when_the_kernel_is_holding_the_door() {
        let home = Temp::new("kernel");
        let root = home.0.join("root");
        let outside = home.0.join("outside");
        std::fs::create_dir_all(&outside).expect("mkdir");
        let vault = Vault::open(&root).expect("a root");
        // Only meaningful where the syscall is there; where it is not, the
        // walk is what is being tested and the next test covers it.
        if vault.how() != HowConfined::Kernel {
            println!("skipped: this kernel has no openat2");
            return;
        }
        escapes_are_refused(&vault, &outside);
    }

    /// And this code doing it, on a kernel too old for the other.
    #[test]
    #[cfg(unix)]
    fn nothing_escapes_when_each_directory_is_held_open() {
        let home = Temp::new("handles");
        let root = home.0.join("root");
        let outside = home.0.join("outside");
        std::fs::create_dir_all(&outside).expect("mkdir");
        let vault = Vault::open_as(&root, HowConfined::Handles).expect("a root");
        assert_eq!(vault.how(), HowConfined::Handles);
        assert!(vault.how().is_race_free());
        escapes_are_refused(&vault, &outside);
    }

    /// A named pipe inside the root would otherwise stop the session: opening
    /// one waits for the other end, and the other end never comes.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_pipe_inside_the_root_is_refused_rather_than_waited_on() {
        let home = Temp::new("fifo");
        let root = home.0.join("root");
        let vault = Vault::open(&root).expect("a root");
        let made = std::process::Command::new("mkfifo")
            .arg(root.join("pipe"))
            .status();
        if !made.is_ok_and(|s| s.success()) {
            println!("skipped: no mkfifo on this machine");
            return;
        }
        let started = std::time::Instant::now();
        assert!(
            vault.open_read(&p("pipe")).is_err(),
            "a pipe was accepted as a file"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "opening a pipe waited for a writer"
        );
        // It is there, so the transfer that would land on it is refused rather
        // than replacing it.
        assert!(vault.exists(&p("pipe")));
        assert!(matches!(
            vault.create_new(&p("pipe")),
            Err(FileError::Exists)
        ));
    }

    #[test]
    fn the_ordinary_things_work() {
        let home = Temp::new("ordinary");
        let root = home.0.join("root");
        let vault = Vault::open(&root).expect("a root");
        assert_eq!(vault.shown(), root.as_path());
        assert!(!vault.how().describe().is_empty());

        let at = p("reports/2026/q3.txt");
        assert!(!vault.exists(&at));
        assert!(vault.open_read(&at).is_err());

        vault.make_parents(&at).expect("directories");
        let mut file = vault.create_new(&at).expect("a new file");
        file.write_all(b"nine bytes").expect("write");
        drop(file);

        assert!(vault.exists(&at));
        assert_eq!(vault.size_of(&at), Some(10));
        let mut back = String::new();
        vault
            .open_read(&at)
            .expect("read")
            .read_to_string(&mut back)
            .expect("read");
        assert_eq!(back, "nine bytes");

        // A second create is refused rather than truncating what is there.
        assert!(matches!(vault.create_new(&at), Err(FileError::Exists)));

        // Appending to it adds rather than replaces.
        {
            use std::io::Seek as _;
            let mut more = vault.open_append(&at).expect("append");
            more.seek(std::io::SeekFrom::End(0)).expect("seek");
            more.write_all(b"!").expect("write");
        }
        assert_eq!(vault.size_of(&at), Some(11));

        let moved = p("reports/2026/final.txt");
        vault.rename(&at, &moved).expect("rename");
        assert!(!vault.exists(&at));
        assert!(vault.exists(&moved));
        vault.sync_dir(&moved);

        vault.remove(&moved).expect("remove");
        assert!(!vault.exists(&moved));
        // Removing what is not there is not a failure: the point is that it
        // is gone.
        vault.remove(&moved).expect("remove again");
    }

    /// The root itself has to be a real directory the machine's owner named.
    #[test]
    fn a_root_that_is_not_a_real_directory_is_refused() {
        let home = Temp::new("root");
        let file = home.0.join("a-file");
        std::fs::write(&file, b"x").expect("write");
        assert!(matches!(Vault::open(&file), Err(FileError::NoRoot(_))));

        #[cfg(unix)]
        {
            let elsewhere = home.0.join("elsewhere");
            std::fs::create_dir_all(&elsewhere).expect("mkdir");
            let linked = home.0.join("linked-root");
            symlink(&elsewhere, &linked);
            assert!(
                matches!(Vault::open(&linked), Err(FileError::NoRoot(_))),
                "a root that is a link is a root somebody else can move"
            );
        }
    }

    /// A root that is not there yet is made, because the alternative is a
    /// feature that fails until somebody runs `mkdir`.
    #[test]
    fn a_root_that_does_not_exist_yet_is_made() {
        let home = Temp::new("make");
        let root = home.0.join("deep/down/here");
        let vault = Vault::open(&root).expect("a root");
        assert!(root.is_dir());
        assert!(!vault.exists(&p("x")));
    }

    /// **A directory anybody can list is a directory that tells them what
    /// somebody is moving.** The files in it are unreadable by others; the
    /// names would not be.
    #[test]
    #[cfg(unix)]
    fn the_file_directory_is_not_readable_by_anybody_else() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = Temp::new("mode");
        let root = home.0.join("root");
        let vault = Vault::open(&root).expect("a root");
        let mode = std::fs::metadata(&root).expect("stat").permissions().mode();
        assert_eq!(mode & 0o077, 0, "the file directory is {:o}", mode & 0o777);

        // **And a directory that was already there is left exactly as it
        // was.** A person's own working directory is very often the
        // destination of a copy, and narrowing it underneath them would be
        // this changing something it was not asked to touch.
        let theirs = home.0.join("theirs");
        std::fs::create_dir_all(&theirs).expect("mkdir");
        std::fs::set_permissions(&theirs, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let _ = Vault::open(&theirs).expect("a root");
        assert_eq!(
            std::fs::metadata(&theirs)
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "somebody's own directory was narrowed underneath them"
        );

        // And what lands in it, likewise.
        let at = p("secret.txt");
        vault.create_new(&at).expect("a file");
        let file = std::fs::metadata(root.join("secret.txt")).expect("stat");
        assert_eq!(
            file.permissions().mode() & 0o077,
            0,
            "a transferred file is {:o}",
            file.permissions().mode() & 0o777
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_space_reported_is_the_space_a_transfer_can_have() {
        let home = Temp::new("space");
        let vault = Vault::open(&home.0.join("root")).expect("a root");
        let free = vault
            .free_space()
            .expect("a Unix says how much room there is");
        assert!(
            free > 0,
            "no room at all on a filesystem that just took a directory"
        );
        // Sanity against the machine actually having a disk, not a precise
        // figure: what is being checked is that this is bytes rather than
        // blocks, which is a factor of a thousand and the whole point.
        assert!(
            free > 1024,
            "{free} bytes reads like blocks rather than bytes"
        );
    }
}
