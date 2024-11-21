use core::fmt::Display;

use abstractions::{IArithOps, IBitwiseOps, IUsizeAlias};

use crate::*;

pub trait IPageNumBase: IUsizeAlias + Copy + Clone + PartialEq + PartialOrd + Eq + Ord {}

pub trait IPageNum: IPageNumBase + IBitwiseOps + IArithOps + Display {
    fn step(&mut self) {
        self.step_by(1);
    }

    fn step_by(&mut self, offset: usize) {
        *self += offset;
    }

    fn step_back(&mut self) {
        self.step_back_by(1);
    }

    fn step_back_by(&mut self, offset: usize) {
        *self -= offset;
    }

    fn from_addr_floor<T: IAddress>(addr: T) -> Self {
        Self::from_usize(addr.align_down(constants::PAGE_SIZE).as_usize() / constants::PAGE_SIZE)
    }

    fn from_addr_ceil<T: IAddress>(addr: T) -> Self {
        Self::from_usize(addr.align_up(constants::PAGE_SIZE).as_usize() / constants::PAGE_SIZE)
    }

    fn start_addr<T: IAddress>(self) -> T {
        T::from_usize(self.as_usize() * constants::PAGE_SIZE)
    }

    fn end_addr<T: IAddress>(self) -> T {
        T::from_usize((self.as_usize() + 1) * constants::PAGE_SIZE)
    }

    fn at_offset_of_start<T: IAddress>(self, offset: usize) -> T {
        T::from_usize(self.as_usize() * constants::PAGE_SIZE + offset)
    }

    fn at_offset_of_end<T: IAlignableAddress>(self, offset: usize) -> T {
        T::from_usize((self.as_usize() + 1) * constants::PAGE_SIZE - offset)
    }

    fn start_offset_of_addr<T: IAddress>(self, addr: T) -> isize {
        addr.diff(self.start_addr())
    }

    fn end_offset_of_addr<T: IAddress>(self, addr: T) -> isize {
        addr.diff(self.end_addr())
    }

    fn diff_page_count(self, other: Self) -> isize {
        (self.as_usize() as i64 - other.as_usize() as i64) as isize
    }

    fn addr_range<T: IAddress>(self) -> AddressRange<T> {
        AddressRange::from_start_end(self.start_addr(), self.end_addr())
    }
}

#[macro_export]
macro_rules! impl_IPageNum {
    ($type:ty) => {
        impl abstractions::IUsizeAlias for $type {
            #[inline(always)]
            fn from_usize(value: usize) -> Self {
                Self(value)
            }

            #[inline(always)]
            fn as_usize(&self) -> usize {
                self.0
            }
        }

        impl IPageNumBase for $type {}

        abstractions::impl_arith_ops!($type);
        abstractions::impl_bitwise_ops!($type);

        impl IPageNum for $type {}

        abstractions::impl_usize_display!($type);
    };
}
