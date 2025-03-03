use constants::PROCESSOR_COUNT;

// Saved context for coroutine
// Following calling convention that only caller-saved registers are saved
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct CoroutineSavedContext {
    saved: [usize; 12], // 36 - 47
    kra: usize,         // kernel return address, 34
    ksp: usize,         // kernel sp, 35
}

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
struct KernelThreadContext {
    pub hartid: usize,
    pub ctx: CoroutineSavedContext,
}

static mut THREAD_CONTEXT_POOL: [KernelThreadContext; PROCESSOR_COUNT] =
    unsafe { core::mem::zeroed() };

// # Safety
// Can only be called once for each thread.
pub unsafe fn init_thread_info() {
    let hartid = platform_specific::tp();
    let p_ctx = unsafe { &raw mut THREAD_CONTEXT_POOL[hartid] }.cast::<usize>();

    p_ctx.write_volatile(hartid);

    // Coroutine saved context
    let p_ctx = p_ctx.add(1) as usize;

    ::core::arch::asm!("mv tp, {}", in(reg) p_ctx);
}
