use std::{
    any::{Any, TypeId},
    collections::HashMap,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::Arc,
};

use futures::{FutureExt, future::select_all};
use tokio::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::task::PodTask;

/// Store the state can shared by different event handlers.
#[derive(Default)]
pub struct State {
    /// pods info may is the most widely used state
    pods_info: RwLock<Vec<PodTask>>,
    /// other states.
    states: Mutex<HashMap<TypeId, Box<dyn Any + Send>>>,
}

type Handler = Arc<
    dyn Fn(Arc<State>, Box<dyn Any + Send>) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;
/// A function generate a future for a event to wait in sync loop.
type Listener = Box<dyn Fn() -> Pin<Box<dyn Future<Output = Box<dyn Any + Send>> + Send>> + Send>;
/// Generated future of a event, listening in the sync loop.
type ListeningFuture = Pin<Box<dyn Future<Output = (TypeId, Box<dyn Any + Send>)> + Send>>;

/// This struct implemented the main sync loop.
/// We use trait object to ensure the conveninece in adding new events.
///
/// # Note
/// This event loop will continuously listen for all events.
/// In other words, after the Future of an event is completed, the Future corresponding to that event will be generated again and awaited once more.
///
/// # Example
/// ```ignore
/// let sync_loop = SyncLoop::new().register_event(handler);
/// sync_loop.run().await;
/// ```
pub struct SyncLoop {
    state: Arc<State>,
    event_handlers: HashMap<TypeId, Handler>,
    event_listeners: HashMap<TypeId, Listener>,
    event_listen_list: Vec<ListeningFuture>,
}

impl Default for SyncLoop {
    fn default() -> Self {
        SyncLoop {
            state: Arc::new(State::default()),
            event_handlers: HashMap::new(),
            event_listeners: HashMap::new(),
            event_listen_list: Vec::new(),
        }
    }
}

pub trait Event<D> {
    fn listen() -> Pin<Box<dyn Future<Output = D> + Send>>;
}

impl SyncLoop {
    /// Register a event handler.
    ///
    /// - The `handler` should have the following function signature:
    ///   `async fn nothing_handler(_state: Arc<State>, _data: Box<()>, _: WithEvent<SomeEvent>);`
    ///   , where `WithEvent<SomeEvent>` is used to mark the event corresponding to the handler.
    /// - The corresponding event must implement `Event<T>`,
    ///   where `T` is the type of data to be passed to the Event Handler when the event occurs.
    ///
    /// # Example
    /// ```ignore
    /// struct Tick;
    ///
    /// impl Event<()> for Tick {
    ///     fn listen() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    ///         async {
    ///             sleep(Duration::from_secs(1)).await;
    ///         }
    ///         .boxed()
    ///     }
    /// }
    ///
    /// async fn tick_handler(_state: std::sync::Arc<State>, _data: Box<()>, _: WithEvent<Tick>) {
    ///     println!("Hello World!");
    /// }
    /// ```
    ///
    /// The above implementation creates an event that prints "Hello World" every second.
    pub fn register_event<T, D, F, O>(mut self, handler: F) -> Self
    where
        T: Event<D> + 'static,
        D: Send + 'static,
        O: Future<Output = ()> + Send,
        F: (Fn(Arc<State>, Box<D>, WithEvent<T>) -> O) + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        let handler = Arc::new(move |state, data: Box<dyn Any + Send>| {
            let handle = handler.clone();
            let data = data.downcast::<D>().expect("failed to downcast event data");
            async move {
                handle(
                    state,
                    data,
                    WithEvent {
                        _marker: PhantomData,
                    },
                )
                .await;
            }
            .boxed()
        });
        self.event_handlers.insert(TypeId::of::<T>(), handler);
        let listener = Box::new(move || {
            async move { Box::new(T::listen().await) as Box<dyn Any + Send> }.boxed()
        });
        self.event_listeners.insert(TypeId::of::<T>(), listener);
        self
    }

    /// Get Future from each event.
    fn gen_event_list(&mut self) {
        self.event_listen_list = self
            .event_listeners
            .iter()
            .map(|(id, f)| {
                let id = *id;
                let fut = f();
                async move {
                    let res = fut.await;
                    (id, res)
                }
                .boxed()
            })
            .collect();
    }

    /// Run the main sync loop.
    pub async fn run(mut self) {
        self.gen_event_list();

        loop {
            let (data, _, remain) = select_all(self.event_listen_list).await;
            let (id, data) = data;
            let state = self.state.clone();

            let handler = self.event_handlers.get(&id).unwrap().clone();
            tokio::spawn(async move {
                let handle = handler.clone();
                handle(state, data).await
            });

            self.event_listen_list = remain;
            let listener = self.event_listeners.get(&id).unwrap();
            let fut = listener();
            self.event_listen_list.push(
                async move {
                    let fut = fut.await;
                    (id, fut)
                }
                .boxed(),
            );
        }
    }
}

