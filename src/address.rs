use core::ops::Deref;

use alloc::boxed::Box;

use crate::actor::{Actor, ActorState};
use crate::context::Context;
use crate::error::ActixAsyncError;
use crate::handler::Handler;
use crate::message::{
    message_send_check, ActorMessage, FunctionMessage, FunctionMutMessage, Message, MessageObject,
};
use crate::request::{BoxedMessageRequest, MessageRequest, _MessageRequest};
use crate::runtime::RuntimeService;
use crate::util::channel::{oneshot, Receiver, Sender, WeakSender};
use crate::util::futures::LocalBoxedFuture;

/// The message sink of `Actor` type. `Message` and boxed async blocks are sent to Actor through it.
pub struct Addr<A>(Sender<ActorMessage<A>>);

impl<A> Clone for Addr<A> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<A> Deref for Addr<A> {
    type Target = Sender<ActorMessage<A>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<A: Actor> Addr<A> {
    /// send a concurrent message to actor. `Handler::handle` will be called for concurrent message
    /// processing.
    #[inline]
    pub fn send<M>(&self, msg: M) -> MessageRequest<A, M::Result>
    where
        M: Message + Send,
        A: Handler<M>,
    {
        self._send(msg, ActorMessage::Ref)
    }

    /// send an exclusive message to actor. `Handler::handle_wait` will be called for exclusive
    /// message processing.
    /// If `Handler::handle_wait` is not override then it would use `Handler::handle` as fallback.
    #[inline]
    pub fn wait<M>(&self, msg: M) -> MessageRequest<A, M::Result>
    where
        M: Message + Send,
        A: Handler<M>,
    {
        self._send(msg, ActorMessage::Mut)
    }

    /// send a concurrent closure to actor. `Handler::handle` will be called for concurrent message
    /// processing.
    /// closure must be `Send` bound.
    #[inline]
    pub fn run<F, R>(&self, func: F) -> MessageRequest<A, R>
    where
        F: for<'a> FnOnce(&'a A, &'a Context<A>) -> LocalBoxedFuture<'a, R> + Send + 'static,
        R: Send + 'static,
    {
        let msg = FunctionMessage::new(func);
        self._send(msg, ActorMessage::Ref)
    }

    /// send a exclusive closure to actor. `Handler::handle_wait` will be called for exclusive
    /// message processing.
    /// If `Handler::handle_wait` is not override then it would use `Handler::handle` as fallback.
    #[inline]
    pub fn run_wait<F, R>(&self, func: F) -> MessageRequest<A, R>
    where
        F: for<'a> FnOnce(&'a mut A, &'a mut Context<A>) -> LocalBoxedFuture<'a, R>
            + Send
            + 'static,
        R: Send + 'static,
    {
        let msg = FunctionMutMessage::new(func);
        self._send(msg, ActorMessage::Mut)
    }

    /// send a message to actor and ignore the result.
    pub fn do_send<M>(&self, msg: M)
    where
        M: Message + Send,
        A: Handler<M>,
    {
        self._do_send(msg, ActorMessage::Ref);
    }

    /// send a exclusive message to actor and ignore the result.
    pub fn do_wait<M>(&self, msg: M)
    where
        M: Message + Send,
        A: Handler<M>,
    {
        self._do_send(msg, ActorMessage::Mut);
    }

    /// stop actor.
    /// When graceful is true the actor would shut it's channel and drain all remaining messages
    /// and exit. When false the actor would exit as soon as the stop message is handled.
    pub fn stop(&self, graceful: bool) -> MessageRequest<A, ()> {
        let state = if graceful {
            ActorState::StopGraceful
        } else {
            ActorState::Stop
        };

        let (tx, rx) = oneshot();

        _MessageRequest::new(
            self.deref().send(ActorMessage::ActorState(state, Some(tx))),
            rx,
        )
    }

    /// weak version of Addr that can be upgraded.
    pub fn downgrade(&self) -> WeakAddr<A> {
        WeakAddr(Sender::downgrade(&self.0))
    }

    /// Recipient bound to message type and not actor.
    pub fn recipient<M>(&self) -> Recipient<A::Runtime, M>
    where
        M: Message + Send,
        A: Handler<M>,
    {
        Recipient(Box::new(self.clone()))
    }

    /// weak version of `Recipient`.
    ///
    /// *. `RecipientWeak` will stay usable as long as the actor and it's `Addr` are alive.
    /// It DOES NOT care if a strong Recipient is alive or not.
    pub fn recipient_weak<M>(&self) -> RecipientWeak<A::Runtime, M>
    where
        M: Message + Send,
        A: Handler<M>,
    {
        RecipientWeak(Box::new(self.downgrade()))
    }

    pub(crate) fn new(tx: Sender<ActorMessage<A>>) -> Self {
        Self(tx)
    }

    pub(crate) fn from_recv(rx: &Receiver<ActorMessage<A>>) -> Result<Self, ActixAsyncError> {
        match rx.as_sender() {
            Some(tx) => Ok(Addr::new(tx)),
            None => Err(ActixAsyncError::Closed),
        }
    }

    fn _send<M, F>(&self, msg: M, f: F) -> MessageRequest<A, M::Result>
    where
        M: Message + Send,
        A: Handler<M>,
        F: FnOnce(MessageObject<A>) -> ActorMessage<A> + 'static,
    {
        Self::_send_inner(msg, |obj| self.deref().send(f(obj)))
    }

    fn _send_boxed<M, F>(&self, msg: M, f: F) -> BoxedMessageRequest<A::Runtime, M::Result>
    where
        M: Message + Send,
        A: Handler<M>,
        F: FnOnce(MessageObject<A>) -> ActorMessage<A> + 'static,
    {
        Self::_send_inner(msg, |obj| Box::pin(self.deref().send(f(obj))) as _)
    }

    fn _do_send<M, F>(&self, msg: M, f: F)
    where
        M: Message + Send,
        A: Handler<M>,
        F: FnOnce(MessageObject<A>) -> ActorMessage<A> + 'static,
    {
        message_send_check::<M>();
        let this = self.clone();
        A::spawn(async move {
            let msg = MessageObject::new(msg, None);
            let _ = this.deref().send(f(msg)).await;
        });
    }

    fn _send_inner<M, F, Fut>(msg: M, f: F) -> _MessageRequest<A::Runtime, Fut, M::Result>
    where
        M: Message + Send,
        A: Handler<M>,
        F: FnOnce(MessageObject<A>) -> Fut,
    {
        message_send_check::<M>();
        let (tx, rx) = oneshot();
        let msg = MessageObject::new(msg, Some(tx));
        _MessageRequest::new(f(msg), rx)
    }
}

/// weak version `Addr`. Can upgrade to `Addr` when at least one instance of `Addr` is still in
/// scope.
pub struct WeakAddr<A>(WeakSender<ActorMessage<A>>);

impl<A> Clone for WeakAddr<A> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<A: Actor> WeakAddr<A> {
    pub fn upgrade(&self) -> Option<Addr<A>> {
        self.0.upgrade().map(Addr)
    }

