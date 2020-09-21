use anyhow::Result;
use async_std::{
    pin::Pin,
    prelude::*,
    sync::Receiver,
    task::{spawn, Context, JoinHandle, Poll},
};
use futures::{future::BoxFuture, Sink};

mod async_par_stream;
pub mod sqlx;
mod surf_redirect_middleware;

pub use async_par_stream::AsyncParallelStream;
pub use surf_redirect_middleware::RedirectMiddleware as SurfRedirectMiddleware;

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

pub struct AsyncPool {
    tasks: Vec<JoinHandle<Result<()>>>,
}

impl AsyncPool {
    pub fn new<T, F>(parallelism: usize, receiver: Receiver<T>, future_fn: F) -> Self
    where
        F: Fn(Receiver<T>) -> BoxFuture<'static, Result<()>> + Send + 'static,
    {
        let mut tasks = Vec::with_capacity(parallelism);
        for _ in 0..parallelism {
            tasks.push(spawn(future_fn(receiver.clone())));
        }
        AsyncPool { tasks }
    }
}

impl Future for AsyncPool {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = Pin::into_inner(self);
        while let Some(mut task) = this.tasks.pop() {
            let task_pin = Pin::new(&mut task);
            match task_pin.poll(cx) {
                Poll::Pending => {
                    this.tasks.push(task);
                    return Poll::Pending;
                }
                Poll::Ready(Ok(())) => (),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            }
        }
        Poll::Ready(Ok(()))
    }
}
