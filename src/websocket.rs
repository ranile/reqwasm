//! Wrapper around `WebSocket` API
//!
//! # Example
//!
//! ```rust
//! # use reqwasm::websocket::*;
//! # use wasm_bindgen_futures::spawn_local;
//! # use futures::{SinkExt, StreamExt};
//! # macro_rules! console_log {
//! #    ($($expr:expr),*) => {{}};
//! # }
//! # fn no_run() {
//! let mut  ws = WebSocket::open("wss://echo.websocket.org").unwrap();
//!
//! spawn_local({
//!     let mut  ws = ws.clone();
//!     async move {
//!         ws.send(Message::Text(String::from("test"))).await.unwrap();
//!         ws.send(Message::Text(String::from("test 2"))).await.unwrap();
//!     }
//! });
//!
//! spawn_local(async move {
//!     while let Some(msg) = ws.next().await {
//!         console_log!(format!("1. {:?}", msg))
//!     }
//!     console_log!("WebSocket Closed")
//! })
//! # }
//! ```
use crate::js_to_js_error;
use crate::websocket::errors::WebSocketError;
use async_broadcast::Receiver;
use futures::ready;
use futures::{Sink, Stream};
use pin_project::{pin_project, pinned_drop};
use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{ErrorEvent, MessageEvent};

/// Wrapper around browser's WebSocket API.
#[allow(missing_debug_implementations)]
#[pin_project(PinnedDrop)]
#[derive(Clone)]
pub struct WebSocket {
    ws: web_sys::WebSocket,
    sink_wakers: Rc<RefCell<Vec<Waker>>>,
    #[pin]
    message_receiver: Receiver<StreamMessage>,
}

/// Message sent to and received from WebSocket.
#[derive(Debug, PartialEq, Clone)]
pub enum Message {
    /// String message
    Text(String),
    /// ArrayBuffer parsed into bytes
    Bytes(Vec<u8>),
}

/// The state of the websocket.
///
/// See [`WebSocket.readyState` on MDN](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/readyState)
/// to learn more.
#[derive(Copy, Clone, Debug)]
pub enum State {
    /// The connection has not yet been established.
    Connecting,
    /// The WebSocket connection is established and communication is possible.
    Open,
    /// The connection is going through the closing handshake, or the close() method has been
    /// invoked.
    Closing,
    /// The connection has been closed or could not be opened.
    Closed,
}

impl WebSocket {
    /// Establish a WebSocket connection.
    pub fn open(url: &str) -> Result<Self, errors::WebSocketError> {
        let wakers: Rc<RefCell<Vec<Waker>>> = Rc::new(RefCell::new(vec![]));
        let ws = web_sys::WebSocket::new(url)
            .map_err(|e| errors::WebSocketError::JsError(js_to_js_error(e)))?;

        let (sender, receiver) = async_broadcast::broadcast(10);

        let open_callback: Closure<dyn FnMut()> = {
            let wakers = Rc::clone(&wakers);
            Closure::wrap(Box::new(move || {
                for waker in wakers.borrow_mut().drain(..) {
                    waker.wake();
                }
            }) as Box<dyn FnMut()>)
        };

        ws.set_onopen(Some(open_callback.as_ref().unchecked_ref()));
        open_callback.forget();

        let message_callback: Closure<dyn FnMut(MessageEvent)> = {
            let sender = sender.clone();
            Closure::wrap(Box::new(move |e: MessageEvent| {
                let sender = sender.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = sender
                        .broadcast(StreamMessage::Message(parse_message(e)))
                        .await;
                })
            }) as Box<dyn FnMut(MessageEvent)>)
        };

        ws.set_onmessage(Some(message_callback.as_ref().unchecked_ref()));
        message_callback.forget();

        let error_callback: Closure<dyn FnMut(ErrorEvent)> = {
            let sender = sender.clone();
            Closure::wrap(Box::new(move |e: ErrorEvent| {
                let sender = sender.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = sender
                        .broadcast(StreamMessage::ConnectionError(parse_error(e)))
                        .await;
                })
            }) as Box<dyn FnMut(ErrorEvent)>)
        };

        ws.set_onerror(Some(error_callback.as_ref().unchecked_ref()));
        error_callback.forget();

        let close_callback: Closure<dyn FnMut()> = {
            Closure::wrap(Box::new(move || {
                let sender = sender.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = sender.broadcast(StreamMessage::ConnectionClose).await;
                })
            }) as Box<dyn FnMut()>)
        };

        ws.set_onerror(Some(close_callback.as_ref().unchecked_ref()));
        close_callback.forget();

        Ok(Self {
            ws,
            sink_wakers: wakers,
            message_receiver: receiver,
        })
    }

    /// Closes the websocket.
    ///
    /// See the [MDN Documentation](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/close#parameters)
    /// to learn about parameters passed to this function and when it can return an `Err(_)`
    pub fn close(self, code: Option<u16>, reason: Option<&str>) -> Result<(), WebSocketError> {
        let result = match (code, reason) {
            (None, None) => self.ws.close(),
            (Some(code), None) => self.ws.close_with_code(code),
            (Some(code), Some(reason)) => self.ws.close_with_code_and_reason(code, reason),
            // default code is 1005 so we use it,
            // see: https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/close#parameters
            (None, Some(reason)) => self.ws.close_with_code_and_reason(1005, reason),
        };
        result.map_err(|e| WebSocketError::JsError(js_to_js_error(e)))
    }

    /// The current state of the websocket.
    pub fn state(&self) -> State {
        let ready_state = self.ws.ready_state();
        match ready_state {
            0 => State::Connecting,
            1 => State::Open,
            2 => State::Closing,
            3 => State::Closed,
            _ => unreachable!(),
        }
    }

    /// The extensions in use.
    pub fn extensions(&self) -> String {
        self.ws.extensions()
    }

    /// The sub-protocol in use.
    pub fn protocol(&self) -> String {
        self.ws.protocol()
    }
}

