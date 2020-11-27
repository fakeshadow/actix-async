use core::future::Future;
use core::pin::Pin;
use core::task::{Context as StdContext, Poll};
use core::time::Duration;

use pin_project_lite::pin_project;

use crate::actor::Actor;
use crate::error::ActixAsyncError;
use crate::message::ActorMessage;
use crate::runtime::RuntimeService;
use crate::util::channel::{OneshotReceiver, SendFuture};
use crate::util::futures::LocalBoxedFuture;

/// default timeout for sending message
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Message request to actor with timeout setting.
pub type MessageRequest<'a, A, R> =
    _MessageRequest<<A as Actor>::Runtime, SendFuture<'a, ActorMessage<A>>, R>;

/// Box version of MessageRequest that bound to `Message::Result` type.
pub type BoxedMessageRequest<'a, RT, R> =
    _MessageRequest<RT, LocalBoxedFuture<'a, Result<(), ActixAsyncError>>, R>;

pin_project! {
    #[project = MessageRequestProj]
    pub enum _MessageRequest<RT, Fut, R>
    where
        RT: RuntimeService,
    {
        Request {
            #[pin]
            fut: Fut,
            rx: Option<OneshotReceiver<R>>,
            timeout: RT::Sleep,
            timeout_response: Option<RT::Sleep>
        },
        Response {
            rx: OneshotReceiver<R>,
            timeout_response: Option<RT::Sleep>
        }
    }
}

const TIMEOUT_CONFIGURABLE: &str = "Timeout is not configurable after Request Future is polled";

impl<RT, Fut, R> _MessageRequest<RT, Fut, R>
where
    RT: RuntimeService,
{
    pub(crate) fn new(fut: Fut, rx: OneshotReceiver<R>) -> Self {
        _MessageRequest::Request {
            fut,
            rx: Some(rx),
            timeout: RT::sleep(DEFAULT_TIMEOUT),
            timeout_response: None,
        }
    }

    /// set the timeout duration for request.
    ///
    /// Default to 10 seconds.
    pub fn timeout(self, dur: Duration) -> Self {
        match self {
            _MessageRequest::Request {
                fut,
                rx,
                timeout_response,
                ..
            } => _MessageRequest::Request {
                fut,
                rx,
                timeout: RT::sleep(dur),
                timeout_response,
            },
            _ => unreachable!(TIMEOUT_CONFIGURABLE),
        }
    }

    /// set the timeout duration for response.(start from the message arrives at actor)
    ///
    /// Default to no timeout.
    pub fn timeout_response(self, dur: Duration) -> Self {
        match self {
            _MessageRequest::Request {
                fut, rx, timeout, ..
            } => _MessageRequest::Request {
                fut,
                rx,
                timeout,
                timeout_response: Some(RT::sleep(dur)),
            },
            _ => unreachable!(TIMEOUT_CONFIGURABLE),
        }
    }
}

impl<RT, Fut, R> Future for _MessageRequest<RT, Fut, R>
where
    RT: RuntimeService,
    Fut: Future<Output = Result<(), ActixAsyncError>>,
{
    type Output = Result<R, ActixAsyncError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut StdContext<'_>) -> Poll<Self::Output> {
        match self.as_mut().project() {
            MessageRequestProj::Request {
                fut,
                rx,
                timeout,
                timeout_response,
            } => match fut.poll(cx) {
                Poll::Ready(Ok(())) => {
                    let rx = rx.take().unwrap();
                    let timeout_response = timeout_response.take();

                    self.set(_MessageRequest::Response {
                        rx,
                        timeout_response,
                    });
                    self.poll(cx)
                }
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => match Pin::new(timeout).poll(cx) {
                    Poll::Ready(_) => Poll::Ready(Err(ActixAsyncError::SendTimeout)),
                    Poll::Pending => Poll::Pending,
                },
            },
            MessageRequestProj::Response {
                rx,
                timeout_response,
            } => match Pin::new(rx).poll(cx) {
                Poll::Ready(res) => Poll::Ready(Ok(res?)),
                Poll::Pending => {
                    if let Some(ref mut timeout) = timeout_response {
                        if Pin::new(timeout).poll(cx).is_ready() {
                            return Poll::Ready(Err(ActixAsyncError::ReceiveTimeout));
                        }
                    }
                    Poll::Pending
                }
            },
        }
    }
}