#[allow(dead_code)]
impl State {
    /// Insert a data to State. If a value with same data is already exist, the new one will replace the old.
    pub async fn insert<T: Send + 'static>(&self, data: T) {
        let mut state_guard = self.states.lock().await;
        (*state_guard).insert(TypeId::of::<T>(), Box::new(data));
    }

    /// Get a [`StateGuard`] point to the original data if it was in state.
    pub async fn get<T: 'static>(&self) -> Option<StateGuard<'_, T>> {
        let mutex_guard = self.states.lock().await;
        let id = TypeId::of::<T>();
        if (*mutex_guard).contains_key(&id) {
            Some(StateGuard::new(mutex_guard, id))
        } else {
            None
        }
    }

    /// Get a ReadGuard of pods information.
    pub async fn pods(&self) -> RwLockReadGuard<Vec<PodTask>> {
        self.pods_info.read().await
    }

    /// Get a WriteGuard of pods information.
    pub async fn pods_mut(&self) -> RwLockWriteGuard<Vec<PodTask>> {
        self.pods_info.write().await
    }
}

/// This type wraps the underlying MutexGuard, allowing access to and modification of shared state data by derefering.
pub struct StateGuard<'a, T> {
    guard: MutexGuard<'a, HashMap<TypeId, Box<dyn Any + Send>>>,
    id: TypeId,
    _marker: PhantomData<T>,
}

impl<T: 'static> Deref for StateGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        (*self.guard)
            .get(&self.id)
            // We have checked the key exists when get the StateGuard, so it is fine to unwrap directly.
            .unwrap()
            .downcast_ref::<T>()
            .expect("downcast failed")
    }
}

impl<T: 'static> DerefMut for StateGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        (*self.guard)
            .get_mut(&self.id)
            // We have checked the key exists when get the StateGuard, so it is fine to unwrap directly.
            .unwrap()
            .downcast_mut()
            .expect("downcast failed")
    }
}

impl<'a, T> StateGuard<'a, T> {
    fn new(guard: MutexGuard<'a, HashMap<TypeId, Box<dyn Any + Send>>>, id: TypeId) -> Self {
        StateGuard {
            guard,
            id,
            _marker: PhantomData,
        }
    }
}

pub struct WithEvent<T> {
    _marker: PhantomData<T>,
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::FutureExt;
    use std::time::Duration;
    use tokio::time::sleep;

    struct Tick;
    impl Event<()> for Tick {
        fn listen() -> Pin<Box<dyn Future<Output = ()> + Send>> {
            async {
                sleep(Duration::from_secs(1)).await;
            }
            .boxed()
        }
    }

    async fn tick_handler(_state: std::sync::Arc<State>, _data: Box<()>, _: WithEvent<Tick>) {
        println!("Hello World!");
    }

    struct Nothing;
    impl Event<()> for Nothing {
        fn listen() -> Pin<Box<dyn Future<Output = ()> + Send>> {
            async {}.boxed()
        }
    }

    async fn nothing_handler(_state: Arc<State>, _data: Box<()>, _: WithEvent<Nothing>) {}

    #[tokio::test]
    async fn test_state_modify() {
        let state = State::default();
        state.insert(233).await;
        let mut p = state.get::<i32>().await.unwrap();
        *p = 114;
        drop(p);
        let p = state.get::<i32>().await.unwrap();
        assert_eq!(*p, 114);
        drop(p);
        state.insert(514).await;
        let p = state.get::<i32>().await.unwrap();
        assert_eq!(*p, 514);
        drop(p);
        let p = state.get::<u64>().await;
        matches!(p, None);
    }

    #[tokio::test]
    async fn test_register_event() {
        let mut sync_loop = SyncLoop::default()
            .register_event(tick_handler)
            .register_event(nothing_handler);
        assert_eq!(sync_loop.event_handlers.len(), 2);
        assert_eq!(sync_loop.event_listen_list.len(), 0);
        assert_eq!(sync_loop.event_listeners.len(), 2);
        matches!(sync_loop.event_handlers.get(&TypeId::of::<Tick>()), Some(_));
        matches!(
            sync_loop.event_handlers.get(&TypeId::of::<Nothing>()),
            Some(_)
        );
        matches!(
            sync_loop.event_listeners.get(&TypeId::of::<Tick>()),
            Some(_)
        );
        matches!(
            sync_loop.event_listeners.get(&TypeId::of::<Nothing>()),
            Some(_)
        );
        sync_loop.gen_event_list();
        assert_eq!(sync_loop.event_listen_list.len(), 2);
        let mut ids = Vec::new();
        while sync_loop.event_listen_list.len() != 0 {
            let (data, _, remain) = select_all(sync_loop.event_listen_list).await;
            ids.push(data.0);
            sync_loop.event_listen_list = remain;
        }
        assert_eq!(ids, vec![TypeId::of::<Nothing>(), TypeId::of::<Tick>()]);
    }
}
