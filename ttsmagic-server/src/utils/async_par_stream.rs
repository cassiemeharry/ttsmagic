use async_std::{
    pin::Pin,
    prelude::*,
    task::{spawn, Context, JoinHandle, Poll},
};
use futures::future::BoxFuture;
use std::collections::VecDeque;

struct ParStreamItem<T> {
    id: usize,
    future: JoinHandle<T>,
}

pub struct AsyncParallelStream<T, I: Iterator<Item = BoxFuture<'static, T>> + Unpin> {
    parallelism: usize,
    next_id: usize,
    tasks: VecDeque<ParStreamItem<T>>,
    values: VecDeque<T>,
    waiting: std::iter::Peekable<I>,
}

impl<T, I> AsyncParallelStream<T, I>
where
    T: Send + Sync + 'static,
    I: Iterator<Item = BoxFuture<'static, T>> + Unpin,
{
    pub fn new(
        parallelism: usize,
        futures: impl IntoIterator<Item = BoxFuture<'static, T>, IntoIter = I>,
    ) -> Self {
        let mut par_stream = Self {
            parallelism,
            next_id: 0,
            tasks: VecDeque::with_capacity(parallelism),
            values: VecDeque::with_capacity(parallelism),
            waiting: futures.into_iter().peekable(),
        };
        par_stream.spawn_tasks();
        par_stream
    }

    fn spawn_tasks(&mut self) {
        let values_count = self.values.len();
        while (self.tasks.len() + values_count) < self.parallelism {
            let next = match self.waiting.next() {
                Some(n) => n,
                None => break,
            };
            let id = self.next_id;
            self.next_id += 1;
            let future = spawn(async move {
                trace!("AsyncParallelStream: starting future #{}", id);
                let value = next.await;
                trace!("AsyncParallelStream: Future #{} finished", id);
                value
            });
            trace!("Enqueuing future #{} in AsyncParallelStream", id);
            let item = ParStreamItem { id, future };
            self.tasks.push_back(item);
        }
    }
}

impl<T, I> Stream for AsyncParallelStream<T, I>
where
    T: Send + Sync + Unpin + 'static,
    I: Iterator<Item = BoxFuture<'static, T>> + Unpin,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // We copy the waker and poll *every* task we have running, collecting
        // the ready results into another collection to pop off the end. We
        // ensure `self.tasks.len() + self.values.len() <= parallelism` to
        // conserve memory.
        let this: &mut Self = Pin::into_inner(self);

        this.spawn_tasks();
        if this.tasks.is_empty() && this.values.is_empty() {
            assert!(this.waiting.peek().is_none());
            return Poll::Ready(None);
        }

        let task_count = this.tasks.len();
        let mut i = 0;
        while i < task_count {
            i += 1;
            let mut item = this.tasks.pop_front().unwrap();
            let pinned = Pin::new(&mut item.future);
            match pinned.poll(cx) {
                Poll::Ready(value) => {
                    trace!("Got value from future #{}!", item.id);
                    this.values.push_back(value);
                }
                Poll::Pending => {
                    this.tasks.push_back(item);
                }
            }
        }

        let value_opt = this.values.pop_front();
        this.spawn_tasks();
        match value_opt {
            Some(value) => Poll::Ready(Some(value)),
            None => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use async_std::{
        future,
        prelude::*,
        task::{block_on, sleep},
    };
    use futures::future::BoxFuture;
    use std::time::Duration;

    use super::AsyncParallelStream;
    use crate::test_helpers::init_logging;

    type F = BoxFuture<'static, Result<usize, ()>>;

    fn make_futures() -> (F, F, F, F) {
        (
            Box::pin(future::ready(Ok(0))),
            Box::pin(async {
                sleep(Duration::from_millis(100)).await;
                Ok(1)
            }),
            Box::pin(async {
                sleep(Duration::from_millis(200)).await;
                Ok(2)
            }),
            Box::pin(async {
                future::timeout(Duration::from_millis(400), future::pending())
                    .await
                    .map_err(|_| ())
            }),
        )
    }

    #[test]
    fn test_ordering() {
        use itertools::Itertools;

        init_logging();

        let expected = vec![Ok(0), Ok(1), Ok(2), Err(())];
        // Check every permutation to ensure there's no ordering bias
        let permutations = vec![0, 1, 2, 3]
            .into_iter()
            .permutations(4)
            .collect::<Vec<Vec<usize>>>();
        let count = permutations.len();
        for (i, positions) in permutations.into_iter().enumerate() {
            let pos_a = positions[0];
            let pos_b = positions[1];
            let pos_c = positions[2];
            let pos_d = positions[3];
            println!(
                "Checking permutation #{}/{} ({}, {}, {}, {})",
                i + 1,
                count,
                pos_a,
                pos_b,
                pos_c,
                pos_d
            );
            let (a, b, c, d) = make_futures();
            let mut slots = [None, None, None, None];
            slots[pos_a] = Some(a);
            slots[pos_b] = Some(b);
            slots[pos_c] = Some(c);
            slots[pos_d] = Some(d);
            let [slot_1, slot_2, slot_3, slot_4] = slots;
            let futures = vec![
                slot_1.unwrap(),
                slot_2.unwrap(),
                slot_3.unwrap(),
                slot_4.unwrap(),
            ];
            let stream = AsyncParallelStream::new(4, futures);
            let actual = block_on(async move { stream.collect::<Vec<_>>().await });
            assert_eq!(actual, expected)
        }
    }
}
