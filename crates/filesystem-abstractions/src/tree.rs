use address::{IPageNum, PhysicalAddress};
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    sync::Weak,
    vec::Vec,
};
use allocation::TrackedFrame;
use constants::SyscallError;
use core::{cell::UnsafeCell, usize};
use hermit_sync::{RwSpinLock, SpinMutex};
use timing::TimeSpec;

use crate::{
    DirectoryEntry, DirectoryEntryType, FileMetadata, FileStatistics, FileSystemError,
    FileSystemResult, IInode, InodeMetadata, OpenFlags, OpenedDiskInode,
};

struct RamFileInode {
    inner: RwSpinLock<RamFileInodeInner>,
}

impl RamFileInode {
    fn new(filename: &str) -> Self {
        RamFileInode {
            inner: RwSpinLock::new(RamFileInodeInner {
                frames: Vec::new(),
                size: 0,
                filename: filename.to_string(),
            }),
        }
    }
}

impl IInode for RamFileInode {
    fn metadata(&self) -> FileSystemResult<InodeMetadata> {
        let inner = unsafe { self.inner.data_ptr().as_ref().unwrap() };

        Ok(InodeMetadata {
            filename: &inner.filename,
            entry_type: DirectoryEntryType::File,
            size: inner.size,
            children_count: 0,
        })
    }

    fn writeat(&self, offset: usize, buffer: &[u8]) -> FileSystemResult<usize> {
        let mut inner = self.inner.write();

        let end_size = offset + buffer.len();

        if end_size > inner.size {
            let required_pages = (end_size + 4095) / 4096;
            inner.frames.resize_with(required_pages, || {
                // TODO: do we have to zero the memory?
                allocation::alloc_frame().expect("Out of memory")
            });
            inner.size = end_size;
        }

        let mut current = offset;
        for frame in &inner.frames[offset / 4096..end_size / 4096 + 1] {
            let in_page_start = current % 4096;
            let in_page_len = usize::min(4096, end_size - current);

            let data_ptr = unsafe {
                frame
                    .ppn()
                    .start_addr::<PhysicalAddress>()
                    .to_high_virtual()
                    .as_mut_ptr::<u8>()
            };
            let data_slice = unsafe {
                core::slice::from_raw_parts_mut(data_ptr.add(in_page_start), in_page_len)
            };

            data_slice.copy_from_slice(&buffer[current - offset..current - offset + in_page_len]);

            current += in_page_len;
        }

        Ok(current - offset)
    }

    fn readat(&self, offset: usize, buffer: &mut [u8]) -> FileSystemResult<usize> {
        let inner = self.inner.read();

        if offset >= inner.size {
            return Ok(0);
        }

        let end_size = usize::min(inner.size, offset + buffer.len());

        let mut current = offset;
        while current < end_size {
            let frame = &inner.frames[current / 4096];
            let in_page_start = current % 4096;
            let in_page_len = usize::min(4096, end_size - current);

            let data_ptr = unsafe {
                frame
                    .ppn()
                    .start_addr::<PhysicalAddress>()
                    .to_high_virtual()
                    .as_ptr::<u8>()
            };
            let data_slice =
                unsafe { core::slice::from_raw_parts(data_ptr.add(in_page_start), in_page_len) };

            buffer[current - offset..current - offset + in_page_len].copy_from_slice(data_slice);

            current += in_page_len;
        }

        Ok(current - offset)
    }

