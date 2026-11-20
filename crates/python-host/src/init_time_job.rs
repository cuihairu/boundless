//! `Script::InitTimeJob`/`FiniTimeJob` and the run-once drivers
//! (`lib/script/init_time_job.cpp`).
//!
//! The reference registers jobs from static constructors before script init
//! and drains them through `runInitTimeJobs()` inside `Script::init`
//! (rung ascending, insertion order within a rung, rung zero reserved for
//! module link initialisations) and `runFiniTimeJobs()` from `Script::fini`,
//! where the last rung carries `Personality::onFini`. `FiniTimeJob`'s
//! default rung is 1.
//!
//! A job constructed after its registry has run is a critical error in the
//! reference; here it reports [`JobError::AlreadyRan`]. A second run of
//! either driver is an error in both.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Why a job could not be registered or a registry could not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobError {
    /// The registry has already run.
    AlreadyRan,
}

/// The reference's `InitTimeJobsVector` of virtual `init()` calls.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// One registry, shared by the init and fini halves through a `OnceLock`.
struct Registry {
    rungs: BTreeMap<i32, Vec<Job>>,
    has_run: bool,
}

fn registry(cell: &'static OnceLock<Mutex<Registry>>) -> MutexGuard<'static, Registry> {
    cell.get_or_init(|| {
        Mutex::new(Registry {
            rungs: BTreeMap::new(),
            has_run: false,
        })
    })
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
}

static INIT_JOBS: OnceLock<Mutex<Registry>> = OnceLock::new();
static FINI_JOBS: OnceLock<Mutex<Registry>> = OnceLock::new();

/// `InitTimeJob( rung )`: a job for the next [`run_init_time_jobs`].
pub fn add_init_time_job(rung: i32, job: impl FnOnce() + Send + 'static) -> Result<(), JobError> {
    add_job(&INIT_JOBS, rung, job)
}

/// `runInitTimeJobs()`: every job in rung order, then the registry is spent.
pub fn run_init_time_jobs() -> Result<(), JobError> {
    run_jobs(&INIT_JOBS)
}

/// `FiniTimeJob( rung = 1 )`, the reference's default rung.
pub fn add_fini_time_job(job: impl FnOnce() + Send + 'static) -> Result<(), JobError> {
    add_fini_time_job_at(1, job)
}

/// `FiniTimeJob( rung )` with an explicit rung.
pub fn add_fini_time_job_at(
    rung: i32,
    job: impl FnOnce() + Send + 'static,
) -> Result<(), JobError> {
    add_job(&FINI_JOBS, rung, job)
}

/// `runFiniTimeJobs()`.
pub fn run_fini_time_jobs() -> Result<(), JobError> {
    run_jobs(&FINI_JOBS)
}

fn add_job(
    cell: &'static OnceLock<Mutex<Registry>>,
    rung: i32,
    job: impl FnOnce() + Send + 'static,
) -> Result<(), JobError> {
    let mut registry = registry(cell);
    if registry.has_run {
        return Err(JobError::AlreadyRan);
    }
    registry.rungs.entry(rung).or_default().push(Box::new(job));
    Ok(())
}

fn run_jobs(cell: &'static OnceLock<Mutex<Registry>>) -> Result<(), JobError> {
    let rungs = {
        let mut registry = registry(cell);
        if registry.has_run {
            return Err(JobError::AlreadyRan);
        }
        registry.has_run = true;
        // Taken before the guard drops, so a job that tries to register
        // another one reports `AlreadyRan` (the reference's critical error)
        // instead of deadlocking on this registry.
        std::mem::take(&mut registry.rungs)
    };
    for (_, jobs) in rungs {
        for job in jobs {
            job();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Jobs drain rung ascending, insertion order within a rung, exactly
    /// once; a second run and a late registration both report `AlreadyRan`.
    #[test]
    fn jobs_drain_in_rung_order_exactly_once() {
        static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
        let ticks = std::sync::Arc::new(Mutex::new(Vec::new()));
        for (rung, stamp) in [(5, 51), (1, 10), (5, 52)] {
            let ticks = ticks.clone();
            assert_eq!(
                add_job(&REGISTRY, rung, move || ticks.lock().unwrap().push(stamp)),
                Ok(())
            );
        }
        assert_eq!(run_jobs(&REGISTRY), Ok(()));
        assert_eq!(*ticks.lock().unwrap(), vec![10, 51, 52]);
        assert_eq!(run_jobs(&REGISTRY), Err(JobError::AlreadyRan));
        assert_eq!(add_job(&REGISTRY, 1, || {}), Err(JobError::AlreadyRan));
    }

    /// A job registered from inside the run finds the registry already
    /// spent and reports `AlreadyRan` -- the reference's critical error for
    /// a late `InitTimeJob`.
    #[test]
    fn a_job_added_from_inside_the_run_reports_already_ran() {
        static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
        let late = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let inside = late.clone();
        assert_eq!(
            add_job(&REGISTRY, 1, move || {
                inside.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
            Ok(())
        );
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let registrar = observed.clone();
        assert_eq!(
            add_job(&REGISTRY, 2, move || {
                registrar.store(
                    add_job(&REGISTRY, 3, || {}).is_err(),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }),
            Ok(())
        );
        assert_eq!(run_jobs(&REGISTRY), Ok(()));
        assert!(late.load(std::sync::atomic::Ordering::SeqCst));
        assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// A poisoned registry lock recovers for late callers: the reference
    /// treats these registries as process-wide, and one panicking job must
    /// not lock the rest of the script system out of them.
    #[test]
    fn a_poisoned_registry_lock_recovers() {
        static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
        let poisoner = std::thread::spawn(|| {
            let _guard = registry(&REGISTRY);
            panic!("poison the registry lock");
        });
        assert!(poisoner.join().is_err());
        assert_eq!(add_job(&REGISTRY, 0, || {}), Ok(()));
    }
}
