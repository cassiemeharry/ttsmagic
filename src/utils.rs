use async_std::{
    pin::Pin,
    prelude::*,
    task::{Context, Poll},
};
use futures::{future::BoxFuture, sink::Sink};

// pub struct FuturesIterStream<I, O> {
//     current_future: Option<Pin<Box<dyn Future<Output = O>>>>,
//     iterator: I,
//     output: std::marker::PhantomData<O>,
// }

// pub fn futures_iter_to_stream<I, O>(iterator: I) -> FuturesIterStream<I::IntoIter, O>
// where
//     I: IntoIterator<Item = Box<dyn Future<Output = O>>>,
// {
//     FuturesIterStream {
//         current_future: None,
//         iterator: iterator.into_iter(),
//         output: std::marker::PhantomData,
//     }
// }

// impl<I, O> Stream for FuturesIterStream<I, O>
// where
//     I: Iterator<Item = Box<dyn Future<Output = O>>>,
// {
//     type Item = O;

//     fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<O>> {
//         // Safety: we must not replace _self with something else.
//         let self_: &mut Self = unsafe { self.get_unchecked_mut() };
//         if self_.current_future.is_none() {
//             self_.current_future = self_.iterator.next().map(|f| {
//                 // Safety: we must not move the contents of `current_future`
//                 // out of the Option without immediately dropping it.
//                 unsafe { Pin::new_unchecked(f) }
//             });
//         }
//         match self_.current_future.as_mut() {
//             // self.iterator.next() returned None, so this stream is over with.
//             None => Poll::Ready(None),
//             Some(pinned_boxed_future) => {
//                 let pinned_mut_future: Pin<&mut _> = Pin::as_mut(pinned_boxed_future);
//                 match pinned_mut_future.poll(cx) {
//                     Poll::Pending => Poll::Pending,
//                     Poll::Ready(value) => {
//                         self_.current_future = None;
//                         Poll::Ready(Some(value))
//                     }
//                 }
//             }
//         }
//     }
// }

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