#[derive(Clone)]
enum StreamMessage {
    ConnectionError(errors::ConnectionError),
    Message(Message),
    ConnectionClose,
}

fn parse_message(event: MessageEvent) -> Message {
    if let Ok(array_buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
        let array = js_sys::Uint8Array::new(&array_buffer);
        Message::Bytes(array.to_vec())
    } else if let Ok(txt) = event.data().dyn_into::<js_sys::JsString>() {
        Message::Text(String::from(&txt))
    } else {
        unreachable!("message event, received Unknown: {:?}", event.data());
    }
}

fn parse_error(event: ErrorEvent) -> errors::ConnectionError {
    errors::ConnectionError {
        message: event.message(),
    }
}

impl Sink<Message> for WebSocket {
    type Error = errors::WebSocketError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let ready_state = self.ws.ready_state();
        if ready_state == 0 {
            self.sink_wakers.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        let result = match item {
            Message::Bytes(bytes) => self.ws.send_with_u8_array(&bytes),
            Message::Text(message) => self.ws.send_with_str(&message),
        };
        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(errors::WebSocketError::JsError(js_to_js_error(e))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Stream for WebSocket {
    type Item = Result<Message, errors::WebSocketError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let msg = ready!(self.project().message_receiver.poll_next(cx));
        match msg {
            Some(StreamMessage::Message(msg)) => Poll::Ready(Some(Ok(msg))),
            Some(StreamMessage::ConnectionError(err)) => {
                Poll::Ready(Some(Err(errors::WebSocketError::ConnectionError(err))))
            }
            Some(StreamMessage::ConnectionClose) => Poll::Ready(None),
            None => Poll::Ready(None),
        }
    }
}

#[pinned_drop]
impl PinnedDrop for WebSocket {
    fn drop(self: Pin<&mut Self>) {
        self.ws.close().unwrap();
    }
}

pub mod errors {
    //! The errors

    /// This is created from [`ErrorEvent`] received from `onerror` listener of the WebSocket.
    #[derive(Clone, Debug, thiserror::Error)]
    #[error("{message}")]
    pub struct ConnectionError {
        /// The error message.
        pub(super) message: String,
    }

    /// Error returned by `WebSocket`
    #[derive(Clone, Debug, thiserror::Error)]
    pub enum WebSocketError {
        /// Error from `onerror`
        #[error("{0}")]
        ConnectionError(
            #[source]
            #[from]
            ConnectionError,
        ),
        /// Error while sending message
        #[error("{0}")]
        JsError(crate::JsError),
    }
}
