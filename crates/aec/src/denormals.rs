// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Flush-to-zero scope for the AEC3 float math.
//!
//! [`FlushDenormals`] sets FTZ and DAZ in the calling thread's MXCSR and
//! restores the previous value on drop. On targets without MXCSR it does
//! nothing.
//!

#[cfg(any(target_arch = "x86_64", all(target_arch = "x86", target_feature = "sse")))]
mod imp {
    use core::arch::asm;
    use core::marker::PhantomData;

    const FTZ: u32 = 1 << 15;
    const DAZ: u32 = 1 << 6;

    pub struct FlushDenormals {
        previous: u32,
        _thread_bound: PhantomData<*const ()>,
    }

    impl FlushDenormals {
        pub fn new() -> Self {
            let mut previous: u32 = 0;
            // SAFETY: stmxcsr writes 4 bytes to `previous`; ldmxcsr loads a
            // value derived from the current MXCSR with two mode bits set.
            unsafe {
                asm!("stmxcsr [{}]", in(reg) &mut previous, options(nostack, preserves_flags));
                let flushed = previous | FTZ | DAZ;
                asm!("ldmxcsr [{}]", in(reg) &flushed, options(nostack, readonly));
            }
            Self { previous, _thread_bound: PhantomData }
        }
    }

    impl Drop for FlushDenormals {
        fn drop(&mut self) {
            // SAFETY: restores the value stmxcsr read in `new`.
            unsafe {
                asm!("ldmxcsr [{}]", in(reg) &self.previous, options(nostack, readonly));
            }
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", all(target_arch = "x86", target_feature = "sse"))))]
mod imp {
    use core::marker::PhantomData;

    pub struct FlushDenormals {
        _thread_bound: PhantomData<*const ()>,
    }

    impl FlushDenormals {
        pub fn new() -> Self {
            Self { _thread_bound: PhantomData }
        }
    }
}

pub use imp::FlushDenormals;

impl Default for FlushDenormals {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, any(target_arch = "x86_64", all(target_arch = "x86", target_feature = "sse"))))]
mod tests {
    use super::*;

    fn mxcsr() -> u32 {
        let mut v: u32 = 0;
        unsafe { core::arch::asm!("stmxcsr [{}]", in(reg) &mut v, options(nostack, preserves_flags)) };
        v
    }

    #[test]
    fn sets_ftz_daz_and_restores() {
        let before = mxcsr();
        {
            let _g = FlushDenormals::new();
            assert_eq!(mxcsr() & ((1 << 15) | (1 << 6)), (1 << 15) | (1 << 6));
            let _nested = FlushDenormals::new();
        }
        assert_eq!(mxcsr(), before);
    }
}
