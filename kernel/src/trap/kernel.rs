use core::arch::asm;

use riscv::register::stvec;

pub fn set_kernel_trap_handler() {
    unsafe { stvec::write(__on_kernel_trap as usize, stvec::TrapMode::Direct) };
}

// #[naked]
#[no_mangle]
#[link_section = ".text.trampoline"]
unsafe extern "C" fn __on_kernel_trap() -> ! {
    asm!("unimp", options(noreturn));
}

#[naked]
#[no_mangle]
#[link_section = ".text.trampoline"]
unsafe extern "C" fn __return_from_kernel_trap() -> ! {
    asm!("unimp", options(noreturn));
}

#[no_mangle]
extern "C" fn __kernel_trap_handler() -> ! {
    unsafe { __return_from_kernel_trap() };
}