    fn stat(&self, stat: &mut FileStatistics) -> FileSystemResult<()> {
        let inner = self.inner.read();

        stat.device_id = 0;
        stat.inode_id = 0;
        stat.mode = crate::FileStatisticsMode::FILE;
        stat.link_count = 1;
        stat.uid = 0;
        stat.gid = 0;
        stat.size = inner.size as u64;
        stat.block_size = 4096; // PAGE_SIZE
        stat.block_count = inner.frames.len() as u64;
        stat.rdev = 0;

        stat.ctime = TimeSpec::zero();
        stat.mtime = TimeSpec::zero();
        stat.atime = TimeSpec::zero();

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MountError {
    InvalidInput,
    NotADirectory,
    FileExists,
    FileNotExists,
    AlreadyMounted,
}

impl MountError {
    fn to_filesystem_error(self) -> FileSystemError {
        match self {
            MountError::InvalidInput => FileSystemError::InvalidInput,
            MountError::NotADirectory => FileSystemError::NotADirectory,
            MountError::FileExists => FileSystemError::AlreadyExists,
            MountError::FileNotExists => FileSystemError::NotFound,
            MountError::AlreadyMounted => FileSystemError::InvalidInput,
        }
    }

    pub fn to_syscall_error(self) -> Result<isize, isize> {
        match self {
            MountError::InvalidInput => SyscallError::InvalidArgument,
            MountError::NotADirectory => SyscallError::NoSuchFileOrDirectory,
            MountError::FileExists => SyscallError::FileExists,
            MountError::FileNotExists => SyscallError::NoSuchFileOrDirectory,
            MountError::AlreadyMounted => SyscallError::DeviceOrResourceBusy,
        }
    }
}

enum DirectoryTreeNodeMetadata {
    Inode { inode: Arc<dyn IInode> },
    Empty,
}

struct DirectoryTreeNodeInner {
    meta: DirectoryTreeNodeMetadata,
    name: String,
    mounted: BTreeMap<String, Arc<DirectoryTreeNode>>,
    opened: BTreeMap<String, Weak<DirectoryTreeNode>>,
    shadowed: Option<Arc<DirectoryTreeNode>>,
}

impl DirectoryTreeNodeInner {
    fn get_mounted(&self, name: &str) -> Option<Arc<DirectoryTreeNode>> {
        self.mounted.get(name).cloned()
    }

    fn is_mounted(&self, name: &str) -> bool {
        self.get_mounted(name).is_some()
    }
}

pub struct DirectoryTreeNode {
    parent: Option<Arc<DirectoryTreeNode>>,
    weak_self: UnsafeCell<Weak<DirectoryTreeNode>>,
    inner: Arc<SpinMutex<DirectoryTreeNodeInner>>,
}

unsafe impl Send for DirectoryTreeNode {}
unsafe impl Sync for DirectoryTreeNode {}

impl DirectoryTreeNode {
    pub fn open_as_file(
        self: Arc<DirectoryTreeNode>,
        flags: OpenFlags,
        offset: usize,
    ) -> Arc<OpenedDiskInode> {
        Arc::new(OpenedDiskInode {
            metadata: Arc::new(FileMetadata::open(self, flags, offset)),
        })
    }

    fn set_weak(self: &Arc<DirectoryTreeNode>) {
        unsafe { *self.weak_self.get().as_mut().unwrap() = Arc::downgrade(self) }
    }

    fn self_arc(&self) -> Option<Arc<DirectoryTreeNode>> {
        let weak = unsafe { self.weak_self.get().as_ref() }?;

        weak.upgrade()
    }

    pub fn from_empty(
        parent: Option<Arc<DirectoryTreeNode>>,
        name: String,
    ) -> Arc<DirectoryTreeNode> {
        let arc = Arc::new(DirectoryTreeNode {
            parent,
            weak_self: UnsafeCell::new(Weak::new()),
            inner: Arc::new(SpinMutex::new(DirectoryTreeNodeInner {
                meta: DirectoryTreeNodeMetadata::Empty,
                name,
                mounted: BTreeMap::new(),
                opened: BTreeMap::new(),
                shadowed: None,
            })),
        });

        arc.set_weak();

        arc
    }

    pub fn from_inode(
        parent: Option<Arc<DirectoryTreeNode>>,
        inode: &Arc<dyn IInode>,
        inode_meta: Option<&InodeMetadata>,
        name: Option<&str>,
    ) -> Arc<DirectoryTreeNode> {
        let arc = Arc::new(DirectoryTreeNode {
            parent,
            weak_self: UnsafeCell::new(Weak::new()),
            inner: Arc::new(SpinMutex::new(DirectoryTreeNodeInner {
                meta: DirectoryTreeNodeMetadata::Inode {
                    inode: inode.clone(),
                },
                name: name
                    .unwrap_or(inode_meta.map(|m| m.filename).unwrap_or_default())
                    .to_string(),
                mounted: BTreeMap::new(),
                opened: BTreeMap::new(),
                shadowed: None,
            })),
        });

        arc.set_weak();

        arc
    }

    pub fn mount_as(
        self: &Arc<DirectoryTreeNode>,
        inode: &Arc<dyn IInode>,
        name: Option<&str>,
    ) -> Result<Arc<DirectoryTreeNode>, MountError> {
        let name = match name {
            Some(n) => n,
            None => {
                let inode_meta = inode.metadata().map_err(|_| MountError::InvalidInput)?;

                inode_meta.filename
            }
        };

        // We actually don't care what the name of the inode to be mounted is,
        // as the 'mount' operation always gives a new name to it, which is the key of the mount list
        let inode = Self::from_inode(
            Some(self.clone()),
            inode,
            inode.metadata().as_ref().ok(),
            Some(name),
        );

        if let Some(mounted) = self.inner.lock().mounted.remove(name) {
            let mut new_inner = inode.inner.lock();

            new_inner.shadowed = Some(mounted);
        }

        self.inner
            .lock()
            .mounted
            .insert(name.to_string(), inode.clone())
            .map_or_else(|| Ok(inode), |_| Err(MountError::FileExists))
    }

    pub fn mount_empty(
        self: &Arc<DirectoryTreeNode>,
        name: &str,
    ) -> Result<Arc<DirectoryTreeNode>, MountError> {
        let mut inner = self.inner.lock();

        let inode = Self::from_empty(Some(self.clone()), name.to_string());

        if let Some(mounted) = inner.mounted.remove(name) {
            let mut new_inner = inode.inner.lock();

            new_inner.shadowed = Some(mounted);
        }

        inner
            .mounted
            .insert(name.to_string(), inode.clone())
            .map_or_else(|| Ok(inode), |_| Err(MountError::FileExists))
    }

    pub fn umount_at(&self, name: &str) -> Result<Arc<DirectoryTreeNode>, MountError> {
        let umounted = self
            .inner
            .lock()
            .mounted
            .remove(name)
            .ok_or(MountError::FileNotExists)?;

        let shadowed = umounted.inner.lock().shadowed.take();

        if let Some(shadowed) = shadowed {
            let self_arc = self.self_arc().expect("Unable to get self arc");
            let mut self_inner = self_arc.inner.lock();

            self_inner.mounted.insert(name.to_string(), shadowed);
        }

        Ok(umounted)
    }

    pub fn name(&self) -> &str {
        self.name_internal()
    }

    fn name_internal(&self) -> &'static str {
        unsafe { &self.inner.data_ptr().as_ref().unwrap().name }
    }

    pub fn close(&self, name: &str) -> (bool, bool) {
        let mut inner = self.inner.lock();

        let closed = inner.opened.remove(name).is_some();
        let unmounted = inner.mounted.remove(name).is_some();

        (closed, unmounted)
    }

    pub fn open(
        self: &Arc<DirectoryTreeNode>,
        path: &str,
    ) -> FileSystemResult<Arc<DirectoryTreeNode>> {
        global_open(path, Some(self))
    }

    pub fn open_child(
        self: &Arc<DirectoryTreeNode>,
        name: &str,
    ) -> FileSystemResult<Arc<DirectoryTreeNode>> {
        debug_assert!(!name.contains(path::SEPARATOR));

        if name == path::CURRENT_DIRECTORY || name.is_empty() {
            return Ok(self.clone());
        }

        if name == path::PARENT_DIRECTORY {
            return self.parent.as_ref().map_or_else(
                || Ok(self.clone()),
                |parent: &Arc<DirectoryTreeNode>| Ok(parent.clone()),
            );
        }

        // prevent dead lock in lookup method
        {
            let inner = self.inner.lock();

            // mounted node has higher priority, as it can shadow the opened node
            if !inner.mounted.is_empty() {
                if let Some(mounted) = inner.mounted.get(name).cloned() {
                    return Ok(mounted);
                }
            }

            if !inner.opened.is_empty() {
                if let Some(opened) = inner.opened.get(name).and_then(|weak| weak.upgrade()) {
                    return Ok(opened);
                }
            }
        }

        #[allow(deprecated)]
        let inode = self.lookup(name)?;
        let inode_meta = inode.as_ref().metadata()?;

        let opened = Self::from_inode(Some(self.clone()), &inode, Some(&inode_meta), None);

        self.inner
            .lock()
            .opened
            .insert(name.to_string(), Arc::downgrade(&opened));

        Ok(opened)
    }

    // if the node was opened in the tree, this returns the full path in the filesystem.
    // if not, the root is considered the deepest node without parent
    pub fn fullpath(&self) -> String {
        let mut stack = Vec::new();

        let mut current = self.self_arc().expect("Unable to get self arc");
        stack.push(current.name_internal());

        while let Some(parent) = &current.parent {
            current = parent.clone();
            stack.push(current.name_internal());
        }

        let size = stack.iter().map(|s| s.len()).sum::<usize>() + stack.len();
        let mut path = String::with_capacity(size);

        while let Some(part) = stack.pop() {
            path.push_str(part);

            if !stack.is_empty() {
                path.push_str(path::SEPARATOR_STR);
            }
        }

        path
    }
}

impl Drop for DirectoryTreeNode {
    fn drop(&mut self) {
        if let Some(ref parent) = self.parent {
            parent.close(self.name());
        }
    }
}

impl IInode for DirectoryTreeNode {
    fn metadata(&self) -> FileSystemResult<InodeMetadata> {
        let inner = self.inner.lock();
        let filename = self.name();

        match &inner.meta {
            DirectoryTreeNodeMetadata::Inode { inode } => {
                let meta = inode.metadata()?;

                Ok(InodeMetadata {
                    filename,
                    entry_type: meta.entry_type,
                    size: meta.size,
                    children_count: meta.children_count,
                })
            }
            DirectoryTreeNodeMetadata::Empty => Ok(InodeMetadata {
                filename,
                entry_type: DirectoryEntryType::Directory,
                size: 0,
                children_count: inner.mounted.len(),
            }),
        }
    }