    async fn send_weak(&self, msg: ActorMessage<A>) -> Result<(), ActixAsyncError> {
        self.upgrade()
            .ok_or(ActixAsyncError::Closed)?
            .deref()
            .send(msg)
            .await
    }
}

/// trait to bind a given `Addr<A>` or `WeakAddr<A>` to `Message` trait type.
pub trait AddrHandler<RT, M>
where
    RT: RuntimeService,
    M: Message + Send,
    Self: Send + Sync + 'static,
{
    fn send(&self, msg: M) -> BoxedMessageRequest<RT, M::Result>;

    fn wait(&self, msg: M) -> BoxedMessageRequest<RT, M::Result>;

    fn do_send(&self, msg: M);

    fn do_wait(&self, msg: M);
}

impl<A, M> AddrHandler<A::Runtime, M> for Addr<A>
where
    A: Actor + Handler<M>,
    M: Message + Send,
{
    fn send(&self, msg: M) -> BoxedMessageRequest<A::Runtime, M::Result> {
        self._send_boxed(msg, ActorMessage::Ref)
    }

    fn wait(&self, msg: M) -> BoxedMessageRequest<A::Runtime, M::Result> {
        self._send_boxed(msg, ActorMessage::Mut)
    }

    fn do_send(&self, msg: M) {
        Addr::do_send(self, msg);
    }

    fn do_wait(&self, msg: M) {
        Addr::do_wait(self, msg);
    }
}

impl<A, M> AddrHandler<A::Runtime, M> for WeakAddr<A>
where
    A: Actor + Handler<M>,
    M: Message + Send,
{
    fn send(&self, msg: M) -> BoxedMessageRequest<A::Runtime, M::Result> {
        Addr::_send_inner(msg, |obj| {
            Box::pin(self.send_weak(ActorMessage::Ref(obj))) as _
        })
    }

    fn wait(&self, msg: M) -> BoxedMessageRequest<A::Runtime, M::Result> {
        Addr::_send_inner(msg, |obj| {
            Box::pin(self.send_weak(ActorMessage::Mut(obj))) as _
        })
    }

    /// `AddrHandler::do_send` will panic if the `Addr` for `RecipientWeak` is gone.
    fn do_send(&self, msg: M) {
        let addr = &self.upgrade().unwrap();
        Addr::do_send(addr, msg);
    }

    /// `AddrHandler::do_wait` will panic if the `Addr` for `RecipientWeak` is gone.
    fn do_wait(&self, msg: M) {
        let addr = &self.upgrade().unwrap();
        Addr::do_wait(addr, msg);
    }
}

/// A trait object of `Addr<Actor>` that bind to given `Message` type
pub struct Recipient<RT, M: Message + Send>(Box<dyn AddrHandler<RT, M>>);

impl<RT, M: Message + Send> Deref for Recipient<RT, M> {
    type Target = dyn AddrHandler<RT, M>;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

/// A trait object of `WeakAddr<Actor>` that bind to given `Message` type
pub struct RecipientWeak<RT, M: Message + Send>(Box<dyn AddrHandler<RT, M>>);

impl<RT, M: Message + Send> Deref for RecipientWeak<RT, M> {
    type Target = dyn AddrHandler<RT, M>;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}
