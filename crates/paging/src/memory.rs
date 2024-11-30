use core::{slice, usize};

use abstractions::IUsizeAlias;
use alloc::{collections::BTreeMap, vec::Vec};

use address::{
    IAlignableAddress, IPageNum, IToPageNum, PhysicalAddress, PhysicalPageNum, VirtualAddress,
    VirtualAddressRange, VirtualPageNum, VirtualPageNumRange,
};
use allocation::{alloc_frame, TrackedFrame};
use log::debug;
use xmas_elf::ElfFile;

use crate::{page_table, PageTable, PageTableEntryFlags};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapType {
    Identity,
    Framed,
    Direct,
    Linear,
}

/// Layout of a typical user memory space
/// +------------------+ <- MEMORY_END
/// |    Unallocated   |
/// |      Frames      |
/// +------------------+ <- += 0x0008_0000 (8MB)
/// |   kernel heap    |   
/// +------------------+ <- ekernel
/// | High half kernel |
/// +------------------+ <- 0xffff_ffc0_8020_0000
/// |    Mapped SBI    |
/// +------------------+ <- 0xffff_ffc0_4000_0000
/// |    Mapped MMIO   |
/// +------------------+ <- 0xffff_ffc0_0000_0000
/// |                  |
/// |                  |
/// |                  |
/// |       void       |
/// |                  |
/// |                  |
/// |                  |
/// +------------------+ <- += 0x0000
/// |       Brk        |       empty at the beginning, dynamically grows or shrinks
/// +------------------+ <- += 0x1000
/// | Stack Guard Top  |
/// +------------------+ <- += USER_STACK_SIZE
/// |                  |
/// |    User stack    |
/// |                  |
/// +------------------+ <- += 0x1000
/// | Stack Guard Base |
/// +------------------+ <- 0x0000_0000_0060_0000
/// |                  |
/// |        ELF       |
/// |                  |
/// +------------------+ <- 0x0000_8000_0000_1000
/// |                  |
/// +------------------+ <- 0x0000_0000_0000_0000
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaType {
    UserElf,
    UserStackGuardBase,
    UserStack,
    UserStackGuardTop,
    UserBrk,
    Kernel,
}

pub struct MappingArea {
    range: VirtualPageNumRange,
    area_type: AreaType,
    map_type: MapType,
    allocated_frames: BTreeMap<VirtualPageNum, TrackedFrame>,
    permissions: PageTableEntryFlags,
}

impl MappingArea {
    pub fn vpn_range(&self) -> VirtualPageNumRange {
        self.range
    }

    pub fn permissions(&self) -> PageTableEntryFlags {
        self.permissions
    }

    pub fn map_type(&self) -> AreaType {
        self.area_type
    }

    pub fn new(
        range: VirtualPageNumRange,
        area_type: AreaType,
        map_type: MapType,
        permissions: PageTableEntryFlags,
    ) -> Self {
        Self {
            range,
            area_type,
            map_type,
            allocated_frames: BTreeMap::new(),
            permissions,
        }
    }

    pub fn clone_from(them: &MappingArea) -> Self {
        Self {
            range: them.range,
            area_type: them.area_type,
            map_type: them.map_type,
            allocated_frames: BTreeMap::new(),
            permissions: them.permissions,
        }
    }

    pub fn contains(&self, vpn: VirtualPageNum) -> bool {
        self.range.contains(vpn)
    }

    pub fn has_ownership_of(&self, vpn: VirtualPageNum) -> bool {
        self.allocated_frames.contains_key(&vpn)
    }
}

impl MappingArea {
    fn apply_mapping_single(
        &mut self,
        vpn: VirtualPageNum,
        frame: Option<TrackedFrame>,
        register_to_table: &mut impl FnMut(VirtualPageNum, PhysicalPageNum, PageTableEntryFlags),
    ) {
        let frame = frame.unwrap_or(alloc_frame().unwrap());
        register_to_table(vpn, frame.ppn(), self.permissions);
        self.allocated_frames.insert(vpn, frame); // this takes ownership of the frame, so add it last
    }

    pub fn apply_mapping(
        &mut self,
        mut register_to_table: impl FnMut(VirtualPageNum, PhysicalPageNum, PageTableEntryFlags),
    ) {
        for vpn in self.range.iter() {
            self.apply_mapping_single(vpn, None, &mut register_to_table);
        }
    }

