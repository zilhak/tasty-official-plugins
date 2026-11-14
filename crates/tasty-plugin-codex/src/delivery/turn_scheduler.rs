//! 턴 후보 재확인 깨우기를 OS 스레드 하나로 예약한다.
//!
//! 예약 요청은 채널로 보내고, 스케줄러 스레드가 가장 이른 시각 하나만 기억한다. 그 시각이
//! 되면 깨우기를 한 번 보내고 기억을 비운다. 더 늦은 예약은 이른 예약에 합쳐진다. 깨우기를
//! 받은 펌프가 남은 후보의 다음 시각을 다시 예약한다. 스케줄러 스레드는 프로세스에 하나이고
//! 예약 수와 관계없이 늘지 않는다. 깨우기는 공유 상태를 지우지 않으므로 옛 깨우기가 새 예약을
//! 취소하지 못한다. 원장 송신 재시도 타이머는 이 스케줄러를 쓰지 않는다.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::store;

/// 깨우기를 보내는 함수. 인자는 깨우기 순번이다(진단용).
pub(crate) type Fire = Box<dyn Fn(u64) + Send + 'static>;

#[derive(Default)]
pub(crate) struct TurnScheduler {
    tx: Mutex<Option<Sender<u64>>>,
    /// 지금까지 띄운 스케줄러 스레드 수와 살아 있는 수.
    started: AtomicUsize,
    alive: Arc<AtomicUsize>,
    poison_reported: AtomicBool,
}

impl TurnScheduler {
    /// `at_ms`(UNIX ms)에 깨우기를 예약한다. 스레드가 없으면 `fire`로 하나 띄운다.
    pub(crate) fn arm(&self, at_ms: u64, fire: impl FnOnce() -> Fire) -> Result<(), String> {
        let mut tx = tasty_utils::poison::recover_mutex(
            self.tx.lock(),
            "codex turn scheduler",
            &self.poison_reported,
        );
        if let Some(sender) = tx.as_ref()
            && sender.send(at_ms).is_ok()
        {
            return Ok(());
        }
        // 처음이거나 스레드가 끝났다.
        let (sender, receiver) = mpsc::channel::<u64>();
        let fire = fire();
        let alive = self.alive.clone();
        // 띄우기 전에 센다. 스레드가 끝날 때 뺀다.
        self.alive.fetch_add(1, Ordering::SeqCst);
        let spawned = std::thread::Builder::new()
            .name("codex-turn-scheduler".into())
            .spawn(move || {
                run(receiver, fire);
                alive.fetch_sub(1, Ordering::SeqCst);
            });
        if let Err(e) = spawned {
            self.alive.fetch_sub(1, Ordering::SeqCst);
            return Err(e.to_string());
        }
        self.started.fetch_add(1, Ordering::SeqCst);
        sender.send(at_ms).map_err(|e| e.to_string())?;
        *tx = Some(sender);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn alive(&self) -> usize {
        self.alive.load(Ordering::SeqCst)
    }
}

/// 가장 이른 예약 하나를 기다렸다가 깨우기를 보낸다. 모든 송신자가 사라지면 끝난다.
fn run(receiver: mpsc::Receiver<u64>, fire: Fire) {
    let mut next: Option<u64> = None;
    let mut seq = 0u64;
    loop {
        let received = match next {
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(at) => {
                let now = store::now_ms();
                if now >= at {
                    next = None;
                    seq += 1;
                    fire(seq);
                    continue;
                }
                receiver.recv_timeout(Duration::from_millis(at - now))
            }
        };
        match received {
            Ok(at) => next = Some(next.map_or(at, |n| n.min(at))),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder() -> (Arc<Mutex<Vec<u64>>>, impl FnOnce() -> Fire + Clone) {
        let fired = Arc::new(Mutex::new(Vec::new()));
        let f = fired.clone();
        let make = move || -> Fire {
            Box::new(move |_seq| {
                f.lock().unwrap().push(store::now_ms());
            })
        };
        (fired, make)
    }

    fn wait_until(pred: impl Fn() -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !pred() && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 긴 예약 뒤 더 이른 예약을 거듭해도 스레드는 하나이고, 늦은 예약은 이른 깨우기 하나로
    /// 합쳐진다. 깨운 뒤 다시 예약해도 같은 스레드를 쓴다.
    #[test]
    fn earlier_rearms_keep_one_thread_and_fire_once() {
        let s = TurnScheduler::default();
        let (fired, make) = recorder();
        let now = store::now_ms();
        s.arm(now + 60_000, make.clone()).unwrap();
        for k in 1..=6u64 {
            s.arm(now + 400 - k * 50, make.clone()).unwrap();
        }
        assert_eq!((s.started(), s.alive()), (1, 1));
        wait_until(|| !fired.lock().unwrap().is_empty());
        let first = fired.lock().unwrap()[0];
        assert!(
            first >= now + 100,
            "가장 이른 시각 전에 깨우지 않는다: {}",
            first - now
        );
        // 옛(늦은) 예약은 따로 깨우지 않는다.
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(fired.lock().unwrap().len(), 1);
        // 시간이 지난 뒤 새 예약도 같은 스레드가 처리한다.
        s.arm(store::now_ms() + 20, make.clone()).unwrap();
        wait_until(|| fired.lock().unwrap().len() == 2);
        assert_eq!(fired.lock().unwrap().len(), 2);
        assert_eq!((s.started(), s.alive()), (1, 1));
    }

    /// 이미 지난 시각의 예약은 바로 깨우고, 더 늦은 예약이 와도 이른 예약을 늦추지 않는다.
    #[test]
    fn a_later_arm_does_not_delay_an_earlier_one() {
        let s = TurnScheduler::default();
        let (fired, make) = recorder();
        let now = store::now_ms();
        s.arm(now + 30, make.clone()).unwrap();
        s.arm(now + 60_000, make.clone()).unwrap();
        wait_until(|| !fired.lock().unwrap().is_empty());
        assert!(fired.lock().unwrap()[0] < now + 30_000);
        s.arm(0, make.clone()).unwrap();
        wait_until(|| fired.lock().unwrap().len() == 2);
        assert_eq!(fired.lock().unwrap().len(), 2);
        assert_eq!(s.started(), 1);
    }

    /// 스케줄러가 사라지면 스레드도 끝난다.
    #[test]
    fn the_thread_ends_with_the_scheduler() {
        let s = TurnScheduler::default();
        let (_fired, make) = recorder();
        s.arm(store::now_ms() + 60_000, make).unwrap();
        let alive = s.alive.clone();
        wait_until(|| alive.load(Ordering::SeqCst) == 1);
        drop(s);
        wait_until(|| alive.load(Ordering::SeqCst) == 0);
        assert_eq!(alive.load(Ordering::SeqCst), 0);
    }
}
