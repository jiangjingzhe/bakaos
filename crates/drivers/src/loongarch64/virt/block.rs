use virtio_drivers::{device::blk::VirtIOBlk, transport::pci::PciTransport};

use crate::block::{IRawDiskDevice, SECTOR_SIZE};

pub struct VirtioDisk<THal>
where
    THal: virtio_drivers::Hal,
{
    sector: usize,
    offset: usize,
    virtio_blk: VirtIOBlk<THal, PciTransport>,
}

impl<T> VirtioDisk<T>
where
    T: virtio_drivers::Hal,
{
    pub fn new(virtio_blk: VirtIOBlk<T, PciTransport>) -> Self {
        VirtioDisk {
            sector: 0,
            offset: 0,
            virtio_blk,
        }
    }
}

impl<T> IRawDiskDevice for VirtioDisk<T>
where
    T: virtio_drivers::Hal,
{
    fn read_blocks(&mut self, buf: &mut [u8]) {
        if self.sector as u64 >= self.virtio_blk.capacity() {
            return;
        }

        self.virtio_blk
            .read_blocks(self.sector, buf)
            .expect("Error occurred when reading VirtIOBlk");
    }

    fn write_blocks(&mut self, buf: &[u8]) {
        if self.sector as u64 >= self.virtio_blk.capacity() {
            return;
        }

        self.virtio_blk
            .write_blocks(self.sector, buf)
            .expect("Error occurred when writing VirtIOBlk");
    }

    fn get_position(&self) -> usize {
        self.sector * SECTOR_SIZE + self.offset
    }

    fn set_position(&mut self, position: usize) {
        self.sector = position / SECTOR_SIZE;
        self.offset = position % SECTOR_SIZE;
    }
}
