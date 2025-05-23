use crate::{ISyscallContext, ISyscallContextMut, SyscallContext};

impl<TPayload> SyscallContext<'_, TPayload> {
    #[inline]
    fn arg_i<T: Sized + Copy>(&self, i: usize) -> T {
        debug_assert!(core::mem::size_of::<T>() <= core::mem::size_of::<usize>());
        debug_assert!(i <= 5);

        let arg0 = unsafe {
            (self.trap_ctx as *const _ as *const usize).add(9 /* offset of a0 */)
        };

        // Since RISCV is little-endian, we can safely cast usize to T
        unsafe { arg0.add(i).cast::<T>().read() }
    }
}

impl<TPayload> ISyscallContext for SyscallContext<'_, TPayload> {
    #[inline(always)]
    fn syscall_id(&self) -> usize {
        self.trap_ctx.regs.a7
    }

    #[inline(always)]
    fn arg0<T: Sized + Copy>(&self) -> T {
        self.arg_i(0)
    }

    #[inline(always)]
    fn arg1<T: Sized + Copy>(&self) -> T {
        self.arg_i(1)
    }

    #[inline(always)]
    fn arg2<T: Sized + Copy>(&self) -> T {
        self.arg_i(2)
    }

    #[inline(always)]
    fn arg3<T: Sized + Copy>(&self) -> T {
        self.arg_i(3)
    }

    #[inline(always)]
    fn arg4<T: Sized + Copy>(&self) -> T {
        self.arg_i(4)
    }

    #[inline(always)]
    fn arg5<T: Sized + Copy>(&self) -> T {
        self.arg_i(5)
    }
}

impl<TPayload> ISyscallContextMut for SyscallContext<'_, TPayload> {
    #[inline(always)]
    fn move_to_next_instruction(&mut self) {
        self.trap_ctx.sepc += 4; // size of `ecall` instruction
    }

    #[inline(always)]
    fn set_return_value(&mut self, value: usize) {
        self.trap_ctx.regs.a0 = value;
    }
}
