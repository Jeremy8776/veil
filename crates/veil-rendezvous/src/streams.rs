//! Bounded, owned relay stream workers. Backpressure stays in QUIC; every worker has
//! a deadline, and dropping the receiver aborts all of its in-flight work.

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinSet;

pub(crate) const MAX_PACKET_WORKERS: usize = 16;
pub(crate) const PACKET_DEADLINE: Duration = Duration::from_secs(2);

pub(crate) async fn receive<F, Work>(conn: quinn::Connection, handle: F)
where
    F: Fn(quinn::RecvStream) -> Work,
    Work: Future<Output = ()> + Send + 'static,
{
    let mut workers = JoinSet::new();
    loop {
        tokio::select! {
            _ = conn.closed() => break,
            stream = conn.accept_uni(), if workers.len() < MAX_PACKET_WORKERS => {
                let Ok(stream) = stream else { break };
                let work = handle(stream);
                workers.spawn(async move {
                    let _ = tokio::time::timeout(PACKET_DEADLINE, work).await;
                });
            }
            _ = workers.join_next(), if !workers.is_empty() => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use veil_net::Node;
    use veil_security::Identity;

    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stalled_streams_are_bounded_expire_and_stop_with_the_receiver() {
        let identity = Identity::generate();
        let node = Node::bind(&identity, "127.0.0.1:0".parse().unwrap()).unwrap();
        let remote = Node::bind(&Identity::generate(), "127.0.0.1:0".parse().unwrap()).unwrap();
        let addresses = [node.local_addr().unwrap()];
        let outgoing = remote.connect(&addresses, identity.public());
        let (sent, accepted) = tokio::join!(outgoing, node.accept());
        let sent = sent.unwrap();
        let accepted = accepted.unwrap().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let (started, mut notices) = tokio::sync::mpsc::channel(64);
        let count = active.clone();
        let task = tokio::spawn(receive(accepted.quic().clone(), move |mut stream| {
            let (count, started) = (count.clone(), started.clone());
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                let _active = Active(count);
                started.send(()).await.unwrap();
                // A hostile sender supplies bytes but never finishes its stream.
                let _ = stream.read_to_end(1024).await;
            }
        }));
        let mut held = Vec::new();
        for _ in 0..MAX_PACKET_WORKERS + 1 {
            let mut stream = sent.quic().open_uni().await.unwrap();
            stream.write_all(b"partial").await.unwrap();
            held.push(stream);
        }
        for _ in 0..MAX_PACKET_WORKERS {
            tokio::time::timeout(Duration::from_secs(1), notices.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(active.load(Ordering::SeqCst), MAX_PACKET_WORKERS);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), notices.recv())
                .await
                .is_err()
        );
        // Deadline reclamation admits the queued stream without closing the link.
        tokio::time::timeout(PACKET_DEADLINE + Duration::from_secs(1), notices.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(active.load(Ordering::SeqCst) <= MAX_PACKET_WORKERS);
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborting the receiver must abort its children");
        drop(held);
    }
}