    fn revoke_mapping_single(
        &mut self,
        vpn: VirtualPageNum,
        revoke_from_table: &mut impl FnMut(VirtualPageNum),
    ) {
        revoke_from_table(vpn);
        drop(self.allocated_frames.remove(&vpn));
    }

    pub fn revoke_mapping(&mut self, mut revoke_from_table: impl FnMut(VirtualPageNum)) {
        for vpn in self.range.iter() {
            self.revoke_mapping_single(vpn, &mut revoke_from_table);
        }
    }
}

pub struct MemorySpace {
    page_table: PageTable,
    mapping_areas: Vec<MappingArea>,
    brk_area_idx: usize,
    brk_start: VirtualAddress,
    stack_guard_base: VirtualAddressRange,
    stack_range: VirtualAddressRange,
    stack_gurad_top: VirtualAddressRange,
    elf_area: VirtualAddressRange,
}

impl MemorySpace {
    pub fn map_area(&mut self, mut area: MappingArea) {
        area.apply_mapping(|vpn, ppn, flags| {
            self.page_table.map_single(vpn, ppn, flags);
        });
        self.mapping_areas.push(area);
    }

    pub fn unmap_first_area_that(&mut self, predicate: &impl Fn(&MappingArea) -> bool) -> bool {
        match self.mapping_areas.iter().position(predicate) {
            Some(index) => {
                let mut area = self.mapping_areas.remove(index);
                area.revoke_mapping(|vpn| {
                    self.page_table.unmap_single(vpn);
                });
                true
            }
            None => false,
        }
    }

    pub fn unmap_all_areas_that(&mut self, predicate: impl Fn(&MappingArea) -> bool) {
        while self.unmap_first_area_that(&predicate) {
            // do nothing
        }
    }

    pub fn unmap_area_starts_with(&mut self, vpn: VirtualPageNum) -> bool {
        self.unmap_first_area_that(&|area| area.range.start() == vpn)
    }
}

impl MemorySpace {
    pub fn brk_start(&self) -> VirtualAddress {
        self.brk_start
    }

    pub fn brk_page_range(&self) -> VirtualPageNumRange {
        self.mapping_areas[self.brk_area_idx].vpn_range()
    }

    pub fn increase_brk(&mut self, new_end_vpn: VirtualPageNum) -> Result<(), &str> {
        let brk_area = &mut self.mapping_areas[self.brk_area_idx];

        if new_end_vpn < brk_area.range.start() {
            return Err("New end is less than the current start");
        }

        let old_end_vpn = brk_area.range.end();
        let page_count = new_end_vpn.diff_page_count(old_end_vpn);

        if page_count == 0 {
            return Ok(());
        }

        let increased_range =
            VirtualPageNumRange::from_start_count(old_end_vpn, page_count as usize);

        for vpn in increased_range.iter() {
            brk_area.apply_mapping_single(vpn, None, &mut |vpn, ppn, flags| {
                self.page_table.map_single(vpn, ppn, flags);
            });
        }

        brk_area.range = VirtualPageNumRange::from_start_end(brk_area.range.start(), new_end_vpn);

        Ok(())
    }

    pub fn shrink_brk(&mut self, new_end_vpn: VirtualPageNum) -> Result<(), &str> {
        let brk_area = &mut self.mapping_areas[self.brk_area_idx];

        if new_end_vpn > brk_area.range.end() {
            return Err("New end is greater than the current end");
        }

        if new_end_vpn < brk_area.range.start() {
            return Err("New end is less than the current start");
        }

        let old_end_vpn = brk_area.range.end();
        let page_count = old_end_vpn.diff_page_count(new_end_vpn);

        if page_count == 0 {
            return Ok(());
        }

        let decreased_range =
            VirtualPageNumRange::from_start_count(new_end_vpn, page_count as usize);

        for vpn in decreased_range.iter() {
            brk_area.revoke_mapping_single(vpn, &mut |vpn| {
                self.page_table.unmap_single(vpn);
            });
        }

        brk_area.range = VirtualPageNumRange::from_start_end(brk_area.range.start(), new_end_vpn);

        Ok(())
    }
}

