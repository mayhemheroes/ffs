use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, SystemTime};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyBmap, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyIoctl, ReplyLock, ReplyLseek, ReplyOpen,
    ReplyStatfs, ReplyWrite, ReplyXTimes, ReplyXattr, Request, TimeOrNow,
};

use tracing::{debug, instrument, warn};

use super::config::Config;

use super::json;

/// A filesystem `FS` is just a vector of nullable inodes, where the index is
/// the inode number.
///
/// NB that inode 0 is always invalid.
#[derive(Debug)]
pub struct FS {
    /// Vector of nullable inodes; the index is the inode number.
    pub inodes: Vec<Option<Inode>>,
    /// Configuration, which determines various file attributes.
    pub config: Config,
}

/// Default TTL on information passed to the OS, which caches responses.
const TTL: Duration = Duration::from_secs(300);

/// An inode, the core structure in the filesystem.
#[derive(Debug)]
pub struct Inode {
    pub parent: u64,
    pub inum: u64,
    pub entry: Entry,
}

#[derive(Debug)]
pub enum Entry {
    // TODO 2021-06-14 need a 'written' flag to determine whether or not to
    // strip newlines during writeback
    File(Vec<u8>),
    Directory(DirType, HashMap<String, DirEntry>),
}

#[derive(Debug)]
pub struct DirEntry {
    pub kind: FileType,
    pub inum: u64,
}

#[derive(Debug)]
pub enum DirType {
    Named,
    List,
}

#[derive(Debug)]
pub enum FSError {
    NoSuchInode(u64),
    InvalidInode(u64),
}

impl FS {
    fn fresh_inode(&mut self, parent: u64, entry: Entry) -> u64 {
        let inum = self.inodes.len() as u64;

        self.inodes.push(Some(Inode {
            parent,
            inum,
            entry,
        }));

        inum
    }

    fn check_access(&self, req: &Request) -> bool {
        req.uid() == self.config.uid
    }

    pub fn get(&self, inum: u64) -> Result<&Inode, FSError> {
        let idx = inum as usize;

        if idx >= self.inodes.len() {
            return Err(FSError::NoSuchInode(inum));
        }

        match &self.inodes[idx] {
            None => Err(FSError::InvalidInode(inum)),
            Some(inode) => Ok(inode),
        }
    }

    fn get_mut(&mut self, inum: u64) -> Result<&mut Inode, FSError> {
        let idx = inum as usize;

        if idx >= self.inodes.len() {
            return Err(FSError::NoSuchInode(inum));
        }

        match self.inodes.get_mut(idx) {
            Some(Some(inode)) => Ok(inode),
            _ => Err(FSError::InvalidInode(inum)),
        }
    }

    fn mode(&self, kind: FileType) -> u16 {
        if kind == FileType::Directory {
            self.config.dirmode
        } else {
            self.config.filemode
        }
    }

    pub fn attr(&self, inode: &Inode) -> FileAttr {
        let size = inode.entry.size();
        let kind = inode.entry.kind();

        let perm = self.mode(kind);

        let nlink: u32 = match &inode.entry {
            Entry::Directory(_, files) => {
                2 + files
                    .iter()
                    .filter(|(_, de)| de.kind == FileType::Directory)
                    .count() as u32
            }
            Entry::File(_) => 1,
        };

        FileAttr {
            ino: inode.inum,
            atime: self.config.timestamp,
            crtime: self.config.timestamp,
            ctime: self.config.timestamp,
            mtime: self.config.timestamp,
            nlink,
            size,
            blksize: 1,
            blocks: size,
            kind,
            uid: self.config.uid,
            gid: self.config.gid,
            perm,
            rdev: 0,
            flags: 0, // weird macOS thing
        }
    }

    /// Syncs the FS with its on-disk representation
    ///
    /// TODO 2021-06-16 need some reference to the output format to do the right thing
    #[instrument(level = "debug", skip(self))]
    pub fn sync(&self) {
        debug!("{:?}", self.inodes);

        json::save_fs(self);
    }
}

impl Entry {
    pub fn size(&self) -> u64 {
        match self {
            Entry::File(s) => s.len() as u64,
            Entry::Directory(DirType::Named, files) => {
                files.iter().map(|(name, _inum)| name.len() as u64).sum()
            }
            Entry::Directory(DirType::List, files) => files.len() as u64,
        }
    }

    pub fn kind(&self) -> FileType {
        match self {
            Entry::File(_) => FileType::RegularFile,
            Entry::Directory(..) => FileType::Directory,
        }
    }
}

