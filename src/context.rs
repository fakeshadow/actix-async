use core::{
    cell::{Cell, RefCell},
    time::Duration,
};

use alloc::{boxed::Box, vec::Vec};

use super::actor::{Actor, ActorState};
use super::address::Addr;
use super::handler::Handler;
use super::message::{
    ActorMessage, ActorMessageClone, FunctionMessage, FunctionMutMessage, FutureMessage,
    IntervalMessage, Message, StreamContainer, StreamMessage,
};
use super::util::{
    channel::{oneshot, OneshotReceiver, OneshotSender, Receiver},
    futures::{LocalBoxFuture, Stream},
};

/// Context type of `Actor`. Can be accessed within `Handler::handle` and
/// `Handler::handle_wait` method.
///
/// Used to mutate the state of actor and add additional tasks to actor.
pub struct Context<'a, A: Actor> {
    state: &'a Cell<ActorState>,
    future_cache: &'a RefCell<Vec<FutureMessage<A>>>,
    stream_cache: &'a RefCell<Vec<StreamMessage<A>>>,
    rx: &'a Receiver<ActorMessage<A>>,
}

/// a join handle can be used to cancel a spawned async task like interval closure and stream
/// handler
pub struct ContextJoinHandle {
    handle: OneshotSender<()>,
}

impl ContextJoinHandle {
    /// Cancel the task associate to this handle.
    pub fn cancel(self) {
        let _ = self.handle.send(());
    }

    /// Check if the task associate with this handle is terminated.
    ///
    /// This happens when the task is finished or the thread task runs on is recovered from a
    /// panic.
    pub fn is_terminated(&self) -> bool {
        self.handle.is_closed()
    }
}

impl<'c, A: Actor> Context<'c, A> {
    pub(crate) fn new(
        state: &'c Cell<ActorState>,
        future_cache: &'c RefCell<Vec<FutureMessage<A>>>,
        stream_cache: &'c RefCell<Vec<StreamMessage<A>>>,
        rx: &'c Receiver<ActorMessage<A>>,
    ) -> Self {
        Context {
            state,
            future_cache,
            stream_cache,
            rx,
        }
    }

    /// run interval concurrent closure on context. `Handler::handle` will be called.
    pub fn run_interval<F>(&self, dur: Duration, f: F) -> ContextJoinHandle
    where
        F: for<'a> FnOnce(&'a A, Context<'a, A>) -> LocalBoxFuture<'a, ()> + Clone + 'static,
    {
        self.interval(|rx| {
            let msg = FunctionMessage::new(f);
            IntervalMessage::new(dur, rx, ActorMessageClone::Ref(Box::new(msg)))
        })
    }

    /// run interval exclusive closure on context. `Handler::handle_wait` will be called.
    /// If `Handler::handle_wait` is not override `Handler::handle` will be called as fallback.
    pub fn run_wait_interval<F>(&self, dur: Duration, f: F) -> ContextJoinHandle
    where
        F: for<'a> FnOnce(&'a mut A, Context<'a, A>) -> LocalBoxFuture<'a, ()> + Clone + 'static,
    {
        self.interval(|rx| {
            let msg = FunctionMutMessage::new(f);
            IntervalMessage::new(dur, rx, ActorMessageClone::Mut(Box::new(msg)))
        })
    }

    fn interval<F>(&self, f: F) -> ContextJoinHandle
    where
        F: FnOnce(OneshotReceiver<()>) -> IntervalMessage<A>,
    {
        let (handle, rx) = oneshot();

        let msg = f(rx);
        let msg = StreamMessage::new_interval(msg);

        self.stream_cache.borrow_mut().push(msg);

        ContextJoinHandle { handle }
    }

    /// run concurrent closure on context after given duration. `Handler::handle` will be called.
    pub fn run_later<F>(&self, dur: Duration, f: F) -> ContextJoinHandle
    where
        F: for<'a> FnOnce(&'a A, Context<'a, A>) -> LocalBoxFuture<'a, ()> + 'static,
    {
        self.later(|rx| {
            let msg = FunctionMessage::<_, ()>::new(f);
            let msg = ActorMessage::new_ref(msg, None);
            FutureMessage::new(dur, rx, msg)
        })
    }

