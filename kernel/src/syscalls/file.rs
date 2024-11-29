use alloc::sync::Arc;
use filesystem_abstractions::{
    FileDescriptor, FileDescriptorBuilder, FileMode, FrozenFileDescriptorBuilder, ICacheableFile,
    IInode, OpenFlags, PipeBuilder,
};
use paging::{IWithPageGuardBuilder, PageTableEntryFlags};

use super::{ISyncSyscallHandler, SyscallContext, SyscallResult};

pub struct WriteSyscall;

impl ISyncSyscallHandler for WriteSyscall {
    fn handle(&self, ctx: &mut SyscallContext<'_>) -> SyscallResult {
        let fd = ctx.arg0::<usize>();
        let p_buf = ctx.arg1::<*const u8>();
        let len = ctx.arg2::<usize>();

        let fd = ctx.tcb.fd_table.lock().get(fd).ok_or(-1isize)?;

        if !fd.can_write() {
            return Err(-1);
        }

        let buf = unsafe { core::slice::from_raw_parts(p_buf, len) };

        match ctx
            .tcb
            .borrow_page_table()
            .guard_slice(buf)
            .mustbe_user()
            .with_read()
        {
            Some(guard) => Ok(fd.access().write(&guard) as isize),
            None => Err(-1),
        }
    }

    fn name(&self) -> &str {
        "sys_write"
    }
}

pub struct Pipe2Syscall;

impl ISyncSyscallHandler for Pipe2Syscall {
    fn handle(&self, ctx: &mut SyscallContext) -> SyscallResult {
        struct FdPair {
            read_end: i32,
            write_end: i32,
        }

        let p_fd = ctx.arg0::<*mut FdPair>();

        match ctx
            .tcb
            .borrow_page_table()
            .guard_ptr(p_fd)
            .mustbe_user()
            .with_write()
        {
            Some(mut guard) => {
                let pipe_pair = PipeBuilder::open();

                let mut fd_table = ctx.tcb.fd_table.lock();

                match fd_table.allocate(pipe_pair.read_end_builder) {
                    Some(read_end) => guard.read_end = read_end as i32,
                    None => return Err(-1),
                }

                match fd_table.allocate(pipe_pair.write_end_builder) {
                    Some(write_end) => guard.write_end = write_end as i32,
                    None => {
                        fd_table.remove(guard.read_end as usize);
                        return Err(-1);
                    }
                }

                Ok(0)
            }
            None => Err(-1),
        }
    }

    fn name(&self) -> &str {
        "sys_pipe2"
    }
}

pub struct OpenAtSyscall;

impl ISyncSyscallHandler for OpenAtSyscall {
    fn handle(&self, ctx: &mut SyscallContext) -> SyscallResult {
        let dirfd = ctx.arg0::<isize>();
        let p_path = ctx.arg1::<*const u8>();
        let flags = ctx.arg2::<OpenFlags>();
        let _mode = ctx.arg3::<FileMode>();

        if dirfd < 0 && dirfd != FileDescriptor::AT_FDCWD {
            return Err(-1);
        }

        match ctx
            .tcb
            .borrow_page_table()
            .guard_cstr(p_path, 1024)
            .must_have(PageTableEntryFlags::User | PageTableEntryFlags::Readable)
        {
            Some(guard) => {
                let dir_inode: Arc<dyn IInode> = if dirfd == FileDescriptor::AT_FDCWD {
                    let cwd = unsafe { ctx.tcb.cwd.get().as_ref().unwrap() };
                    filesystem_abstractions::lookup_inode(cwd).ok_or(-1isize)?
                } else {
                    let fd_table = ctx.tcb.fd_table.lock();
                    let fd = fd_table.get(dirfd as usize).ok_or(-1isize)?;
                    fd.access().inode().ok_or(-1isize)?
                };

                let path = core::str::from_utf8(&guard).map_err(|_| -1isize)?;
                let path = path::remove_relative_segments(path);
                let filename = path::get_filename(&path);
                let parent_inode_path = path::get_directory_name(&path).ok_or(-1isize)?;

                let inode: Arc<dyn IInode>;
                match dir_inode.lookup_recursive(&path) {
                    Ok(i) => inode = i,
                    Err(_) => {
                        if flags.contains(OpenFlags::O_CREAT) {
                            let parent_inode = dir_inode
                                .lookup_recursive(parent_inode_path)
                                .map_err(|_| -1isize)?;

                            let new_inode = parent_inode.touch(filename).map_err(|_| -1isize)?;

                            inode = new_inode;
                        } else {
                            return Err(-1);
                        }
                    }
                }

                let opened_file = filesystem_abstractions::open_file(inode, flags, 0).clear_type();

                let accessor = opened_file.cache_as_arc_accessor();

                let builder = FileDescriptorBuilder::new(accessor)
                    .set_readable()
                    .set_writable()
                    .freeze();

                let mut fd_table = ctx.tcb.fd_table.lock();
                match fd_table.allocate(builder) {
                    Some(fd) => Ok(fd as isize),
                    None => Err(-1),
                }
            }
            None => Err(-1),
        }
    }

    fn name(&self) -> &str {
        "sys_openat"
    }
}

pub struct CloseSyscall;

impl ISyncSyscallHandler for CloseSyscall {
    fn handle(&self, ctx: &mut SyscallContext) -> SyscallResult {
        let fd = ctx.arg0::<usize>();

        ctx.tcb.fd_table.lock().remove(fd); // rc to file will be dropped as the fd is removed
                                            // and when rc is 0, the opened file will be dropped

        Ok(0)
    }

    fn name(&self) -> &str {
        "sys_close"
    }
}

pub struct DupSyscall;

impl ISyncSyscallHandler for DupSyscall {
    fn handle(&self, ctx: &mut SyscallContext) -> SyscallResult {
        let fd = ctx.arg0::<usize>();

        let mut fd_table = ctx.tcb.fd_table.lock();
        match fd_table.get(fd) {
            Some(old) => {
                let builder = FrozenFileDescriptorBuilder::deconstruct(&old);
                match fd_table.allocate(builder) {
                    Some(newfd) => Ok(newfd as isize),
                    None => Err(-1),
                }
            }
            None => Err(-1),
        }
    }

    fn name(&self) -> &str {
        "sys_dup"
    }
}

pub struct Dup3Syscall;

impl ISyncSyscallHandler for Dup3Syscall {
    fn handle(&self, ctx: &mut SyscallContext) -> SyscallResult {
        let oldfd = ctx.arg0::<usize>();
        let newfd = ctx.arg1::<usize>();
        let _flags = ctx.arg2::<usize>();

        if oldfd == newfd {
            return Ok(newfd as isize);
        }

        let mut fd_table = ctx.tcb.fd_table.lock();
        match fd_table.get(oldfd) {
            Some(old) => {
                let builder = FrozenFileDescriptorBuilder::deconstruct(&old);

                // if newfd is already open, close it
                if fd_table.get(newfd).is_some() {
                    fd_table.remove(newfd);
                }

                match fd_table.allocate_at(builder, newfd) {
                    Some(newfd) => Ok(newfd as isize),
                    None => Err(-1),
                }
            }
            None => Err(-1),
        }
    }

    fn name(&self) -> &str {
        "sys_dup3"
    }
}
