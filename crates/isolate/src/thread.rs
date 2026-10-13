//! Where no program serves a job and there is no fork (Windows test
//! programs, other programs using Slerp's crates): the work runs on a
//! thread of its own behind a panic guard.  A thread that overruns is
//! abandoned rather than killed, and a crash there ends the whole process;
//! this is the fallback, not the isolation.

use crate::{Job, Limits, NO_ANSWER, TOO_LARGE, guarded, say, too_slow};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

type Answer = Result<Vec<u8>, String>;

/// As large a stack as a child program's job thread gets.
const STACK: usize = 16 << 20;

fn wait(job: &Job, rx: &Receiver<Answer>, time: Duration, output: usize) -> Answer {
    match rx.recv_timeout(time) {
        Ok(Ok(bytes)) if bytes.len() > output => Err(say(job, TOO_LARGE)),
        Ok(answer) => answer,
        Err(RecvTimeoutError::Timeout) => Err(say(job, &too_slow(time))),
        Err(RecvTimeoutError::Disconnected) => Err(say(job, NO_ANSWER)),
    }
}

fn spawn(f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name("slerp-isolated".into())
        .stack_size(STACK)
        .spawn(f)
        .map(|_| ())
        .map_err(|e| format!("couldn't start a thread: {e}"))
}

pub fn run(job: &'static Job, input: &[u8], limits: Limits) -> Answer {
    let (tx, rx) = channel();
    let input = input.to_vec();
    spawn(move || {
        let _ = tx.send(guarded(job, || job.open(&input)).map(|(_, answer)| answer));
    })?;
    wait(job, &rx, limits.time, limits.output)
}

/// A worker thread with the state; the caller stops waiting for one that
/// overruns, and it ends when it next looks for a request.
pub struct Worker {
    job: &'static Job,
    requests: Option<Sender<Vec<u8>>>,
    answers: Receiver<Answer>,
    pub limits: Limits,
}

impl Worker {
    pub fn start(job: &'static Job, input: &[u8], limits: Limits) -> Result<(Worker, Vec<u8>), String> {
        let (req_tx, req_rx) = channel::<Vec<u8>>();
        let (ans_tx, ans_rx) = channel::<Answer>();
        let input = input.to_vec();
        spawn(move || {
            let mut session = match guarded(job, || job.open(&input)) {
                Ok((session, opening)) => {
                    let _ = ans_tx.send(Ok(opening));
                    match session {
                        Some(s) => s,
                        None => return,
                    }
                }
                Err(e) => {
                    let _ = ans_tx.send(Err(e));
                    return;
                }
            };
            drop(input);
            while let Ok(request) = req_rx.recv() {
                if ans_tx.send(guarded(job, || session(&request))).is_err() {
                    break;
                }
            }
        })?;
        let mut worker = Worker { job, requests: Some(req_tx), answers: ans_rx, limits };
        match wait(job, &worker.answers, limits.time, limits.output) {
            Ok(opening) => Ok((worker, opening)),
            Err(e) => {
                worker.requests = None;
                Err(e)
            }
        }
    }

    pub fn alive(&self) -> bool {
        self.requests.is_some()
    }

    pub fn ask(&mut self, request: &[u8], time: Duration) -> Answer {
        let Some(tx) = &self.requests else { return Err(say(self.job, "has stopped")) };
        if tx.send(request.to_vec()).is_err() {
            self.requests = None;
            return Err(say(self.job, NO_ANSWER));
        }
        let answer = wait(self.job, &self.answers, time, self.limits.output);
        let ended = [NO_ANSWER, TOO_LARGE, "took longer than"].map(|p| say(self.job, p));
        if matches!(&answer, Err(e) if ended.iter().any(|p| e.starts_with(p.as_str()))) {
            self.requests = None;
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use std::time::Instant;

    fn limits() -> Limits {
        Limits { time: Duration::from_secs(5), memory: 256 << 20, output: 1 << 20 }
    }

    #[test]
    fn results_errors_and_panics_come_back() {
        assert_eq!(run(&testing::ECHO, b"hello", limits()).unwrap(), b"hello");
        assert_eq!(run(&testing::FAIL, b"nope", limits()).unwrap_err(), "nope");
        let e = run(&testing::PANIC, b"boom", limits()).unwrap_err();
        assert!(e.contains("failed inside") && e.contains("boom"), "{e}");
    }

    #[test]
    fn a_hang_is_abandoned() {
        let started = Instant::now();
        let e = run(&testing::HANG, b"", Limits { time: Duration::from_millis(300), ..limits() }).unwrap_err();
        assert!(e.contains("took longer than"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_worker_keeps_its_state() {
        let (mut w, opening) = Worker::start(&testing::COUNTER, &[10], limits()).unwrap();
        assert_eq!(opening, b"open");
        assert_eq!(w.ask(b"a", limits().time).unwrap(), [11]);
        assert_eq!(w.ask(b"fail", limits().time).unwrap_err(), "no");
        assert!(w.alive());
        assert!(w.ask(b"hang", Duration::from_millis(200)).is_err());
        assert!(!w.alive());
    }
}
