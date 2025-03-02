#![feature(const_trait_impl)]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
extern crate std;

extern crate alloc;

mod address;
mod address_range;
mod page_num;
mod page_num_range;
mod physical_address;
mod physical_address_range;
mod physical_page_num;
mod physical_page_num_range;
mod virtual_address;
mod virtual_address_range;
mod virtual_page_num;
mod virtual_page_num_range;

pub use address::*;
pub use address_range::*;
pub use page_num::*;
pub use page_num_range::*;
pub use physical_address::*;
pub use physical_address_range::*;
pub use physical_page_num::*;
pub use physical_page_num_range::*;
pub use virtual_address::*;
pub use virtual_address_range::*;
pub use virtual_page_num::*;
pub use virtual_page_num_range::*;

pub const PAGE_SIZE_BITS: usize = 0xc;

pub trait IPhysicalAddress<T> {
    fn to_virthal(&self) -> T;
}