impl MemorySpace {
    pub fn empty() -> Self {
        Self {
            page_table: PageTable::allocate(),
            mapping_areas: Vec::new(),
            brk_area_idx: usize::MAX,
            brk_start: VirtualAddress::from_usize(usize::MAX),
            stack_guard_base: VirtualAddressRange::from_start_len(
                VirtualAddress::from_usize(usize::MAX),
                0,
            ),
            stack_range: VirtualAddressRange::from_start_len(
                VirtualAddress::from_usize(usize::MAX),
                0,
            ),
            stack_gurad_top: VirtualAddressRange::from_start_len(
                VirtualAddress::from_usize(usize::MAX),
                0,
            ),
            elf_area: VirtualAddressRange::from_start_len(
                VirtualAddress::from_usize(usize::MAX),
                0,
            ),
        }
    }

    pub fn satp(&self) -> usize {
        self.page_table.satp()
    }

    pub fn page_table(&self) -> &PageTable {
        &self.page_table
    }

    pub fn page_table_mut(&mut self) -> &mut PageTable {
        &mut self.page_table
    }

    // Checks if the memory space the current active memory space
    pub fn is_activated(&self) -> bool {
        self.page_table.is_activated()
    }
}

impl MemorySpace {
    // Clone the existing memory space
    pub fn clone_existing(them: &MemorySpace) -> Self {
        let mut this = Self::empty();

        this.register_kernel_area();

        for area in them.mapping_areas.iter() {
            let my_area = MappingArea::clone_from(area);
            this.map_area(my_area);

            // Copy datas through high half address
            for src_page in area.range.iter() {
                let src_addr = them
                    .page_table
                    .as_high_half(src_page.start_addr::<VirtualAddress>())
                    .expect("Virtual address is not mapped")
                    .1;

                let dst_addr = this
                    .page_table
                    .as_high_half(src_page.start_addr::<VirtualAddress>())
                    .expect("Virtual address is not mapped")
                    .1;

                let src_slice =
                    unsafe { slice::from_raw_parts(src_addr.as_ptr::<u8>(), constants::PAGE_SIZE) };
                let dst_slice = unsafe {
                    slice::from_raw_parts_mut(dst_addr.as_mut_ptr::<u8>(), constants::PAGE_SIZE)
                };

                // Can not use _src_guard::copy_from_slice because the slice is in their own memory space.
                // We use high half address(mapped by the frame allocator) to access the slice
                // The `translate` method returns the high level address
                dst_slice.copy_from_slice(src_slice);
            }
        }

        this
    }

    // Map the whole kernel area to the memory space
    // See virtual memory layout in `main.rs` of the kernel for more details
    pub fn register_kernel_area(&mut self) {
        let table_va = self
            .page_table
            .root_ppn()
            .start_addr::<PhysicalAddress>()
            .to_high_virtual();
        let p_table = unsafe { &mut *table_va.as_mut_ptr::<[usize; 512]>() };

        // layout
        // root[0x100] = (0x00000 << 10) | 0xcf;
        // root[0x101] = (0x40000 << 10) | 0xcf;
        // root[0x102] = (0x80000 << 10) | 0xcf;
        // No `User` flag so that only kernel can access these pages

        // PageTableEntryFlags's BitOr operation functions triggers fetch instruction page fault
        // So we uses bare instructions
        p_table[0x100] = 0xcf;
        p_table[0x101] = (0x40000 << 10) | 0xcf;
        p_table[0x102] = (0x80000 << 10) | 0xcf;

        debug!("Kernel area registered for {:}", self.page_table.root_ppn());
    }
}

// A data structure to build a memory space that is used to create a new process
pub struct MemorySpaceBuilder {
    pub memory_space: MemorySpace,
    pub entry_pc: VirtualAddress,
    pub stack_top: VirtualAddress,
    pub argc: usize,
    pub argv_base: VirtualAddress,
    pub envp_base: VirtualAddress,
    pub auxv: Vec<AuxVecEntry>,
}

// Fix that `TaskControlBlock::from(memory_space_builder)` complains `Arc<MemorySpaceBuilder>` is not `Send` and `Sync`
unsafe impl Sync for MemorySpaceBuilder {}
unsafe impl Send for MemorySpaceBuilder {}

impl MemorySpaceBuilder {
    pub fn from_elf(elf_data: &[u8]) -> Result<Self, &'static str> {
        let current_page_table = PageTable::borrow_current();
        let mut memory_space = MemorySpace::empty();
        memory_space.register_kernel_area();

