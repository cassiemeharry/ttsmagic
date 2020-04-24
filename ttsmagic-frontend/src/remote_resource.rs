#[allow(unused)]

pub enum RemoteResource<T, E = anyhow::Error> {
    Loading,
    Loaded(T),
    Error(E),
}

impl<T, E> RemoteResource<T, E> {
    pub fn as_ref(&self) -> RemoteResource<&T, &E> {
        match self {
            Self::Loading => RemoteResource::Loading,
            Self::Loaded(ref t) => RemoteResource::Loaded(t),
            Self::Error(ref e) => RemoteResource::Error(e),
        }
    }

    pub fn as_mut(&mut self) -> RemoteResource<&mut T, &mut E> {
        match self {
            Self::Loading => RemoteResource::Loading,
            Self::Loaded(ref mut t) => RemoteResource::Loaded(t),
            Self::Error(ref mut e) => RemoteResource::Error(e),
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> RemoteResource<U, E> {
        match self {
            Self::Loading => RemoteResource::Loading,
            Self::Loaded(t) => RemoteResource::Loaded(f(t)),
            Self::Error(e) => RemoteResource::Error(e),
        }
    }
}