    fn lookup(&self, name: &str) -> FileSystemResult<Arc<dyn IInode>> {
        // We dont't use DirectoryTreeNode::open because this method only cares the lookup process,
        // it doesn't mean the inode has to be opened.
        let inner = self.inner.lock();

        #[allow(deprecated)]
        match &inner.meta {
            DirectoryTreeNodeMetadata::Inode { inode } => inode.lookup(name),
            DirectoryTreeNodeMetadata::Empty => Err(FileSystemError::NotFound),
        }
    }

    fn readat(&self, offset: usize, buffer: &mut [u8]) -> FileSystemResult<usize> {
        match &self.inner.lock().meta {
            DirectoryTreeNodeMetadata::Inode { inode } => inode.readat(offset, buffer),
            DirectoryTreeNodeMetadata::Empty => Err(FileSystemError::NotAFile),
        }
    }

    fn writeat(&self, offset: usize, buffer: &[u8]) -> FileSystemResult<usize> {
        match &self.inner.lock().meta {
            DirectoryTreeNodeMetadata::Inode { inode } => inode.writeat(offset, buffer),
            DirectoryTreeNodeMetadata::Empty => Err(FileSystemError::NotAFile),
        }
    }

    fn mkdir(&self, name: &str) -> FileSystemResult<Arc<dyn IInode>> {
        let inner = self.inner.lock();

        if inner.is_mounted(name) {
            return Err(FileSystemError::AlreadyExists);
        }

        match &inner.meta {
            DirectoryTreeNodeMetadata::Inode { inode } => inode.mkdir(name),
            DirectoryTreeNodeMetadata::Empty => {
                drop(inner); // release lock, as mount operation requires lock

                let self_arc = self.self_arc().expect("Unable to get self arc");
                self_arc
                    .mount_empty(name)
                    .map_err(|e| e.to_filesystem_error())
                    .map(|inode| inode as Arc<dyn IInode>)
            }
        }
    }