        let elf_info = ElfFile::new(elf_data)?;

        // No need to check the ELF magic number because it is already checked in `ElfFile::new`
        // let elf_magic = elf_header.pt1.magic;
        // '\x7fELF' in ASCII
        // const ELF_MAGIC: [u8; 4] = [0x7f, 0x45, 0x4c, 0x46];

        let mut min_start_vpn = VirtualPageNum::from_usize(usize::MAX);
        let mut max_end_vpn = VirtualPageNum::from_usize(0);

        let mut auxv = Vec::new();

        let mut p_head = VirtualAddress::from_usize(0);

        for ph in elf_info
            .program_iter()
            // Only loadable segments are considered
            .filter(|p| p.get_type() == Ok(xmas_elf::program::Type::Load))
        {
            debug!("loading ph: {:?}", ph);

            let start = VirtualAddress::from_usize(ph.virtual_addr() as usize);
            let end = start + ph.mem_size() as usize;

            if p_head == VirtualAddress::from_usize(0) {
                p_head = start;
            }

            min_start_vpn = min_start_vpn.min(start.to_floor_page_num());
            max_end_vpn = max_end_vpn.max(end.to_floor_page_num());

            let mut segment_permissions = PageTableEntryFlags::Valid | PageTableEntryFlags::User;

            if ph.flags().is_read() {
                segment_permissions |= PageTableEntryFlags::Readable;
            }

            if ph.flags().is_write() {
                segment_permissions |= PageTableEntryFlags::Writable;
            }

            if ph.flags().is_execute() {
                segment_permissions |= PageTableEntryFlags::Executable;
            }

            let page_range = VirtualPageNumRange::from_start_end(
                start.to_floor_page_num(),
                end.to_ceil_page_num(), // end is exclusive
            );

            memory_space.map_area(MappingArea::new(
                page_range,
                AreaType::UserElf,
                MapType::Framed,
                segment_permissions,
            ));

            let data = &elf_data[ph.offset() as usize..(ph.offset() + ph.file_size()) as usize];

            let copied = current_page_table.activated_copy_data_to_other(
                &memory_space.page_table,
                start,
                data,
            );

            debug_assert!(copied == data.len());
        }

        debug_assert!(min_start_vpn > VirtualPageNum::from_usize(0));

        memory_space.elf_area = VirtualAddressRange::from_start_end(
            min_start_vpn.start_addr::<VirtualAddress>(),
            max_end_vpn.start_addr::<VirtualAddress>(),
        );

        log::debug!("Elf segments loaded, max_end_vpn: {:?}", max_end_vpn);

        let p_phhead = p_head + elf_info.header.pt2.ph_offset() as usize;

        auxv.push(AuxVecEntry::new(AT_PHDR, p_phhead.as_usize()));
        auxv.push(AuxVecEntry::new(
            AT_PHENT,
            elf_info.header.pt2.ph_entry_size() as usize,
        ));
        auxv.push(AuxVecEntry::new(
            AT_PHNUM,
            elf_info.header.pt2.ph_count() as usize,
        ));
        auxv.push(AuxVecEntry::new(AT_PAGESZ, constants::PAGE_SIZE));
        auxv.push(AuxVecEntry::new(AT_BASE, 0));
        auxv.push(AuxVecEntry::new(AT_FLAGS, 0));
        auxv.push(AuxVecEntry::new(
            AT_ENTRY,
            elf_info.header.pt2.entry_point() as usize,
        ));
        auxv.push(AuxVecEntry::new(AT_UID, 0));
        auxv.push(AuxVecEntry::new(AT_EUID, 0));
        auxv.push(AuxVecEntry::new(AT_GID, 0));
        auxv.push(AuxVecEntry::new(AT_EGID, 0));
        auxv.push(AuxVecEntry::new(AT_HWCAP, 0));
        // FIXME: Decouple the IMachine to separate crate and load the machine specific values
        auxv.push(AuxVecEntry::new(AT_CLKTCK, 125000000usize));
        auxv.push(AuxVecEntry::new(AT_SECURE, 0));

