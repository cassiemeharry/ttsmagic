use async_std::{
    pin::Pin,
    prelude::*,
    task::{Context, Poll},
};
use futures::{future::BoxFuture, sink::Sink};

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

thread_local! {
    static TOKIO_RUNTIME: std::cell::RefCell<tokio::runtime::Runtime> = {
        let runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
        std::cell::RefCell::new(runtime)
    };
}

/// Adapts a Tokio-based future in a dedicated thread under the Tokio runtime.
/// This is rather inefficent for short futures, but might be ok for I/O
/// operations?
pub async fn adapt_tokio_future<F>(future: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    TOKIO_RUNTIME.with(|runtime_cell| {
        let mut runtime = runtime_cell
            .try_borrow_mut()
            .expect("Tokio runtime is already in use!");
        debug!("Running Tokio future");
        runtime.block_on(future)
    })
}