    /// run exclusive closure on context after given duration. `Handler::handle_wait` will be
    /// called.
    /// If `Handler::handle_wait` is not override `Handler::handle` will be called as fallback.
    pub fn run_wait_later<F>(&self, dur: Duration, f: F) -> ContextJoinHandle
    where
        F: for<'a> FnOnce(&'a mut A, Context<'a, A>) -> LocalBoxFuture<'a, ()> + 'static,
    {
        self.later(|rx| {
            let msg = FunctionMutMessage::<_, ()>::new(f);
            let msg = ActorMessage::new_mut(msg, None);
            FutureMessage::new(dur, rx, msg)
        })
    }

    fn later<F>(&self, f: F) -> ContextJoinHandle
    where
        F: FnOnce(OneshotReceiver<()>) -> FutureMessage<A>,
    {
        let (handle, rx) = oneshot();
        self.future_cache.borrow_mut().push(f(rx));
        ContextJoinHandle { handle }
    }

    /// stop the context. It would end the actor gracefully by close the channel draining all
    /// remaining messages.
    pub fn stop(&self) {
        self.rx.close();
        self.state.set(ActorState::StopGraceful);
    }

    /// get the address of actor from context.
    pub fn address(&self) -> Option<Addr<A>> {
        Addr::from_recv(self.rx).ok()
    }

    /// add a stream to context. multiple stream can be added to one context.
    ///
    /// stream item will be treated as concurrent message and `Handler::handle` will be called.
    /// If `Handler::handle_wait` is not override `Handler::handle` will be called as fallback.
    ///
    /// *. Stream would force closed when the actor is stopped. Either by dropping all `Addr` or
    /// calling `Addr::stop`
    ///
    /// # example:
    /// ```rust
    /// use actix_async::prelude::*;
    /// use futures_util::stream::once;
    ///
    /// struct StreamActor;
    /// actor!(StreamActor);
    ///
    /// struct StreamMessage;
    /// message!(StreamMessage, ());
    ///
    /// #[async_trait::async_trait(?Send)]
    /// impl Handler<StreamMessage> for StreamActor {
    ///     async fn handle(&self, _: StreamMessage, _: Context<'_, Self>) {
    ///     /*
    ///         The stream is owned by Context so there is no default way to return anything
    ///         from the handler.
    ///         A suggest way to return anything here is to use a channel sender or another
    ///         actor's Addr to StreamActor as it's state.
    ///     */
    ///     }
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     tokio::task::LocalSet::new().run_until(async {
    ///         let address = StreamActor::create(|ctx| {
    ///             ctx.add_stream(once(async { StreamMessage }));
    ///             StreamActor
    ///         });
    ///     })
    ///     .await
    /// }
    /// ```
    pub fn add_stream<S>(&self, stream: S) -> ContextJoinHandle
    where
        S: Stream + 'static,
        S::Item: Message + 'static,
        A: Handler<S::Item>,
    {
        self.stream(stream, |item| ActorMessage::new_ref(item, None))
    }

    /// add a stream to context. multiple stream can be added to one context.
    ///
    /// stream item will be treated as exclusive message and `Handler::handle_wait` will be called.
    pub fn add_wait_stream<S>(&self, stream: S) -> ContextJoinHandle
    where
        S: Stream + 'static,
        S::Item: Message + 'static,
        A: Handler<S::Item>,
    {
        self.stream(stream, |item| ActorMessage::new_mut(item, None))
    }

    fn stream<S, F>(&self, stream: S, f: F) -> ContextJoinHandle
    where
        S: Stream + 'static,
        S::Item: Message + 'static,
        A: Handler<S::Item>,
        F: FnOnce(S::Item) -> ActorMessage<A> + Copy + 'static,
    {
        let (handle, rx) = oneshot();
        let stream = StreamContainer::new(stream, rx, f);
        let msg = StreamMessage::new_boxed(stream);
        self.stream_cache.borrow_mut().push(msg);
        ContextJoinHandle { handle }
    }
}