impl Filesystem for FS {
    #[instrument(level = "debug")]
    fn destroy(&mut self, _req: &Request) {
        debug!("calling sync");
        self.sync();
        debug!("done syncing");
    }

    #[instrument(level = "debug")]
    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        reply.statfs(0, 0, 0, 0, 0, 1, 255, 0);
    }

    #[instrument(level = "debug")]
    fn access(&mut self, req: &Request, inode: u64, mut mask: i32, reply: ReplyEmpty) {
        if mask == libc::F_OK {
            reply.ok();
            return;
        }

        match self.get(inode) {
            Ok(inode) => {
                // cribbed from https://github.com/cberner/fuser/blob/4639a490f4aa7dfe8a342069a761d4cf2bd8f821/examples/simple.rs#L1703-L1736
                let attr = self.attr(inode);
                let mode = attr.perm as i32;

                if req.uid() == 0 {
                    // root only allowed to exec if one of the X bits is set
                    mask &= libc::X_OK;
                    mask -= mask & (mode >> 6);
                    mask -= mask & (mode >> 3);
                    mask -= mask & mode;
                } else if req.uid() == self.config.uid {
                    mask -= mask & (mode >> 6);
                } else if req.gid() == self.config.gid {
                    mask -= mask & (mode >> 3);
                } else {
                    mask -= mask & mode;
                }

                if mask == 0 {
                    reply.ok();
                } else {
                    reply.error(libc::EACCES);
                }
            }
            Err(_) => reply.error(libc::ENOENT),
        }
    }

    #[instrument(level = "debug")]
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let dir = match self.get(parent) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(inode) => inode,
        };

        let filename = match name.to_str() {
            None => {
                reply.error(libc::ENOENT);
                return;
            }
            Some(name) => name,
        };

        match &dir.entry {
            Entry::Directory(_kind, files) => match files.get(filename) {
                None => {
                    reply.error(libc::ENOENT);
                }
                Some(DirEntry { inum, .. }) => {
                    let file = match self.get(*inum) {
                        Err(_e) => {
                            reply.error(libc::ENOENT);
                            return;
                        }
                        Ok(inode) => inode,
                    };

                    reply.entry(&TTL, &self.attr(file), 0);
                }
            },
            _ => {
                reply.error(libc::ENOTDIR);
            }
        }
    }

    #[instrument(level = "debug")]
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let file = match self.get(ino) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(inode) => inode,
        };

        reply.attr(&TTL, &self.attr(file));
    }

    #[instrument(level = "debug")]
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        _size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        let file = match self.get(ino) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(inode) => inode,
        };

        match &file.entry {
            Entry::File(s) => reply.data(&s[offset as usize..]),
            _ => reply.error(libc::ENOENT),
        }
    }

    #[instrument(level = "debug")]
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let inode = match self.get(ino) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(inode) => inode,
        };

        match &inode.entry {
            Entry::File(_) => reply.error(libc::ENOTDIR),
            Entry::Directory(_kind, files) => {
                let dot_entries = vec![
                    (ino, FileType::Directory, "."),
                    (inode.parent, FileType::Directory, ".."),
                ];

                let entries = files
                    .iter()
                    .map(|(filename, DirEntry { inum, kind })| (*inum, *kind, filename.as_str()));

                for (i, entry) in dot_entries
                    .into_iter()
                    .chain(entries)
                    .into_iter()
                    .enumerate()
                    .skip(offset as usize)
                {
                    if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                        break;
                    }
                }
                reply.ok()
            }
        }
    }

    #[instrument(level = "debug")]
    fn create(
        &mut self,
        _req: &Request<'_>,
        _parent: u64,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        // force the system to use mknod and open
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn mknod(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        // access control
        if !self.check_access(req) {
            reply.error(libc::EACCES);
            return;
        }

        // make sure we have a good file type
        let file_type = mode & libc::S_IFMT as u32;
        if !vec![libc::S_IFREG as u32, libc::S_IFDIR as u32].contains(&file_type) {
            warn!(
                "mknod only supports regular files and directories; got {:o}",
                mode
            );
            reply.error(libc::ENOSYS);
            return;
        }

        // get the filename
        let filename = match name.to_str() {
            None => {
                reply.error(libc::ENOENT);
                return;
            }
            Some(name) => name,
        };

        // make sure the parent exists, is a directory, and doesn't have that file
        match self.get(parent) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(inode) => match &inode.entry {
                Entry::File(_) => {
                    reply.error(libc::ENOTDIR);
                    return;
                }
                Entry::Directory(_dirtype, files) => {
                    if files.contains_key(filename) {
                        reply.error(libc::EEXIST);
                        return;
                    }
                }
            },
        };

        // create the inode entry
        let (entry, kind) = if file_type == libc::S_IFREG as u32 {
            (Entry::File(Vec::new()), FileType::RegularFile)
        } else {
            assert_eq!(file_type, libc::S_IFDIR as u32);
            (
                Entry::Directory(DirType::Named, HashMap::new()),
                FileType::Directory,
            )
        };

        // allocate the inode
        let inum = self.fresh_inode(parent, entry);

        // update the parent
        // NB we can't get_mut the parent earlier due to borrowing restrictions
        match self.get_mut(parent) {
            Err(_e) => unreachable!("error finding parent again"),
            Ok(inode) => match &mut inode.entry {
                Entry::File(_) => unreachable!("parent changed to a regular file"),
                Entry::Directory(_dirtype, files) => {
                    files.insert(filename.into(), DirEntry { kind, inum });
                }
            },
        };

        reply.entry(&TTL, &self.attr(self.get(inum).unwrap()), 0);
    }

    #[instrument(level = "debug")]
    fn mkdir(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        if mode != 0o755 {
            warn!("Given mode {:o}, using 755", mode);
        }
        if !self.check_access(req) {
            reply.error(libc::EACCES);
            return;
        }

        // get the new directory name
        let filename = match name.to_str() {
            None => {
                reply.error(libc::ENOENT);
                return;
            }
            Some(name) => name,
        };

        // make sure the parent exists, is a directory, and doesn't have anything with that name
        match self.get(parent) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(inode) => match &inode.entry {
                Entry::File(_) => {
                    reply.error(libc::ENOTDIR);
                    return;
                }
                Entry::Directory(_dirtype, files) => {
                    if files.contains_key(filename) {
                        reply.error(libc::EEXIST);
                        return;
                    }
                }
            },
        };

        // create the inode entry
        let entry = Entry::Directory(DirType::Named, HashMap::new());
        let kind = FileType::Directory;

        // allocate the inode
        let inum = self.fresh_inode(parent, entry);

        // update the parent
        // NB we can't get_mut the parent earlier due to borrowing restrictions
        match self.get_mut(parent) {
            Err(_e) => unreachable!("error finding parent again"),
            Ok(inode) => match &mut inode.entry {
                Entry::File(_) => unreachable!("parent changed to a regular file"),
                Entry::Directory(_dirtype, files) => {
                    files.insert(filename.into(), DirEntry { kind, inum });
                }
            },
        };

        reply.entry(&TTL, &self.attr(self.get(inum).unwrap()), 0);
    }

    #[instrument(level = "debug")]
    fn write(
        &mut self,
        req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        assert!(offset >= 0);

        // access control
        if !self.check_access(req) {
            reply.error(libc::EACCES);
            return;
        }

        // find inode
        let file = match self.get_mut(ino) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(inode) => inode,
        };

        // load contents
        let contents = match &mut file.entry {
            Entry::File(contents) => contents,
            Entry::Directory(_, _) => {
                reply.error(libc::EISDIR);
                return;
            }
        };

        // make space
        let extra_bytes = (offset + data.len() as i64) - contents.len() as i64;
        if extra_bytes > 0 {
            contents.resize(contents.len() + extra_bytes as usize, 0);
        }

        // actually write
        let offset = offset as usize;
        contents[offset..offset + data.len()].copy_from_slice(data);

        reply.written(data.len() as u32);
    }

    #[instrument(level = "debug")]
    fn unlink(&mut self, req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        // access control
        if !self.check_access(req) {
            reply.error(libc::EACCES);
            return;
        }

        // get the filename
        let filename = match name.to_str() {
            None => {
                reply.error(libc::ENOENT);
                return;
            }
            Some(name) => name,
        };

        // find the parent
        let files = match self.get_mut(parent) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(Inode {
                entry: Entry::Directory(_dirtype, files),
                ..
            }) => files,
            Ok(Inode {
                entry: Entry::File(_),
                ..
            }) => {
                reply.error(libc::ENOTDIR);
                return;
            }
        };

        // ensure it's a regular file
        match files.get(filename) {
            Some(DirEntry {
                kind: FileType::RegularFile,
                ..
            }) => (),
            _ => {
                reply.error(libc::EPERM);
                return;
            }
        }

        // try to remove it
        let res = files.remove(filename);
        assert!(res.is_some());
        reply.ok();
    }

    #[instrument(level = "debug")]
    fn rmdir(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        // access control
        if !self.check_access(req) {
            reply.error(libc::EACCES);
            return;
        }

        // get the filename
        let filename = match name.to_str() {
            None => {
                reply.error(libc::ENOENT);
                return;
            }
            Some(name) => name,
        };

        // find the parent
        let files = match self.get(parent) {
            Err(_e) => {
                reply.error(libc::ENOENT);
                return;
            }
            Ok(Inode {
                entry: Entry::Directory(_dirtype, files),
                ..
            }) => files,
            Ok(Inode {
                entry: Entry::File(_),
                ..
            }) => {
                reply.error(libc::ENOTDIR);
                return;
            }
        };

        // find the actual directory being deleted
        let inum = match files.get(filename) {
            Some(DirEntry {
                kind: FileType::Directory,
                inum,
            }) => inum,
            Some(_) => {
                reply.error(libc::ENOTDIR);
                return;
            }
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        // make sure it's empty
        match self.get(*inum) {
            Ok(Inode {
                entry: Entry::Directory(_, dir_files),
                ..
            }) => {
                if !dir_files.is_empty() {
                    reply.error(libc::ENOTEMPTY);
                    return;
                }
            }
            Ok(_) => unreachable!("mismatched metadata on inode {} in parent {}", inum, parent),
            _ => unreachable!("couldn't find inode {} in parent {}", inum, parent),
        };

        // find the parent again, mutably
        let files = match self.get_mut(parent) {
            Ok(Inode {
                entry: Entry::Directory(_dirtype, files),
                ..
            }) => files,
            Ok(_) => unreachable!("parent changed to a regular file"),
            Err(_) => unreachable!("error finding parent again"),
        };

        // try to remove it
        let res = files.remove(filename);
        assert!(res.is_some());
        reply.ok();
    }

    #[instrument(level = "debug")]
    fn rename(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32, // TODO 2021-06-14 support RENAME_ flags
        reply: ReplyEmpty,
    ) {
        // access control
        if !self.check_access(req) {
            reply.error(libc::EACCES);
            return;
        }

        let src = match name.to_str() {
            None => {
                reply.error(libc::ENOENT);
                return;
            }
            Some(name) => name,
        };

        if src == "." || src == ".." {
            reply.error(libc::EINVAL);
            return;
        }

        let tgt = match newname.to_str() {
            None => {
                reply.error(libc::ENOENT);
                return;
            }
            Some(name) => name,
        };

        // make sure src exists
        let (src_kind, src_inum) = match self.get(parent) {
            Ok(Inode {
                entry: Entry::Directory(_kind, files),
                ..
            }) => match files.get(src) {
                Some(DirEntry { kind, inum }) => (*kind, *inum),
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            },
            _ => {
                reply.error(libc::ENOENT);
                return;
            }
        };
        // determine whether tgt exists
        let tgt_info = match self.get(newparent) {
            Ok(Inode {
                entry: Entry::Directory(_kind, files),
                ..
            }) => match files.get(tgt) {
                Some(DirEntry { kind, inum }) => {
                    if src_kind != *kind {
                        reply.error(libc::ENOTDIR);
                        return;
                    }
                    Some((*kind, *inum))
                }
                None => None,
            },
            _ => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        // if tgt exists and is a directory, make sure it's empty
        if let Some((FileType::Directory, tgt_inum)) = tgt_info {
            match self.get(tgt_inum) {
                Ok(Inode {
                    entry: Entry::Directory(_type, files),
                    ..
                }) => {
                    if !files.is_empty() {
                        reply.error(libc::ENOTEMPTY);
                        return;
                    }
                }
                _ => unreachable!("bad metadata on inode {} in {}", tgt_inum, newparent),
            }
        }
        // remove src from parent
        match self.get_mut(parent) {
            Ok(Inode {
                entry: Entry::Directory(_kind, files),
                ..
            }) => files.remove(src),
            _ => unreachable!("parent changed"),
        };

        // add src as tgt to newparent
        match self.get_mut(newparent) {
            Ok(Inode {
                entry: Entry::Directory(_kind, files),
                ..
            }) => files.insert(
                tgt.into(),
                DirEntry {
                    kind: src_kind,
                    inum: src_inum,
                },
            ),
            _ => unreachable!("parent changed"),
        };

        // set src's parent inode
        match self.get_mut(src_inum) {
            Ok(inode) => inode.parent = newparent,
            Err(_) => unreachable!(
                "missing inode {} moved from {} to {}",
                src_inum, parent, newparent
            ),
        }

        reply.ok();
    }

    #[instrument(level = "debug")]
    fn fallocate(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
        if offset < 0 || length <= 0 {
            reply.error(libc::EINVAL);
            return;
        }

        if mode != 0 {
            reply.error(libc::EOPNOTSUPP);
            return;
        }

        // access control
        if !self.check_access(req) {
            reply.error(libc::EACCES);
            return;
        }

        // load the contents
        let contents = match self.get_mut(ino) {
            Ok(Inode {
                entry: Entry::File(contents),
                ..
            }) => contents,
            Ok(Inode {
                entry: Entry::Directory(..),
                ..
            }) => {
                reply.error(libc::EBADF);
                return;
            }
            Err(_e) => {
                reply.error(libc::ENODEV);
                return;
            }
        };

        // extend the vector
        let extra_bytes = (offset + length as i64) - contents.len() as i64;
        if extra_bytes > 0 {
            contents.resize(contents.len() + extra_bytes as usize, 0);
        }

        reply.ok()
    }

    #[instrument(level = "debug")]
    fn fsync(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        // TODO 2021-06-16 not really what fsync is meant to mean (it's per inode)
        self.sync();
        reply.ok();
    }

    // TODO
    #[instrument(level = "debug")]
    fn copy_file_range(
        &mut self,
        _req: &Request<'_>,
        _ino_in: u64,
        _fh_in: u64,
        _offset_in: i64,
        _ino_out: u64,
        _fh_out: u64,
        _offset_out: i64,
        _len: u64,
        _flags: u32,
        reply: ReplyWrite,
    ) {
        reply.error(libc::ENOSYS);
    }

    // TODO
    #[instrument(level = "debug")]
    fn ioctl(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: u32,
        _cmd: u32,
        _in_data: &[u8],
        _out_size: u32,
        reply: ReplyIoctl,
    ) {
        reply.error(libc::ENOSYS);
    }

    // Unimplemented/default-implementation calls
    #[instrument(level = "debug")]
    fn forget(&mut self, _req: &Request<'_>, _ino: u64, _nlookup: u64) {}

    #[instrument(level = "debug")]
    fn setattr(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn readlink(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyData) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn symlink(
        &mut self,
        _req: &Request<'_>,
        _parent: u64,
        _name: &OsStr,
        _link: &Path,
        reply: ReplyEntry,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn link(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _newparent: u64,
        _newname: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn open(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: ReplyOpen) {
        // TODO 2021-06-16 access check?
        reply.opened(0, 0);
    }

    #[instrument(level = "debug")]
    fn flush(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }
    #[instrument(level = "debug")]
    fn opendir(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: ReplyOpen) {
        reply.opened(0, 0);
    }

    #[instrument(level = "debug")]
    fn readdirplus(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        reply: ReplyDirectoryPlus,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn releasedir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    #[instrument(level = "debug")]
    fn fsyncdir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn setxattr(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _name: &OsStr,
        _value: &[u8],
        _flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _name: &OsStr,
        _size: u32,
        reply: ReplyXattr,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn listxattr(&mut self, _req: &Request<'_>, _ino: u64, _size: u32, reply: ReplyXattr) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn removexattr(&mut self, _req: &Request<'_>, _ino: u64, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn getlk(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: i32,
        _pid: u32,
        reply: ReplyLock,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn setlk(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: i32,
        _pid: u32,
        _sleep: bool,
        reply: ReplyEmpty,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn bmap(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _blocksize: u32,
        _idx: u64,
        reply: ReplyBmap,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[instrument(level = "debug")]
    fn lseek(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        _whence: i32,
        reply: ReplyLseek,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[cfg(target_os = "macos")]
    #[instrument(level = "debug")]
    fn setvolname(&mut self, _req: &Request<'_>, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(libc::ENOSYS);
    }

    #[cfg(target_os = "macos")]
    #[instrument(level = "debug")]
    fn exchange(
        &mut self,
        _req: &Request<'_>,
        _parent: u64,
        _name: &OsStr,
        _newparent: u64,
        _newname: &OsStr,
        _options: u64,
        reply: ReplyEmpty,
    ) {
        reply.error(libc::ENOSYS);
    }

    #[cfg(target_os = "macos")]
    #[instrument(level = "debug")]
    fn getxtimes(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyXTimes) {
        reply.error(libc::ENOSYS);
    }
}
