use async_std::{
    pin::Pin,
    prelude::*,
    task::{Context, Poll},
};
use futures::{future::BoxFuture, sink::Sink};
use std::collections::VecDeque;

pub struct AsyncStdStreamWrapper<S> {
    stream: S,
    terminated: bool,
}

impl<S> AsyncStdStreamWrapper<S> {
    pub fn new(stream: S) -> Self {
        AsyncStdStreamWrapper {
            stream,
            terminated: false,
        }
    }

    pub fn next(&mut self) -> AsyncStdStreamWrapperFuture<S::Item>
    where
        S: Stream + Send + Unpin,
    {
        let future = Box::pin(self.stream.next());
        AsyncStdStreamWrapperFuture {
            terminated: &mut self.terminated,
            future,
        }
    }
}

impl<S: Stream + Unpin> Stream for AsyncStdStreamWrapper<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut Pin::get_mut(self).stream).poll_next(cx)
    }
}

impl<Item, S: Sink<Item> + Unpin> Sink<Item> for AsyncStdStreamWrapper<S> {
    type Error = S::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), S::Error>> {
        Pin::new(&mut Pin::get_mut(self).stream).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Item) -> Result<(), S::Error> {
        Pin::new(&mut Pin::get_mut(self).stream).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), S::Error>> {
        Pin::new(&mut Pin::get_mut(self).stream).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), S::Error>> {
        Pin::new(&mut Pin::get_mut(self).stream).poll_close(cx)
    }
}

pub struct AsyncStdStreamWrapperFuture<'a, T> {
    terminated: &'a mut bool,
    future: BoxFuture<'a, Option<T>>,
}

impl<'a, T> async_std::future::Future for AsyncStdStreamWrapperFuture<'a, T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let self_: &mut Self = unsafe { self.get_unchecked_mut() };
        let future: Pin<&mut dyn Future<Output = _>> = Pin::as_mut(&mut self_.future);
        match future.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(v)) => Poll::Ready(Some(v)),
            Poll::Ready(None) => {
                *self_.terminated = true;
                Poll::Ready(None)
            }
        }
    }
}

impl<'a, T> futures::future::FusedFuture for AsyncStdStreamWrapperFuture<'a, T> {
    fn is_terminated(&self) -> bool {
        *self.terminated
    }
}

/// Adapts a Tokio-based future in a dedicated thread under the Tokio runtime.
/// This is rather inefficent for short futures, but might be ok for I/O
/// operations?
#[inline(never)]
pub async fn adapt_tokio_future<F>(future: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    async_std::task::spawn_blocking(move || {
        let mut runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
        debug!("Running Tokio future");
        runtime.block_on(future)
    })
    .await
}

pub struct AsyncParallelStream<T> {
    parallelism: usize,
    tasks: VecDeque<async_std::task::JoinHandle<T>>,
    waiting: VecDeque<BoxFuture<'static, T>>,
}

impl<T: Send + Sync + 'static> AsyncParallelStream<T> {
    pub fn new(
        parallelism: usize,
        futures: impl IntoIterator<Item = BoxFuture<'static, T>>,
    ) -> Self {
        let futures = futures.into_iter();
        let mut tasks = VecDeque::with_capacity(parallelism);
        let mut waiting =
            VecDeque::with_capacity(futures.size_hint().0.saturating_sub(parallelism));
        for future in futures {
            if tasks.len() < parallelism {
                tasks.push_back(async_std::task::spawn(future));
            } else {
                waiting.push_back(future);
            }
        }
        Self {
            parallelism,
            tasks,
            waiting,
        }
    }
}

impl<T: Send + Sync + 'static> Stream for AsyncParallelStream<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this: &mut Self = Pin::into_inner(self);
        let mut front = match this.tasks.pop_front() {
            None => {
                assert!(this.waiting.is_empty());
                return Poll::Ready(None);
            }
            Some(front) => front,
        };
        let front_pinned = Pin::new(&mut front);
        match front_pinned.poll(cx) {
            Poll::Ready(item) => {
                // This future has resolved. We should drop it and move futures
                // from `waiting` to `tasks`.
                while !this.waiting.is_empty() && this.tasks.len() < this.parallelism {
                    // unwrap: we know this will succeed because of the
                    // !is_empty() call directly before it.
                    let next = this.waiting.pop_front().unwrap();
                    let next_handle = async_std::task::spawn(next);
                    this.tasks.push_back(next_handle);
                }
                Poll::Ready(Some(item))
            }
            Poll::Pending => {
                // Not ready, so put it back for now.
                this.tasks.push_front(front);
                Poll::Pending
            }
        }
    }
}
