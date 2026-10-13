//! Serves this crate's test jobs as a program of its own, for the tests of
//! running jobs in a started program (`tests/process.rs`), which is how
//! Windows isolates work.  Slerp's own program is `slerp` itself.

static JOBS: &[slerp_isolate::Jobs] = &[slerp_isolate::testing::JOBS];

fn main() {
    slerp_isolate::serve_jobs(JOBS);
    eprintln!("isolate_child only runs slerp-isolate's test jobs (--isolated-job NAME)");
    std::process::exit(1);
}
