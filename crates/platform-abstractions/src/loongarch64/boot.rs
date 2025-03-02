use core::arch::global_asm;

use loongArch64::{
    self,
    register::{pgdh, pgdl, pwch, pwcl, stlbps, tlbidx, tlbrehi, tlbrentry},
};
use platform_specific::virt_to_phys;

#[naked]
#[no_mangle]
#[link_section = ".text.entry"] // Don't rename, cross crates inter-operation
pub unsafe extern "C" fn _start() -> ! {
    ::core::arch::naked_asm!("
            ori         $t0, $zero, 0x1     # CSR_DMW1_PLV0
            lu52i.d     $t0, $t0, -2048     # UC, PLV0, 0x8000 xxxx xxxx xxxx
            csrwr       $t0, 0x180          # LOONGARCH_CSR_DMWIN0
            ori         $t0, $zero, 0x11    # CSR_DMW1_MAT | CSR_DMW1_PLV0
            lu52i.d     $t0, $t0, -1792     # CA, PLV0, 0x9000 xxxx xxxx xxxx
            csrwr       $t0, 0x181          # LOONGARCH_CSR_DMWIN1

            # Setup stack for main thread
            la.global   $sp, __tmp_stack_top

            # Initialize virtual memory
            bl          {init_boot_page_table}
            bl          {init_mmu}          # setup boot page table and enabel MMU
            invtlb      0x00, $r0, $r0

            # Enable PG 
            li.w		$t0, 0xb0		# PLV=0, IE=0, PG=1
            csrwr		$t0, 0x0        # LOONGARCH_CSR_CRMD
            li.w		$t0, 0x00		# PLV=0, PIE=0, PWE=0
            csrwr		$t0, 0x1        # LOONGARCH_CSR_PRMD
            li.w		$t0, 0x00		# FPE=0, SXE=0, ASXE=0, BTE=0
            csrwr		$t0, 0x2        # LOONGARCH_CSR_EUEN

            # aka. u0 in Linux
            csrrd       $r21, 0x20           # cpuid
            la.global   $t0, __kernel_start_main

            # We can't use bl to jump to higher address, so we use jirl to jump to higher address.
            jirl        $zero, $t0, 0
            ",
        init_boot_page_table = sym init_boot_page_table,
        init_mmu = sym init_mmu,
    )
}

global_asm!(
    "
.section .text
.balign 4096
.global handle_tlb_refill
handle_tlb_refill:
         csrwr   $t0, 0x8b               # LA_CSR_TLBRSAVE, KScratch for TLB refill exception
         csrrd   $t0, 0x1b               # LA_CSR_PGD, Page table base
         lddir   $t0, $t0, 3
         lddir   $t0, $t0, 2
         lddir   $t0, $t0, 1
         ldpte   $t0, 0
         ldpte   $t0, 1
         tlbfill
         csrrd   $t0, 0x8b               # LA_CSR_TLBRSAVE
         ertn
"
);

extern "C" {
    fn handle_tlb_refill();
}

/// Init the TLB configuration and set tlb refill handler.
unsafe fn init_tlb() {
    // Page Size 4KB
    const PS_4K: usize = 0x0c;
    tlbidx::set_ps(PS_4K);
    stlbps::set_ps(PS_4K);
    tlbrehi::set_ps(PS_4K);

    // Set Page table entry width
    pwcl::set_pte_width(8);
    // Set Page table width and offset
    pwcl::set_ptbase(12);
    pwcl::set_ptwidth(9);
    pwcl::set_dir1_base(21);
    pwcl::set_dir1_width(9);
    pwcl::set_dir2_base(30);
    pwcl::set_dir2_width(9);
    pwch::set_dir3_base(39);
    pwch::set_dir3_width(9);

    let paddr = virt_to_phys(handle_tlb_refill as usize);
    tlbrentry::set_tlbrentry(paddr);
}

unsafe fn init_mmu() {
    init_tlb();

    let paddr = virt_to_phys(&raw const PT_L0 as usize);
    pgdh::set_base(paddr);
    pgdl::set_base(0);
}

// Huge Page Mapping Flags: V | D | HUGE | P | W
const HUGE_FLAGS: u64 = (1 << 0) | (1 << 1) | (1 << 6) | (1 << 7) | (1 << 8);

#[link_section = ".data.prepage"]
static mut PT_L0: [u64; 512] = [0; 512];

#[link_section = ".data.prepage"]
static mut PT_L1: [u64; 512] = {
    let mut pt_l1 = [0; 512];
    // 0x0000_0000..0x4000_0000, VRWX_GAD, 1G block
    pt_l1[0] = HUGE_FLAGS;
    // 0x8000_0000..0xc000_0000, VRWX_GAD, 1G block
    pt_l1[2] = 0x8000_0000 | HUGE_FLAGS;
    pt_l1
};

unsafe fn init_boot_page_table() {
    unsafe {
        let l1_va = &raw const PT_L1 as usize;
        // 0x0000_0000_0000 ~ 0x0080_0000_0000, table
        PT_L0[0] = virt_to_phys(l1_va) as u64;
    }
}