    fn rmdir(&self, name: &str) -> FileSystemResult<()> {
        // FIXME: Do we have to check if it's a directory?
        if self.close(name).1 {
            return Ok(());
        }

        match &self.inner.lock().meta {
            DirectoryTreeNodeMetadata::Inode { inode } => inode.rmdir(name),
            DirectoryTreeNodeMetadata::Empty => Ok(()), // same as below
        }
    }

    fn remove(&self, name: &str) -> FileSystemResult<()> {
        if self.close(name).1 {
            return Ok(());
        }

        match &self.inner.lock().meta {
            DirectoryTreeNodeMetadata::Inode { inode } => inode.remove(name),
            DirectoryTreeNodeMetadata::Empty => Ok(()), // already removed in close method
        }
    }

    fn touch(&self, name: &str) -> FileSystemResult<Arc<dyn IInode>> {
        let inner = self.inner.lock();

        match &inner.meta {
            DirectoryTreeNodeMetadata::Inode { inode } => inode.touch(name),
            DirectoryTreeNodeMetadata::Empty => {
                drop(inner); // release lock, as mount operation requires lock

                let self_arc = self.self_arc().expect("Unable to get self arc");
                let ram_inode: Arc<dyn IInode> = Arc::new(RamFileInode::new(name));

                self_arc
                    .mount_as(&ram_inode, Some(name))
                    .map_err(|e| e.to_filesystem_error())
                    .map(|inode| inode as Arc<dyn IInode>)
            }
        }
    }

