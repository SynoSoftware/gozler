//! What the benches measure. On Windows it is the CPU cycles of the bench
//! thread, because the machine they run on is rarely idle and other work
//! moves wall-clock timings. Elsewhere it is wall time.

use criterion::Criterion;

#[cfg(windows)]
pub(crate) type Measure = cycles::Cycles;
#[cfg(not(windows))]
pub(crate) type Measure = criterion::measurement::WallTime;

pub(crate) fn criterion() -> Criterion<Measure> {
    #[cfg(windows)]
    let measure = cycles::Cycles;
    #[cfg(not(windows))]
    let measure = criterion::measurement::WallTime;
    Criterion::default().with_measurement(measure)
}

/// What [`Measure`] counts so far: the CPU cycles of this thread. The
/// difference of two calls is a timing in the bench's unit.
#[cfg(windows)]
// The compact bench shares this module and reads neither function.
#[allow(dead_code)]
pub(crate) fn thread_cycles() -> u64 {
    gozler_winfs::thread_cycles()
}

/// What [`Measure`] counts so far: the nanoseconds since the first call.
#[cfg(not(windows))]
#[allow(dead_code)]
pub(crate) fn thread_cycles() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// The process's private bytes, its commit charge.
#[cfg(windows)]
#[allow(dead_code)]
pub(crate) fn private_bytes() -> u64 {
    gozler_winfs::self_memory()
        .expect("a process reads its own counters")
        .private_bytes
}

/// 0, because only the Windows benches report memory.
#[cfg(not(windows))]
#[allow(dead_code)]
pub(crate) fn private_bytes() -> u64 {
    0
}

#[cfg(windows)]
mod cycles {
    use criterion::Throughput;
    use criterion::measurement::{Measurement, ValueFormatter};

    /// CPU cycles of the bench thread ([`gozler_winfs::thread_cycles`]).
    pub(crate) struct Cycles;

    impl Measurement for Cycles {
        type Intermediate = u64;
        type Value = u64;
        fn start(&self) -> u64 {
            gozler_winfs::thread_cycles()
        }
        fn end(&self, start: u64) -> u64 {
            gozler_winfs::thread_cycles() - start
        }
        fn add(&self, a: &u64, b: &u64) -> u64 {
            a + b
        }
        fn zero(&self) -> u64 {
            0
        }
        fn to_f64(&self, cycles: &u64) -> f64 {
            *cycles as f64
        }
        fn formatter(&self) -> &dyn ValueFormatter {
            &Megacycles
        }
    }

    struct Megacycles;

    impl ValueFormatter for Megacycles {
        fn scale_values(&self, _typical: f64, values: &mut [f64]) -> &'static str {
            values.iter_mut().for_each(|v| *v /= 1e6);
            "Mcycles"
        }
        fn scale_throughputs(&self, _: f64, _: &Throughput, values: &mut [f64]) -> &'static str {
            values.iter_mut().for_each(|v| *v /= 1e6);
            "Mcycles"
        }
        fn scale_for_machines(&self, _: &mut [f64]) -> &'static str {
            "cycles"
        }
    }
}
