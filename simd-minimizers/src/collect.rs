//! Collect (and dedup) SIMD-iterator values into a flat `Vec<u32>`.
use std::{
    array::{self, from_fn},
    cell::RefCell,
    mem::transmute,
};

use crate::S;
use wide::u32x8;

use crate::intrinsics::transpose;

/// Convenience wrapper around `collect_into`.
pub fn collect(
    (par_head, tail): (
        impl ExactSizeIterator<Item = S>,
        impl ExactSizeIterator<Item = u32>,
    ),
) -> Vec<u32> {
    let mut v = vec![];
    collect_into((par_head, tail), &mut v);
    v
}

/// Collect a SIMD-iterator into a single flat vector.
/// Works by taking 8 elements from each stream, and transposing this SIMD-matrix before writing out the results.
/// The `tail` is appended at the end.
#[inline(always)]
pub fn collect_into(
    (par_head, tail): (
        impl ExactSizeIterator<Item = S>,
        impl ExactSizeIterator<Item = u32>,
    ),
    out_vec: &mut Vec<u32>,
) {
    let len = par_head.len();
    out_vec.resize(len * 8 + tail.len(), 0);

    let mut m = [unsafe { transmute([0; 8]) }; 8];
    let mut i = 0;
    par_head.for_each(|x| {
        m[i % 8] = x;
        if i % 8 == 7 {
            let t = transpose(m);
            for j in 0..8 {
                unsafe {
                    *out_vec
                        .get_unchecked_mut(j * len + 8 * (i / 8)..)
                        .split_first_chunk_mut::<8>()
                        .unwrap()
                        .0 = transmute(t[j]);
                }
            }
        }
        i += 1;
    });

    // Manually write the unfinished parts of length k=i%8.
    let t = transpose(m);
    let k = i % 8;
    for j in 0..8 {
        unsafe {
            out_vec[j * len + 8 * (i / 8)..j * len + 8 * (i / 8) + k]
                .copy_from_slice(&transmute::<_, [u32; 8]>(t[j])[..k]);
        }
    }

    // Manually write the explicit tail.
    for (i, x) in tail.enumerate() {
        out_vec[8 * len + i] = x;
    }
}

thread_local! {
    static CACHE: RefCell<[Vec<u32>; 8]> = RefCell::new(array::from_fn(|_| Vec::new()));
}

/// Convenience wrapper around `collect_and_dedup_into`.
pub fn collect_and_dedup<const SUPER: bool>(
    (par_head, tail): (
        impl ExactSizeIterator<Item = S>,
        impl ExactSizeIterator<Item = u32>,
    ),
) -> Vec<u32> {
    let mut v = vec![];
    collect_and_dedup_into::<SUPER>((par_head, tail), &mut v);
    v
}

/// Collect a SIMD-iterator into a single vector, and duplicate adjacent equal elements.
/// Works by taking 8 elements from each stream, and then transposing the SIMD-matrix before writing out the results.
///
/// By default (when `SUPER` is false), the output is simply the deduplicated input values.
/// When `SUPER` is true, each returned `u32` is a tuple of `(u16,16)` where the low bits are those of the input value,
/// and the high bits are the index of the stream it first appeared, i.e., the start of its super-k-mer.
/// These positions are mod 2^16. When the window length is <2^16, this is sufficient to recover full super-k-mers.
#[inline(always)]
pub fn collect_and_dedup_into<const SUPER: bool>(
    (par_head, tail): (
        impl ExactSizeIterator<Item = S>,
        impl ExactSizeIterator<Item = u32>,
    ),
    out_vec: &mut Vec<u32>,
) {
    CACHE.with(|v| {
        let mut v = v.borrow_mut();

        let mut write_idx = [0; 8];
        // Vec of last pushed elements in each lane.
        let mut old = [unsafe { transmute([u32::MAX; 8]) }; 8];

        let len = par_head.len();
        let lane_offsets: [u32x8; 8] = from_fn(|i| u32x8::splat(((i * len) << 16) as u32));
        let offsets: [u32; 8] = from_fn(|i| (i << 16) as u32);
        let mut offsets: u32x8 = unsafe { transmute(offsets) };

        let mut m = [u32x8::ZERO; 8];
        let mut i = 0;
        par_head.for_each(|x| {
            m[i % 8] = x;
            if i % 8 == 7 {
                let t = transpose(m);
                offsets += u32x8::splat(8 << 16);
                for j in 0..8 {
                    let lane = t[j];
                    let vals = if SUPER {
                        // High 16 bits are the index where the minimizer first becomes minimal.
                        // Low 16 bits are the position of the minimizer itself.
                        (offsets + lane_offsets[j]) | (lane & u32x8::splat(0xFFFF))
                    } else {
                        lane
                    };
                    if write_idx[j] + 8 > v[j].len() {
                        let new_len = v[j].len() + 1024;
                        v[j].resize(new_len, 0);
                    }
                    unsafe {
                        crate::intrinsics::append_unique_vals(
                            old[j],
                            transmute(lane),
                            transmute(vals),
                            &mut v[j],
                            &mut write_idx[j],
                        );
                        old[j] = transmute(lane);
                    }
                }
            }
            i += 1;
        });

        for j in 0..8 {
            v[j].truncate(write_idx[j]);
        }

        // Manually write the unfinished parts of length k=i%8.
        let t = transpose(m);
        let k = i % 8;
        for j in 0..8 {
            let lane = &unsafe { transmute::<_, [u32; 8]>(t[j]) }[..k];
            for x in lane {
                if v[j].last() != Some(x) {
                    v[j].push(*x);
                }
            }
        }

        // Flatten v.
        for lane in v.iter() {
            let mut lane = lane.as_slice();
            while !lane.is_empty() && Some(lane[0]) == out_vec.last().copied() {
                lane = &lane[1..];
            }
            out_vec.extend_from_slice(lane);
        }

        // Manually write the dedup'ed explicit tail.
        for x in tail {
            if out_vec.last() != Some(&x) {
                out_vec.push(x);
            }
        }

        // v_flat
    })
}
