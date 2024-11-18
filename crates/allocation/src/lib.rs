#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
extern crate std;

extern crate alloc;

pub mod frame;

pub use frame::{alloc_frames, alloc_frame};
use log::debug;

pub fn init(memory_end: usize) {
    debug!("Initializing frame allocator with memory end at {:#018x}", memory_end);
    frame::init_frame_allocator(memory_end);
}
