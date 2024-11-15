use virtio_drivers::{device::blk::VirtIOBlk, transport::mmio::MmioTransport};

use crate::IDiskDevice;

pub const SECTOR_SIZE: usize = 512;

pub struct VirtioDisk<THal>
where
    THal: virtio_drivers::Hal,
{
    sector: usize,
    offset: usize,
    virtio_blk: VirtIOBlk<THal, MmioTransport>,
}

impl<T> VirtioDisk<T>
where
    T: virtio_drivers::Hal,
{
    pub fn new(virtio_blk: VirtIOBlk<T, MmioTransport>) -> Self {
        VirtioDisk {
            sector: 0,
            offset: 0,
            virtio_blk,
        }
    }
}

impl<T> IDiskDevice for VirtioDisk<T>
where
    T: virtio_drivers::Hal,
{
    fn read_blocks(&mut self, buf: &mut [u8]) {
        self.virtio_blk
            .read_blocks(self.sector, buf)
            .expect("Error occurred when reading VirtIOBlk");
    }

    fn write_blocks(&mut self, buf: &[u8]) {
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

    fn move_cursor(&mut self, amount: usize) {
        self.set_position(self.get_position() + amount)
    }
}
