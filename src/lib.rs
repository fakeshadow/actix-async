//! actix API(mostly) with async/await friendly.
//!
//! # Example:
//! ```rust
//! use actix_async::prelude::*;
//!
//! // actor type
//! struct TestActor;
//! // impl actor trait for actor type
//! actor!(TestActor);
//!
//! // message type
//! struct TestMessage;
//! // impl message trait for message type and define the result type.
//! message!(TestMessage, u32);
//!
//! // impl handler trait for message and actor types.
//! #[async_trait::async_trait(?Send)]
//! impl Handler<TestMessage> for TestActor {
//!     // concurrent message handler where actor state and context are borrowed immutably.
//!     async fn handle(&self, _: TestMessage, _: Context<'_, Self>) -> u32 {
//!         996
//!     }
//!     
//!     // exclusive message handler where actor state and context are borrowed mutably.
//!     async fn handle_wait(&mut self, _: TestMessage, _: Context<'_, Self>) -> u32 {
//!         251
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     tokio::task::LocalSet::new().run_until(async {
//!         // construct actor
//!         let actor = TestActor;
//!
//!         // start actor and get address
//!         let address = actor.start();
//!
//!         // send concurrent message with address
//!         let res = address.send(TestMessage).await.unwrap();
//!
//!         // got result
//!         assert_eq!(996, res);
//!
//!         // send exclusive message with address
//!         let res = address.wait(TestMessage).await.unwrap();
//!
//!         // got result
//!         assert_eq!(251, res);
//!     })
//!     .await
//! }
//! ```

#![forbid(unused_imports, unused_mut, unused_variables)]

#[cfg(not(feature = "std"))]
extern crate alloc;

mod actor;
mod context_future;
mod handler;
mod macros;
mod message;
mod util;
mod waker;

pub mod address;
pub mod context;
pub mod error;
pub mod prelude {
    pub use crate::actor::Actor;
    pub use crate::context::Context;
    pub use crate::context::ContextJoinHandle;
    pub use crate::error::ActixAsyncError;
    pub use crate::handler::Handler;
    pub use crate::message::Message;
    pub use crate::runtime::RuntimeService;
    pub use crate::util::futures::LocalBoxFuture;

    // message macro
    pub use crate::message;

    #[cfg(feature = "tokio-rt")]
    // tokio actor macro
    pub use crate::actor;

    #[cfg(feature = "tokio-rt")]
    pub use self::default_tokio_rt::TokioRuntime;

    #[cfg(feature = "tokio-rt")]
    mod default_tokio_rt {
        use super::RuntimeService;

        use core::{future::Future, time::Duration};

        pub struct TokioRuntime;

        impl RuntimeService for TokioRuntime {
            type Sleep = tokio::time::Sleep;

            fn spawn<F: Future<Output = ()> + 'static>(f: F) {
                tokio::task::spawn_local(f);
            }

            fn sleep(dur: Duration) -> Self::Sleep {
                tokio::time::sleep(dur)
            }
        }
    }
}
pub mod request;
pub mod runtime;

#[cfg(doctest)]
doc_comment::doctest!("../README.md");

#[cfg(test)]
mod test {
    use core::{
        cell::Cell,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context as StdContext, Poll},
        time::Duration,
    };

    #[cfg(not(feature = "std"))]
    use alloc::{boxed::Box, rc::Rc, sync::Arc};

    #[cfg(feature = "std")]
    use std::{rc::Rc, sync::Arc};

    use async_trait::async_trait;
    use futures_util::StreamExt;
    use tokio::{
        task::LocalSet,
        time::{interval, sleep, Instant, Interval},
    };

    use crate as actix_async;
    use actix_async::prelude::*;

    #[tokio::test]
    async fn stop_graceful() {
        LocalSet::new()
            .run_until(async {
                let addr = TestActor::default().start();

                let res = addr.stop(true).await;
                assert!(res.is_ok());
                assert!(addr.send(TestMsg).await.is_err());
            })
            .await
    }

    #[tokio::test]
    async fn run_future() {
        LocalSet::new()
            .run_until(async {
                let addr = TestActor::default().start();

                let res = addr.run(|_act, _ctx| Box::pin(async move { 123 })).await;
                assert_eq!(123, res.unwrap());

                let res = addr
                    .run_wait(|_act, _ctx| Box::pin(async move { 321 }))
                    .await;
                assert_eq!(321, res.unwrap());
            })
            .await
    }

    #[tokio::test]
    async fn run_interval() {
        LocalSet::new()
            .run_until(async {
                let addr = TestActor::default().start();

                let (size, handle) = addr.send(TestIntervalMessage).await.unwrap();
                sleep(Duration::from_millis(1250)).await;
                handle.cancel();
                assert_eq!(size.load(Ordering::SeqCst), 2);

                let (size, handle) = addr.wait(TestIntervalMessage).await.unwrap();
                sleep(Duration::from_millis(1250)).await;
                handle.cancel();
                assert_eq!(size.load(Ordering::SeqCst), 2)
            })
            .await
    }

    #[tokio::test]
    async fn test_timeout() {
        LocalSet::new()
            .run_until(async {
                let addr = TestActor::default().start();

                let res = addr
                    .send(TestTimeoutMessage)
                    .timeout(Duration::from_secs(1))
                    .timeout_response(Duration::from_secs(1))
                    .await;

                assert_eq!(res, Err(ActixAsyncError::ReceiveTimeout));
            })
            .await
    }

    #[tokio::test]
    async fn test_recipient() {
        LocalSet::new()
            .run_until(async {
                let addr = TestActor::default().start();

                let re = addr.recipient::<TestMsg>();

                let res = re.send(TestMsg).await;
                assert_eq!(996, res.unwrap());

                let res = re.wait(TestMsg).await;
                assert_eq!(251, res.unwrap());

                drop(re);

                let re = addr.recipient_weak::<TestMsg>();

                let res = re.send(TestMsg).await;
                assert_eq!(996, res.unwrap());

                let res = re.wait(TestMsg).await;
                assert_eq!(251, res.unwrap());

                drop(addr);

                let res = re.send(TestMsg).await;

                assert_eq!(res, Err(ActixAsyncError::Closed));
            })
            .await
    }

    #[tokio::test]
    async fn test_delay() {
        LocalSet::new()
            .run_until(async {
                let addr = TestActor::default().start();

                let res = addr.send(TestDelayMessage).await.unwrap();
                drop(res);

                sleep(Duration::from_millis(300)).await;
                let res = addr.send(TestMsg).await.unwrap();
                assert_eq!(996, res);

                sleep(Duration::from_millis(300)).await;
                let res = addr.send(TestMsg).await.unwrap();
                assert_eq!(997, res);

                let res = addr.send(TestDelayMessage).await.unwrap();

                sleep(Duration::from_millis(400)).await;
                res.cancel();

                sleep(Duration::from_millis(300)).await;
                let res = addr.send(TestMsg).await.unwrap();
                assert_eq!(997, res);
            })
            .await
    }

    #[tokio::test]
    async fn test_stream() {
        LocalSet::new()
            .run_until(async {
                let addr = TestActor::default().start();

                struct TestStream {
                    interval: Interval,
                    state: Arc<AtomicUsize>,
                }

                impl futures_util::stream::Stream for TestStream {
                    type Item = TestMsg;

                    fn poll_next(
                        self: Pin<&mut Self>,
                        cx: &mut StdContext<'_>,
                    ) -> Poll<Option<Self::Item>> {
                        let this = self.get_mut();
                        if this.interval.poll_tick(cx).is_pending() {
                            return Poll::Pending;
                        }
                        this.state.fetch_add(1, Ordering::SeqCst);
                        Poll::Ready(Some(TestMsg))
                    }
                }

                let state = Arc::new(AtomicUsize::new(0));

                let handle = addr
                    .run_wait({
                        let state = state.clone();
                        move |_, ctx| {
                            Box::pin(async move {
                                ctx.add_stream(TestStream {
                                    interval: interval(Duration::from_millis(500)),
                                    state,
                                })
                            })
                        }
                    })
                    .await
                    .unwrap();

                sleep(Duration::from_millis(750)).await;
                handle.cancel();
                sleep(Duration::from_millis(1000)).await;
                assert_eq!(2, state.load(Ordering::SeqCst));
            })
            .await
    }

    #[tokio::test]
    async fn test_capacity() {
        LocalSet::new()
            .run_until(async {
                let state = Rc::new(Cell::new(0));

                let addr = TestCapActor(state.clone()).start();

                let addr_clone = addr.clone();
                tokio::task::spawn_local(async move {
                    let mut futs = futures_util::stream::FuturesUnordered::new();

                    for _ in 0..4 {
                        futs.push(addr_clone.send(TestCapMsg));
                    }

                    while futs.next().await.is_some() {}
                });

                let now = Instant::now();

                tokio::task::yield_now().await;

                assert_eq!(state.get(), 4);

                addr.do_send(TestMsg);

                let res = addr.send(TestMsg).await.unwrap();

                assert_eq!(res, 5);
                assert_eq!(state.get(), 6);
                assert!(now.elapsed() > Duration::from_secs(3));
            })
            .await
    }

    //
    // #[tokio::test]
    // async fn test_panic_recovery() {
    //     let supervisor = Supervisor::new(1);
    //     let addr = supervisor.start_in_arbiter(1, |_| TestActor::default());
    //
    //     let _ = addr.send(TestPanicMsg).await;
    //     sleep(Duration::from_millis(1000)).await;
    //     let res = addr.send(TestMessage).await;
    //
    //     assert_eq!(996, res.unwrap());
    // }

    struct TestActor(usize);

    impl Default for TestActor {
        fn default() -> Self {
            Self(996)
        }
    }

    #[async_trait(?Send)]
    impl Actor for TestActor {
        type Runtime = TokioRuntime;

        async fn on_start(&mut self, _: Context<'_, Self>) {
            self.0 += 1;
            assert_eq!(997, self.0);
            self.0 -= 1;
        }
    }

    struct TestMsg;

    message!(TestMsg, usize);

    #[async_trait(?Send)]
    impl Handler<TestMsg> for TestActor {
        async fn handle(&self, _: TestMsg, _: Context<'_, Self>) -> usize {
            self.0
        }

        async fn handle_wait(&mut self, _: TestMsg, _: Context<'_, Self>) -> usize {
            251
        }
    }

    struct TestIntervalMessage;

    message!(TestIntervalMessage, (Arc<AtomicUsize>, ContextJoinHandle));

    #[async_trait(?Send)]
    impl Handler<TestIntervalMessage> for TestActor {
        async fn handle(
            &self,
            _: TestIntervalMessage,
            ctx: Context<'_, Self>,
        ) -> (Arc<AtomicUsize>, ContextJoinHandle) {
            let size = Arc::new(AtomicUsize::new(0));
            let handle = ctx.run_interval(Duration::from_millis(500), {
                let size = size.clone();
                move |_, _| {
                    Box::pin(async move {
                        size.fetch_add(1, Ordering::SeqCst);
                    })
                }
            });

            (size, handle)
        }

        async fn handle_wait(
            &mut self,
            _: TestIntervalMessage,
            ctx: Context<'_, Self>,
        ) -> (Arc<AtomicUsize>, ContextJoinHandle) {
            let size = Arc::new(AtomicUsize::new(0));
            let handle = ctx.run_wait_interval(Duration::from_millis(500), {
                let size = size.clone();
                move |_, _| {
                    Box::pin(async move {
                        size.fetch_add(1, Ordering::SeqCst);
                    })
                }
            });

            (size, handle)
        }
    }

    struct TestTimeoutMessage;

    message!(TestTimeoutMessage, ());

    #[async_trait(?Send)]
    impl Handler<TestTimeoutMessage> for TestActor {
        async fn handle(&self, _: TestTimeoutMessage, _: Context<'_, Self>) {
            sleep(Duration::from_secs(2)).await;
        }
    }

    struct TestDelayMessage;

    message!(TestDelayMessage, ContextJoinHandle);

    #[async_trait(?Send)]
    impl Handler<TestDelayMessage> for TestActor {
        async fn handle(&self, _: TestDelayMessage, ctx: Context<'_, Self>) -> ContextJoinHandle {
            ctx.run_wait_later(Duration::from_millis(500), |act, _| {
                Box::pin(async move {
                    act.0 += 1;
                })
            })
        }
    }

    struct TestPanicMsg;

    message!(TestPanicMsg, ());

    #[async_trait(?Send)]
    impl Handler<TestPanicMsg> for TestActor {
        async fn handle(&self, _: TestPanicMsg, _: Context<'_, Self>) {
            panic!("This is a purpose panic to test actor recovery");
        }
    }

    struct TestCapActor(Rc<Cell<usize>>);

    impl Actor for TestCapActor {
        type Runtime = TokioRuntime;

        fn size_hint() -> usize {
            4
        }
    }

    struct TestCapMsg;
    message!(TestCapMsg, ());

    #[async_trait(?Send)]
    impl Handler<TestMsg> for TestCapActor {
        async fn handle(&self, _: TestMsg, _: Context<'_, Self>) -> usize {
            let current = self.0.get();
            self.0.set(current + 1);
            current
        }
    }

    #[async_trait(?Send)]
    impl Handler<TestCapMsg> for TestCapActor {
        async fn handle(&self, _: TestCapMsg, _: Context<'_, Self>) {
            self.0.set(self.0.get() + 1);
            sleep(Duration::from_secs(3)).await
        }
    }
}