        max_end_vpn += 1;
        debug!("Stack guard base: {:?}", max_end_vpn);
        memory_space.map_area(MappingArea::new(
            VirtualPageNumRange::from_single(max_end_vpn),
            AreaType::UserStackGuardBase,
            MapType::Framed,
            PageTableEntryFlags::empty(),
        ));
        memory_space.stack_guard_base = VirtualAddressRange::from_start_len(
            max_end_vpn.start_addr::<VirtualAddress>(),
            constants::USER_STACK_SIZE,
        );

        let stack_page_count = constants::USER_STACK_SIZE / constants::PAGE_SIZE;
        max_end_vpn += 1;
        debug!(
            "Stack: {:?}..{:?}",
            max_end_vpn,
            max_end_vpn + stack_page_count
        );
        memory_space.map_area(MappingArea::new(
            VirtualPageNumRange::from_start_count(max_end_vpn, stack_page_count),
            AreaType::UserStack,
            MapType::Framed,
            PageTableEntryFlags::Valid
                | PageTableEntryFlags::Writable
                | PageTableEntryFlags::Readable
                | PageTableEntryFlags::User,
        ));
        memory_space.stack_range = VirtualAddressRange::from_start_len(
            max_end_vpn.start_addr::<VirtualAddress>(),
            constants::USER_STACK_SIZE,
        );

        max_end_vpn += stack_page_count;
        let stack_top = max_end_vpn.start_addr::<VirtualAddress>();
        debug!("Stack guard top: {:?}", max_end_vpn);
        memory_space.map_area(MappingArea::new(
            VirtualPageNumRange::from_single(max_end_vpn),
            AreaType::UserStackGuardTop,
            MapType::Framed,
            PageTableEntryFlags::empty(),
        ));
        memory_space.stack_gurad_top = VirtualAddressRange::from_start_len(
            max_end_vpn.start_addr::<VirtualAddress>(),
            constants::PAGE_SIZE,
        );

        max_end_vpn += 1;
        debug!("Brk at: {:?}", max_end_vpn);
        memory_space.map_area(MappingArea::new(
            VirtualPageNumRange::from_start_count(max_end_vpn, 0),
            AreaType::UserBrk,
            MapType::Framed,
            PageTableEntryFlags::Valid
                | PageTableEntryFlags::Writable
                | PageTableEntryFlags::Readable
                | PageTableEntryFlags::User,
        ));
        memory_space.brk_area_idx = memory_space
            .mapping_areas
            .iter()
            .enumerate()
            .find(|(_, area)| area.area_type == AreaType::UserBrk)
            .expect("UserBrk area not found")
            .0;
        memory_space.brk_start = max_end_vpn.start_addr::<VirtualAddress>();

        let entry_pc = VirtualAddress::from_usize(elf_info.header.pt2.entry_point() as usize);

