use async_std::{
    pin::Pin,
    prelude::*,
    task::{Context, Poll},
};

pub struct FuturesIterStream<I, O> {
    current_future: Option<Pin<Box<dyn Future<Output = O>>>>,
    iterator: I,
    output: std::marker::PhantomData<O>,
}

pub fn futures_iter_to_stream<I, O>(iterator: I) -> FuturesIterStream<I::IntoIter, O>
where
    I: IntoIterator<Item = Box<dyn Future<Output = O>>>,
{
    FuturesIterStream {
        current_future: None,
        iterator: iterator.into_iter(),
        output: std::marker::PhantomData,
    }
}

impl<I, O> Stream for FuturesIterStream<I, O>
where
    I: Iterator<Item = Box<dyn Future<Output = O>>>,
{
    type Item = O;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<O>> {
        // Safety: we must not replace _self with something else.
        let self_: &mut Self = unsafe { self.get_unchecked_mut() };
        if self_.current_future.is_none() {
            self_.current_future = self_.iterator.next().map(|f| {
                // Safety: we must not move the contents of `current_future`
                // out of the Option without immediately dropping it.
                unsafe { Pin::new_unchecked(f) }
            });
        }
        match self_.current_future.as_mut() {
            // self.iterator.next() returned None, so this stream is over with.
            None => Poll::Ready(None),
            Some(pinned_boxed_future) => {
                let pinned_mut_future: Pin<&mut _> = Pin::as_mut(pinned_boxed_future);
                match pinned_mut_future.poll(cx) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(value) => {
                        self_.current_future = None;
                        Poll::Ready(Some(value))
                    }
                }
            }
        }
    }
}

// impl<'a, I, O> Stream for &'a mut FuturesIterStream<I, O>
// where
//     I: Iterator<Item = Box<dyn Future<Output = O>>>,
// {
//     type Item = O;

//     fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<O>> {}
// }