    fn read_dir(&self) -> FileSystemResult<Vec<DirectoryEntry>> {
        let inner = self.inner.lock();

        // TODO: handle "." and ".." entries

        // If the directory itself was mounted as its child, we have to be care of potential deadlock,
        // so we copy a list of the list.
        let mounted = inner.mounted.clone();
        let mounted_entries =
            mounted
                .iter()
                .map(|(name, mounted)| match &mounted.inner.lock().meta {
                    DirectoryTreeNodeMetadata::Inode { inode } => {
                        let inode_meta = inode.metadata().expect("Mounted node with no metadata");

                        DirectoryEntry {
                            filename: name.clone(),
                            entry_type: inode_meta.entry_type,
                        }
                    }
                    DirectoryTreeNodeMetadata::Empty => DirectoryEntry {
                        filename: name.clone(),
                        entry_type: DirectoryEntryType::Directory,
                    },
                });

        // FIXME: handle opened files, there may be inodes that were shadowed by other inodes

        match &inner.meta {
            DirectoryTreeNodeMetadata::Inode { inode } => {
                let inode = inode.clone();
                drop(inner); // release lock, in case the node it self is mounted as its children

                let mut entries = inode.read_dir()?;

                for entry in mounted_entries {
                    if mounted.get(&entry.filename).is_none() {
                        entries.push(entry);
                    }
                }

                Ok(entries)
            }
            DirectoryTreeNodeMetadata::Empty => Ok(mounted_entries.collect()),
        }
    }

