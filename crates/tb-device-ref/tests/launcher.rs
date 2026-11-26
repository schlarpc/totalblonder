//! The host launcher's fibers and shuffles (kernels/tb_kernels_ref.cc), tested directly with
//! its own test kernels: what `k_xf`, `k_rowstat` and `k_ent_rowsum` rely on.

use tb_device_ref::{LauncherTest, launcher_test_count, run_launcher_test, sanitized};

/// The feature `asan` links the library built with AddressSanitizer, and only it does.
#[test]
fn the_library_is_the_one_the_features_ask_for() {
    assert_eq!(sanitized(), cfg!(feature = "asan"));
}

#[test]
fn the_test_kernels_are_those_of_the_library() {
    assert_eq!(launcher_test_count(), LauncherTest::ALL.len());
}

#[test]
fn barriers_hold_every_thread_of_a_block() {
    for (grid, block) in [(1, 1), (1, 2), (3, 32), (2, 40), (2, 256), (1, 1024)] {
        let out = run_launcher_test(LauncherTest::Sync, grid, block, 0).unwrap();
        for b in 0..grid {
            for t in 0..block {
                assert_eq!(
                    out[(b * block + t) as usize],
                    ((b << 16) | t) + 1,
                    "grid {grid} block {block}: block {b} thread {t}"
                );
            }
        }
    }
}

#[test]
fn a_kernel_with_barriers_run_as_a_plain_loop_is_reported() {
    assert_eq!(
        run_launcher_test(LauncherTest::SyncUnflagged, 1, 64, 0),
        Err(-5)
    );
}

#[test]
fn shuffle_xor_reads_the_partner_lane() {
    for block in [32, 64, 96, 256] {
        for d in [1, 2, 4, 8, 16] {
            let out = run_launcher_test(LauncherTest::ShflXor, 2, block, d).unwrap();
            for b in 0..2u32 {
                for t in 0..block {
                    let p = t ^ d as u32;
                    assert_eq!(out[(b * block + t) as usize], (b << 16) | p, "d {d} t {t}");
                }
            }
        }
    }
}

#[test]
fn shuffle_xor_past_the_end_of_a_partial_warp_reads_zero() {
    // A block of 40 threads: lanes 32..40 of the second warp have no partner above 40. CUDA
    // leaves the value undefined; the launcher gives 0. No kernel relies on it.
    let out = run_launcher_test(LauncherTest::ShflXor, 1, 40, 16).unwrap();
    for t in 32..40u32 {
        assert_eq!(out[t as usize], 0, "lane {t}");
    }
    for t in 0..32u32 {
        assert_eq!(out[t as usize], t ^ 16);
    }
}

#[test]
fn shuffle_up_reads_the_lane_below_within_the_warp() {
    for block in [32, 64, 128] {
        for d in [1, 2, 4, 8, 16, 31] {
            let out = run_launcher_test(LauncherTest::ShflUp, 1, block, d).unwrap();
            for t in 0..block {
                let lane = t & 31;
                let want = if lane >= d as u32 { t - d as u32 } else { t };
                assert_eq!(out[t as usize], want, "block {block} d {d} t {t}");
            }
        }
    }
}

#[test]
fn block_scan_as_k_ent_rowsum_does_it() {
    for (grid, block, k) in [(1, 32, 1), (2, 64, 3), (1, 256, 7), (3, 1024, 2)] {
        let out = run_launcher_test(LauncherTest::Scan, grid, block, k).unwrap();
        for b in 0..grid {
            let mut acc = 0u32;
            for t in 0..block {
                acc += t * k as u32 + 1;
                assert_eq!(out[(b * block + t) as usize], acc, "block {block} t {t}");
            }
        }
    }
}

#[test]
fn threads_at_different_barriers_are_reported() {
    for test in [
        LauncherTest::DivergeKind,
        LauncherTest::DivergeCount,
        LauncherTest::DivergeDelta,
    ] {
        for block in [2, 32, 64] {
            assert_eq!(
                run_launcher_test(test, 2, block, 0),
                Err(-3),
                "{test:?} block {block}"
            );
        }
    }
}

#[test]
fn block_sizes_outside_the_range_are_refused() {
    assert_eq!(run_launcher_test(LauncherTest::Sync, 1, 0, 0), Err(-2));
    assert_eq!(run_launcher_test(LauncherTest::Sync, 1, 1025, 0), Err(-2));
}

#[test]
fn the_launcher_recovers_after_a_reported_failure() {
    assert_eq!(
        run_launcher_test(LauncherTest::DivergeCount, 1, 64, 0),
        Err(-3)
    );
    let out = run_launcher_test(LauncherTest::Sync, 2, 64, 0).unwrap();
    assert_eq!(out[64 + 5], ((1 << 16) | 5) + 1);
}
