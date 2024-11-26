use core::sync::atomic::Ordering;

use abstractions::operations::IUsizeAlias;
use address::{IPageNum, IToPageNum, VirtualAddress};
use log::debug;
use paging::{IWithPageGuardBuilder, PageTableEntryFlags};
use tasks::TaskStatus;

use crate::timing::ITimer;

use super::{ISyncSyscallHandler, SyscallContext, SyscallResult};

pub struct ExitSyscall;

impl ISyncSyscallHandler for ExitSyscall {
    fn handle(&self, ctx: &mut SyscallContext<'_>) -> SyscallResult {
        let code = ctx.arg0::<isize>();

        *ctx.tcb.task_status.lock() = TaskStatus::Exited;
        ctx.tcb
            .exit_code
            .store(code as i32, core::sync::atomic::Ordering::Relaxed);

        debug!("Task {} exited with code {}", ctx.tcb.task_id.id(), code);
        Ok(0)
    }

    fn name(&self) -> &str {
        "sys_exit"
    }
}

#[repr(C)]
struct Tms {
    tms_utime: i64,
    tms_stime: i64,
    tms_cutime: i64,
    tms_cstime: i64,
}

pub struct TimesSyscall;

impl ISyncSyscallHandler for TimesSyscall {
    fn handle(&self, ctx: &mut SyscallContext) -> SyscallResult {
        let p_tms = ctx.arg0::<*mut Tms>();

        let memory_space = ctx.tcb.memory_space.lock();
        match memory_space
            .page_table()
            .guard_ptr(p_tms)
            .must_have(PageTableEntryFlags::User)
            .with(PageTableEntryFlags::Writable)
        {
            Some(mut guard) => {
                let user_timer = ctx.tcb.timer.lock().clone();
                let kernel_timer: tasks::UserTaskTimer = ctx.tcb.kernel_timer.lock().clone();

                guard.tms_utime = user_timer.elapsed().total_microseconds() as i64;
                guard.tms_stime = kernel_timer.elapsed().total_microseconds() as i64;
                // TODO: calculate tms_cutime and tms_cstime

                Ok(0)
            }
            None => Err(-1),
        }
    }

    fn name(&self) -> &str {
        "sys_times"
    }
}

pub struct BrkSyscall;

impl ISyncSyscallHandler for BrkSyscall {
    fn handle(&self, ctx: &mut SyscallContext) -> SyscallResult {
        let brk = ctx.arg0::<usize>();

        let current_brk = ctx.tcb.brk_pos.load(Ordering::Relaxed);

        if brk == 0 || brk == current_brk {
            return Ok(current_brk as isize);
        }

        if brk < current_brk {
            return Err(-1);
        }

        let mut memory_space = ctx.tcb.memory_space.lock();
        let brk_area = memory_space.brk_page_range();

        // new brk is in the same page, no need to allocate new pages
        // Only update brk position
        let brk_page_end = brk_area.end().start_addr::<VirtualAddress>().as_usize();
        if brk <  brk_page_end {
            ctx.tcb.brk_pos.store(brk, Ordering::Relaxed);
            return Ok(brk as isize);
        }

        let va = VirtualAddress::from_usize(brk);
        let vpn = va.to_ceil_page_num(); // end is exclusive

        match memory_space.increase_brk(vpn) {
            Ok(_) => {
                ctx.tcb.brk_pos.store(brk, Ordering::Relaxed);
                Ok(brk as isize)
            }
            Err(reason) => {
                debug!("Failed to increase brk to {:#x}, reason: {}", brk, reason);
                Err(-1)
            }
        }
    }

    fn name(&self) -> &str {
        "sys_brk"
    }
}