    fn stat(&self, stat: &mut FileStatistics) -> FileSystemResult<()> {
        match &self.inner.lock().meta {
            DirectoryTreeNodeMetadata::Inode { inode } => inode.stat(stat),
            DirectoryTreeNodeMetadata::Empty => {
                stat.device_id = 0;
                stat.inode_id = 0;
                stat.mode = crate::FileStatisticsMode::DIR;
                stat.link_count = 1;
                stat.uid = 0;
                stat.gid = 0;
                stat.size = 0;
                stat.block_size = 512;
                stat.block_count = 0;
                stat.rdev = 0;

                stat.ctime = TimeSpec::zero();
                stat.mtime = TimeSpec::zero();
                stat.atime = TimeSpec::zero();

                Ok(())
            }
        }
    }
}

// The root of the directory tree
static mut ROOT: SpinMutex<Option<Arc<DirectoryTreeNode>>> = SpinMutex::new(None);

pub fn initialize() {
    let root = DirectoryTreeNode::from_empty(None, String::new());

    for node in [
        "boot", "dev", "etc", "home", "root", "opt", "mnt", "proc", "sys", "tmp", "run", "usr",
        "var",
    ]
    .iter()
    {
        root.mount_empty(node).unwrap();
    }

    unsafe {
        *ROOT.lock() = Some(root);
    }
}

pub fn global_open(
    path: &str,
    relative_to: Option<&Arc<DirectoryTreeNode>>,
) -> FileSystemResult<Arc<DirectoryTreeNode>> {
    let root = match (relative_to, path::is_path_fully_qualified(path)) {
        (_, true) => unsafe { ROOT.lock().as_ref().unwrap().clone() },
        (Some(root), false) => root.clone(),
        (None, false) => return Err(FileSystemError::InvalidInput),
    };

    let parts = path.split(path::SEPARATOR).skip_while(|d| d.is_empty());

    let mut current = root;
    for part in parts {
        current = current.open_child(part)?;
    }

    Ok(current)
}

pub fn global_umount(
    path: &str,
    relative_to: Option<&Arc<DirectoryTreeNode>>,
) -> Result<Arc<DirectoryTreeNode>, MountError> {
    let root = match (relative_to, path::is_path_fully_qualified(path)) {
        (_, true) => {
            let mut root = unsafe { ROOT.lock() };

            let root_node = root.as_ref().unwrap();

            // Umount root, restoring shadowed node
            if path.trim_start_matches(path::SEPARATOR).is_empty() {
                let mut root_inner = root_node.inner.lock();
                let previous_root = root_inner
                    .shadowed
                    .take()
                    .unwrap_or(DirectoryTreeNode::from_empty(None, String::new()));

                drop(root_inner);

                *root = Some(previous_root.clone());

                return Ok(previous_root);
            }

            root_node.clone()
        }
        (Some(root), false) => root.clone(),
        (None, false) => return Err(MountError::InvalidInput),
    };

    let parent_path = path::get_directory_name(path).unwrap_or("");
    let name = path::get_filename(path);

    let parent = global_open(parent_path, Some(&root)).map_err(|_| MountError::FileNotExists)?;

    parent.umount_at(name)
}

pub fn global_mount(
    inode: &Arc<dyn IInode>,
    path: &str,
    relative_to: Option<&Arc<DirectoryTreeNode>>,
) -> Result<Arc<DirectoryTreeNode>, MountError> {
    log::info!(
        "Mounting {} at {}",
        inode
            .metadata()
            .map(|m| m.filename)
            .unwrap_or("anonymous inode"),
        path
    );

    let root = match (relative_to, path::is_path_fully_qualified(path)) {
        (_, true) => {
            let mut root = unsafe { ROOT.lock() };

            // new root
            if path.trim_start_matches(path::SEPARATOR).is_empty() {
                let new_root = DirectoryTreeNode::from_inode(None, inode, None, None);
                new_root.inner.lock().shadowed = root.take();

                *root = Some(new_root.clone());

                return Ok(new_root);
            }

            root.as_ref().unwrap().clone()
        }
        (Some(root), false) => root.clone(),
        (None, false) => return Err(MountError::InvalidInput),
    };

    let parent_path = path::get_directory_name(path).unwrap_or("");
    let name = path::get_filename(path);

    let parent = global_open(parent_path, Some(&root)).map_err(|_| MountError::FileNotExists)?;

    parent.mount_as(inode, Some(name))
}

struct RamFileInodeInner {
    frames: Vec<TrackedFrame>,
    size: usize,
    filename: String,
}