        Ok(MemorySpaceBuilder {
            memory_space,
            entry_pc,
            stack_top,
            argc: 0,
            argv_base: stack_top,
            envp_base: stack_top,
            auxv,
        })
    }

    fn push<T>(&mut self, value: T) {
        let kernel_pt = page_table::get_kernel_page_table();

        self.stack_top -= core::mem::size_of::<T>();
        self.stack_top = self.stack_top.align_down(core::mem::align_of::<T>());

        let pt = self.memory_space.page_table_mut();

        kernel_pt.activated_copy_val_to_other(self.stack_top, pt, &value);
    }

    pub fn init_stack(&mut self, args: &[&str], envp: &[&str]) {
        let mut envps = Vec::new(); // envp pointers

        // Step1: Copy envp strings vector to the stack
        for env in envp.iter().rev() {
            self.push(0u8); // NULL-terminated
            for byte in env.bytes().rev() {
                self.push(byte);
            }
            envps.push(self.stack_top);
        }

        let mut argvs = Vec::new(); // argv pointers

        // Step2: Copy args strings vector to the stack
        for arg in args.iter().rev() {
            self.push(0u8); // NULL-terminated
            for byte in arg.bytes().rev() {
                self.push(byte);
            }
            argvs.push(self.stack_top);
        }

        // align stack top down to 8 bytes
        self.stack_top = self.stack_top.align_down(8);
        debug_assert!(self.stack_top.as_usize() % 8 == 0);

        // Step3: Copy PLATFORM string to the stack
        const PLATFORM: &str = "RISC-V64\0";
        const PLATFORM_LEN: usize = PLATFORM.len();

        // Ensure that start address of copied PLATFORM is aligned to 8 bytes
        self.stack_top -= PLATFORM_LEN;
        self.stack_top = self.stack_top.align_down(8);
        debug_assert!(self.stack_top.as_usize() % 8 == 0);
        self.stack_top += PLATFORM_LEN;

        for byte in PLATFORM.bytes().rev() {
            self.push(byte);
        }

        // Step4: Setup 16 random bytes for aux vector
        self.push(0xdeadbeefu64);
        let aux_random_base = self.stack_top;

        // align down to 16 bytes
        self.stack_top = self.stack_top.align_down(16);
        debug_assert!(self.stack_top.as_usize() % 16 == 0);

        // Step5: setup aux vector
        self.push(AuxVecEntry::new(AT_NULL, 0));

        self.push(aux_random_base);
        self.push(AT_RANDOM);

        // Move auxv out of self
        let auxv = core::mem::take(&mut self.auxv);

        // Push other auxv entries
        for aux in auxv.iter().rev() {
            self.push(aux.value);
            self.push(aux.key.0);
        }

        // Step6: setup envp vector

        // push NULL for envp
        self.push(0usize);

        // push envp, envps is already in reverse order
        for env in envps.iter() {
            self.push(*env);
        }

        let envp_base = self.stack_top;

        // Step7: setup argv vector

        // push NULL for args
        self.push(0usize);

        // push args, argvs is already in reverse order
        for arg in argvs.iter() {
            self.push(*arg);
        }

        let argv_base = self.stack_top;

        // Step8: setup argc

        // push argc
        let argc = args.len();
        self.push(argc);

        // let argc_base = self.stack_top;

        self.argc = argc;
        self.argv_base = argv_base;
        self.envp_base = envp_base;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AuxVecKey(pub usize);

pub const AT_NULL: AuxVecKey = AuxVecKey(0); // end of vector
pub const AT_IGNORE: AuxVecKey = AuxVecKey(1); // entry should be ignored
pub const AT_EXECFD: AuxVecKey = AuxVecKey(2); // file descriptor of program
pub const AT_NOTELF: AuxVecKey = AuxVecKey(10); // program is not ELF
pub const AT_PLATFORM: AuxVecKey = AuxVecKey(15); // string identifying CPU for optimizations
pub const AT_BASE_PLATFORM: AuxVecKey = AuxVecKey(24); // string identifying real platform, may differ from AT_PLATFORM.
pub const AT_HWCAP2: AuxVecKey = AuxVecKey(26); // extension of AT_HWCAP
pub const AT_EXECFN: AuxVecKey = AuxVecKey(31); // filename of program
pub const AT_PHDR: AuxVecKey = AuxVecKey(3); // program headers for program
pub const AT_PHENT: AuxVecKey = AuxVecKey(4); // size of program header entry
pub const AT_PHNUM: AuxVecKey = AuxVecKey(5); // number of program headers
pub const AT_PAGESZ: AuxVecKey = AuxVecKey(6); // system page size
pub const AT_BASE: AuxVecKey = AuxVecKey(7); // base address of interpreter
pub const AT_FLAGS: AuxVecKey = AuxVecKey(8); // flags
pub const AT_ENTRY: AuxVecKey = AuxVecKey(9); // entry point of program
pub const AT_UID: AuxVecKey = AuxVecKey(11); // real uid
pub const AT_EUID: AuxVecKey = AuxVecKey(12); // effective uid
pub const AT_GID: AuxVecKey = AuxVecKey(13); // real gid
pub const AT_EGID: AuxVecKey = AuxVecKey(14); // effective gid
pub const AT_HWCAP: AuxVecKey = AuxVecKey(16); // arch dependent hints at CPU capabilities
pub const AT_CLKTCK: AuxVecKey = AuxVecKey(17); // frequency at which times() increments
pub const AT_SECURE: AuxVecKey = AuxVecKey(23); // secure mode boolean
pub const AT_RANDOM: AuxVecKey = AuxVecKey(25); // address of 16 random bytes

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AuxVecEntry {
    pub key: AuxVecKey,
    pub value: usize,
}

impl AuxVecEntry {
    pub const fn new(key: AuxVecKey, val: usize) -> Self {
        AuxVecEntry { key, value: val }
    }
}
